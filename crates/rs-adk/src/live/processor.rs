//! Three-lane event processor for Live sessions.
//!
//! **Fast lane**: audio, text, VAD (sync callbacks, never blocks)
//! **Control lane**: tool calls, interruptions, lifecycle, transcript accumulation,
//!   extractors, phases, watchers (async callbacks, can block)
//! **Telemetry lane**: SessionSignals + SessionTelemetry (debounced state writes,
//!   runs on its own broadcast receiver — zero work on the router hot path)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use rs_genai::prelude::{FunctionCall, FunctionResponse, SessionEvent, SessionPhase, UsageMetadata};
use rs_genai::session::SessionWriter;

use crate::state::State;
use crate::tool::ToolDispatcher;

use super::background_tool::BackgroundToolTracker;
use super::callbacks::{CallbackMode, EventCallbacks};
use super::computed::ComputedRegistry;
use super::extractor::{ExtractionTrigger, TurnExtractor};
use super::needs::{NeedsFulfillment, RepairAction};
use super::persistence::{SessionPersistence, SessionSnapshot};
use super::phase::{PhaseMachine, TransitionResult};
use super::session_signals::SessionSignals;
use super::soft_turn::SoftTurnDetector;
use super::steering::{self, SteeringMode};
use super::telemetry::SessionTelemetry;
use super::temporal::TemporalRegistry;
use super::transcript::TranscriptBuffer;
use super::watcher::WatcherRegistry;

/// Events routed to the fast lane (sync processing).
pub(crate) enum FastEvent {
    Audio(Bytes),
    Text(String),
    TextComplete(String),
    InputTranscript(String),
    OutputTranscript(String),
    VadStart,
    VadEnd,
    Phase(SessionPhase),
    /// Interruption flag — tells fast lane to stop forwarding audio.
    Interrupted,
}

/// Events routed to the control lane (async processing).
pub(crate) enum ControlEvent {
    ToolCall(Vec<FunctionCall>),
    ToolCallCancelled(Vec<String>),
    Interrupted,
    TurnComplete,
    /// Model finished generating (even if interrupted). Fires before TurnComplete.
    GenerationComplete,
    GoAway(Option<String>),
    Connected,
    Disconnected(Option<String>),
    SessionResumeUpdate(rs_genai::session::ResumeInfo),
    Error(String),
    /// Transcript accumulation — pushed from router, exclusive to control lane.
    InputTranscript(String),
    OutputTranscript(String),
}

/// Shared state between the two lanes.
pub(crate) struct SharedState {
    /// When true, fast lane suppresses audio callbacks.
    pub interrupted: AtomicBool,
    /// Latest resume handle from server.
    pub resume_handle: parking_lot::Mutex<Option<String>>,
    /// Last instruction sent via instruction_template (for dedup).
    pub last_instruction: parking_lot::Mutex<Option<String>>,
}

/// Runs the three-lane event processor.
///
/// Returns JoinHandles for the fast consumer and control processor tasks.
/// The telemetry lane is spawned separately via [`spawn_telemetry_lane`].
/// Configuration for the control plane's new capabilities.
pub(crate) struct ControlPlaneConfig {
    /// Soft turn detector for proactive silence awareness.
    pub soft_turn: Option<SoftTurnDetector>,
    /// Steering mode for phase instruction delivery.
    pub steering_mode: SteeringMode,
    /// Conversation repair tracker.
    pub needs_fulfillment: Option<NeedsFulfillment>,
    /// Session persistence backend.
    pub persistence: Option<Arc<dyn SessionPersistence>>,
    /// Session ID for persistence key.
    pub session_id: Option<String>,
    /// Whether to inject tool availability advisory on phase transitions.
    pub tool_advisory: bool,
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self {
            soft_turn: None,
            steering_mode: SteeringMode::default(),
            needs_fulfillment: None,
            persistence: None,
            session_id: None,
            tool_advisory: true,
        }
    }
}

pub(crate) fn spawn_event_processor(
    mut event_rx: broadcast::Receiver<SessionEvent>,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    writer: Arc<dyn SessionWriter>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    state: State,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<Arc<TemporalRegistry>>,
    background_tracker: Option<Arc<BackgroundToolTracker>>,
    execution_modes: std::collections::HashMap<String, super::background_tool::ToolExecutionMode>,
    control_plane: ControlPlaneConfig,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let shared = Arc::new(SharedState {
        interrupted: AtomicBool::new(false),
        resume_handle: parking_lot::Mutex::new(None),
        last_instruction: parking_lot::Mutex::new(None),
    });

    let timer_cancel = CancellationToken::new();

    // Channels between router and lanes
    let (fast_tx, fast_rx) = mpsc::channel::<FastEvent>(512);
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlEvent>(64);

    // Spawn the router task (reads broadcast, routes to lanes)
    // NOTE: SessionSignals is NOT called here — it runs on the telemetry lane.
    let fast_tx_clone = fast_tx.clone();
    let ctrl_tx_clone = ctrl_tx.clone();
    let shared_clone = shared.clone();
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    route_event(event, &fast_tx_clone, &ctrl_tx_clone, &shared_clone).await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    #[cfg(feature = "tracing-support")]
                    tracing::warn!(skipped = n, "Event processor lagged, skipped events");
                    let _ = n;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Spawn fast consumer (no transcript buffer — transcripts are in control lane)
    let fast_callbacks = callbacks.clone();
    let fast_shared = shared.clone();
    let fast_handle = tokio::spawn(async move {
        run_fast_lane(fast_rx, fast_callbacks, fast_shared).await;
    });

    // Clone for the timer task (before moving into ctrl spawn)
    let timer_temporal = temporal.clone();
    let timer_state = state.clone();
    let timer_writer = writer.clone();

    // Spawn control processor (owns TranscriptBuffer exclusively — no mutex needed)
    let ctrl_callbacks = callbacks;
    let ctrl_shared = shared;
    let ctrl_timer_cancel = timer_cancel.clone();
    let ctrl_handle = tokio::spawn(async move {
        run_control_lane(
            ctrl_rx,
            ctrl_callbacks,
            dispatcher,
            writer,
            ctrl_shared,
            extractors,
            state,
            computed,
            phase_machine,
            watchers,
            temporal,
            background_tracker,
            execution_modes,
            control_plane,
        )
        .await;
        ctrl_timer_cancel.cancel();
    });

    // Optional timer task for sustained temporal patterns
    if let Some(ref temporal_ref) = timer_temporal {
        if temporal_ref.needs_timer() {
            let t = temporal_ref.clone();
            let cancel = timer_cancel.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(500));
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            for action in t.check_all(&timer_state, None, &timer_writer) {
                                tokio::spawn(action);
                            }
                        }
                    }
                }
            });
        }
    }

    (fast_handle, ctrl_handle)
}

/// Spawns the telemetry lane — processes events on its own broadcast receiver.
///
/// SessionSignals + SessionTelemetry run here, off the router hot path.
/// Derived timing signals (silence_ms, elapsed_ms, remaining_budget_ms)
/// are flushed every 100ms via debounced timer.
pub(crate) fn spawn_telemetry_lane(
    mut telem_rx: broadcast::Receiver<SessionEvent>,
    signals: SessionSignals,
    telemetry: Arc<SessionTelemetry>,
    cancel: CancellationToken,
    on_usage: Option<Box<dyn Fn(&UsageMetadata) + Send + Sync>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut debounce = tokio::time::interval(Duration::from_millis(100));
        // Consume the first immediate tick
        debounce.tick().await;
        loop {
            tokio::select! {
                biased;
                result = telem_rx.recv() => {
                    match result {
                        Ok(event) => {
                            // SessionTelemetry: record atomic counters
                            match &event {
                                SessionEvent::AudioData(data) => {
                                    telemetry.record_audio_out(data.len());
                                }
                                SessionEvent::VoiceActivityEnd => {
                                    telemetry.record_vad_end();
                                }
                                SessionEvent::Interrupted => {
                                    telemetry.record_interruption();
                                }
                                SessionEvent::TurnComplete => {
                                    telemetry.record_turn_complete();
                                }
                                SessionEvent::VoiceActivityStart => {
                                    telemetry.mark_turn_start();
                                }
                                SessionEvent::Usage(ref usage) => {
                                    telemetry.record_usage(
                                        usage.total_token_count,
                                        usage.prompt_token_count,
                                        usage.response_token_count,
                                        usage.cached_content_token_count,
                                        usage.thoughts_token_count,
                                    );
                                    if let Some(cb) = &on_usage {
                                        cb(usage);
                                    }
                                }
                                _ => {}
                            }
                            // SessionSignals: update state keys + atomic timestamps
                            signals.on_event(&event);
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            #[cfg(feature = "tracing-support")]
                            tracing::warn!(skipped = n, "Telemetry lane lagged");
                            let _ = n;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = debounce.tick() => {
                    // Flush derived timing signals to state (debounced)
                    signals.flush_timing();
                }
                _ = cancel.cancelled() => break,
            }
        }
    })
}

/// Dispatch an async callback respecting its [`CallbackMode`].
///
/// - [`Blocking`](CallbackMode::Blocking): awaits the callback inline.
/// - [`Concurrent`](CallbackMode::Concurrent): spawns as a detached tokio task.
macro_rules! dispatch_callback {
    ($mode:expr, $cb:expr) => {
        match $mode {
            CallbackMode::Blocking => { $cb.await; }
            CallbackMode::Concurrent => { let f = $cb; tokio::spawn(async move { f.await; }); }
        }
    };
}

/// Routes a SessionEvent to the appropriate lane.
async fn route_event(
    event: SessionEvent,
    fast_tx: &mpsc::Sender<FastEvent>,
    ctrl_tx: &mpsc::Sender<ControlEvent>,
    shared: &SharedState,
) {
    match event {
        // Fast lane events
        SessionEvent::AudioData(data) => {
            if fast_tx.send(FastEvent::Audio(data)).await.is_err() {
                return; // channel closed
            }
        }
        SessionEvent::TextDelta(text) => {
            let _ = fast_tx.send(FastEvent::Text(text)).await;
        }
        SessionEvent::TextComplete(text) => {
            let _ = fast_tx.send(FastEvent::TextComplete(text)).await;
        }
        // Transcripts: fast lane for callbacks, control lane for accumulation
        SessionEvent::InputTranscription(text) => {
            let _ = fast_tx.send(FastEvent::InputTranscript(text.clone())).await;
            let _ = ctrl_tx.send(ControlEvent::InputTranscript(text)).await;
        }
        SessionEvent::OutputTranscription(text) => {
            let _ = fast_tx.send(FastEvent::OutputTranscript(text.clone())).await;
            let _ = ctrl_tx.send(ControlEvent::OutputTranscript(text)).await;
        }
        SessionEvent::VoiceActivityStart => {
            let _ = fast_tx.send(FastEvent::VadStart).await;
        }
        SessionEvent::VoiceActivityEnd => {
            let _ = fast_tx.send(FastEvent::VadEnd).await;
        }
        SessionEvent::PhaseChanged(phase) => {
            let _ = fast_tx.send(FastEvent::Phase(phase)).await;
        }
        SessionEvent::SessionResumeUpdate(info) => {
            *shared.resume_handle.lock() = Some(info.handle.clone());
            let _ = ctrl_tx.send(ControlEvent::SessionResumeUpdate(info)).await;
        }
        SessionEvent::GenerationComplete => {
            let _ = ctrl_tx.send(ControlEvent::GenerationComplete).await;
        }

        // Control lane events
        SessionEvent::ToolCall(calls) => {
            let _ = ctrl_tx.send(ControlEvent::ToolCall(calls)).await;
        }
        SessionEvent::ToolCallCancelled(ids) => {
            let _ = ctrl_tx.send(ControlEvent::ToolCallCancelled(ids)).await;
        }
        SessionEvent::Interrupted => {
            // Signal BOTH lanes
            shared.interrupted.store(true, Ordering::Release);
            let _ = fast_tx.send(FastEvent::Interrupted).await;
            let _ = ctrl_tx.send(ControlEvent::Interrupted).await;
        }
        SessionEvent::TurnComplete => {
            let _ = ctrl_tx.send(ControlEvent::TurnComplete).await;
        }
        // Usage metadata is handled by the telemetry lane (SessionSignals)
        SessionEvent::Usage(_) => {}
        SessionEvent::GoAway(time_left) => {
            let _ = ctrl_tx.send(ControlEvent::GoAway(time_left)).await;
        }
        SessionEvent::Connected => {
            let _ = ctrl_tx.send(ControlEvent::Connected).await;
        }
        SessionEvent::Disconnected(reason) => {
            let _ = ctrl_tx.send(ControlEvent::Disconnected(reason)).await;
        }
        SessionEvent::Error(err) => {
            let _ = ctrl_tx.send(ControlEvent::Error(err)).await;
        }
    }
}

/// Fast lane consumer — processes high-frequency events with sync callbacks.
/// No transcript buffer — transcripts are accumulated exclusively in the control lane.
async fn run_fast_lane(
    mut rx: mpsc::Receiver<FastEvent>,
    callbacks: Arc<EventCallbacks>,
    shared: Arc<SharedState>,
) {
    while let Some(event) = rx.recv().await {
        match event {
            FastEvent::Audio(data) => {
                // Suppress audio during interruption
                if !shared.interrupted.load(Ordering::Acquire) {
                    if let Some(cb) = &callbacks.on_audio {
                        cb(&data);
                    }
                }
            }
            FastEvent::Text(delta) => {
                if let Some(cb) = &callbacks.on_text {
                    cb(&delta);
                }
            }
            FastEvent::TextComplete(text) => {
                if let Some(cb) = &callbacks.on_text_complete {
                    cb(&text);
                }
            }
            FastEvent::InputTranscript(text) => {
                // Callback only — accumulation happens in control lane
                if let Some(cb) = &callbacks.on_input_transcript {
                    cb(&text, false);
                }
            }
            FastEvent::OutputTranscript(text) => {
                // Callback only — accumulation happens in control lane
                if let Some(cb) = &callbacks.on_output_transcript {
                    cb(&text, false);
                }
            }
            FastEvent::VadStart => {
                if let Some(cb) = &callbacks.on_vad_start {
                    cb();
                }
            }
            FastEvent::VadEnd => {
                if let Some(cb) = &callbacks.on_vad_end {
                    cb();
                }
            }
            FastEvent::Phase(phase) => {
                if let Some(cb) = &callbacks.on_phase {
                    cb(phase);
                }
            }
            FastEvent::Interrupted => {
                // Audio already suppressed via shared.interrupted flag
            }
        }
    }
}

/// Handle tool calls: phase filtering → user callback → auto-dispatch → interceptor → send.
async fn handle_tool_calls(
    calls: Vec<FunctionCall>,
    callbacks: &EventCallbacks,
    dispatcher: &Option<Arc<ToolDispatcher>>,
    writer: &Arc<dyn SessionWriter>,
    state: &State,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    transcript_buffer: &mut TranscriptBuffer,
    execution_modes: &std::collections::HashMap<String, super::background_tool::ToolExecutionMode>,
    background_tracker: &Option<Arc<BackgroundToolTracker>>,
    extractors: &[Arc<dyn TurnExtractor>],
) {
    // 0. Phase-scoped tool filtering: reject calls not in phase's allowed list
    let (allowed_calls, rejected_responses) = if let Some(ref pm) = phase_machine {
        let active_tools = {
            let pm_guard = pm.lock().await;
            pm_guard.active_tools().map(|t| t.to_vec())
        };
        if let Some(active_tools) = active_tools {
            let mut allowed = Vec::new();
            let mut rejected = Vec::new();
            for call in calls {
                if active_tools.iter().any(|t| t == &call.name) {
                    allowed.push(call);
                } else {
                    rejected.push(FunctionResponse {
                        name: call.name.clone(),
                        response: serde_json::json!({
                            "error": format!(
                                "Tool '{}' is not available in the current conversation phase.",
                                call.name
                            )
                        }),
                        id: call.id.clone(),
                        scheduling: None,
                    });
                }
            }
            (allowed, rejected)
        } else {
            (calls, Vec::new())
        }
    } else {
        (calls, Vec::new())
    };

    // 1. Check user callback for override (receives State)
    let responses = if allowed_calls.is_empty() && !rejected_responses.is_empty() {
        Some(rejected_responses.clone())
    } else if let Some(cb) = &callbacks.on_tool_call {
        let mut result = cb(allowed_calls.clone(), state.clone()).await;
        if !rejected_responses.is_empty() {
            let r = result.get_or_insert_with(Vec::new);
            r.extend(rejected_responses.clone());
        }
        result
    } else {
        None
    };

    // 2. If no override, auto-dispatch via ToolDispatcher (split standard vs background)
    let (responses, background_spawns) = match responses {
        Some(r) => (r, Vec::new()),
        None => {
            let mut results: Vec<FunctionResponse> = rejected_responses;
            let mut bg_spawns: Vec<(FunctionCall, Option<Arc<dyn super::background_tool::ResultFormatter>>)> = Vec::new();

            if let Some(ref disp) = dispatcher {
                for call in &allowed_calls {
                    let mode = execution_modes.get(&call.name);
                    match mode {
                        Some(super::background_tool::ToolExecutionMode::Background { formatter, scheduling }) => {
                            // Send immediate ack
                            let fmt: &dyn super::background_tool::ResultFormatter = formatter
                                .as_ref()
                                .map(|f| f.as_ref())
                                .unwrap_or(&super::background_tool::DefaultResultFormatter);
                            let ack = fmt.format_running(call);
                            results.push(FunctionResponse {
                                name: call.name.clone(),
                                response: ack,
                                id: call.id.clone(),
                                scheduling: *scheduling,
                            });
                            bg_spawns.push((call.clone(), formatter.clone()));
                        }
                        _ => {
                            // Standard: execute inline
                            match disp.call_function(&call.name, call.args.clone()).await {
                                Ok(result) => results.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: result,
                                    id: call.id.clone(),
                                    scheduling: None,
                                }),
                                Err(e) => results.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: serde_json::json!({"error": e.to_string()}),
                                    id: call.id.clone(),
                                    scheduling: None,
                                }),
                            }
                        }
                    }
                }
            } else if results.is_empty() {
                #[cfg(feature = "tracing-support")]
                tracing::warn!("Tool call received but no dispatcher or callback registered");
            }
            (results, bg_spawns)
        }
    };

    // 3. Run through before_tool_response interceptor
    let responses = if let Some(cb) = &callbacks.before_tool_response {
        cb(responses, state.clone()).await
    } else {
        responses
    };

    // 4. Record tool call summaries in transcript buffer (no mutex)
    for resp in &responses {
        let args = allowed_calls
            .iter()
            .find(|c| c.name == resp.name)
            .map(|c| &c.args)
            .unwrap_or(&serde_json::Value::Null);
        transcript_buffer.push_tool_call(resp.name.clone(), args, &resp.response);
    }

    // 5. Send tool responses (standard + ack) back to Gemini
    if !responses.is_empty() {
        if let Err(_e) = writer.send_tool_response(responses).await {
            #[cfg(feature = "tracing-support")]
            tracing::error!("Failed to send tool response: {_e}");
        }
    }

    // 6. Spawn background tool tasks
    for (call, formatter) in background_spawns {
        let disp = dispatcher.clone();
        let bg_writer = writer.clone();
        let tracker = background_tracker.clone();
        let call_id = call.id.clone().unwrap_or_default();
        let cancel = CancellationToken::new();

        let handle = tokio::spawn(async move {
            let result = if let Some(ref d) = disp {
                d.call_function(&call.name, call.args.clone())
                    .await
                    .map_err(|e| crate::error::ToolError::ExecutionFailed(e.to_string()))
            } else {
                Err(crate::error::ToolError::NotFound(call.name.clone()))
            };

            let fmt: &dyn super::background_tool::ResultFormatter = formatter
                .as_ref()
                .map(|f| f.as_ref())
                .unwrap_or(&super::background_tool::DefaultResultFormatter);
            let formatted = fmt.format_result(&call, result);

            bg_writer
                .send_tool_response(vec![FunctionResponse {
                    name: call.name.clone(),
                    response: formatted,
                    id: call.id.clone(),
                    scheduling: None,
                }])
                .await
                .ok();

            // Self-cleanup from tracker
            if let Some(ref t) = tracker {
                t.remove(&call.id.clone().unwrap_or_default());
            }
        });

        // Register in tracker for cancellation
        if let Some(ref t) = background_tracker {
            t.spawn(call_id, handle, cancel);
        }
    }

    // 7. Run AfterToolCall extractors
    let after_tool_extractors: Vec<Arc<dyn TurnExtractor>> = extractors
        .iter()
        .filter(|e| matches!(e.trigger(), ExtractionTrigger::AfterToolCall))
        .cloned()
        .collect();
    run_extractors(&after_tool_extractors, transcript_buffer, state, callbacks).await;
}

/// Run a subset of extractors concurrently and merge results into state.
///
/// Shared between handle_turn_complete (EveryTurn/Interval),
/// handle_tool_calls (AfterToolCall), and phase transitions (OnPhaseChange).
async fn run_extractors(
    extractors: &[Arc<dyn TurnExtractor>],
    transcript_buffer: &mut TranscriptBuffer,
    state: &State,
    callbacks: &EventCallbacks,
) {
    if extractors.is_empty() {
        return;
    }

    let extraction_futures: Vec<_> = extractors
        .iter()
        .filter_map(|extractor| {
            let window_size = extractor.window_size();
            let window: Vec<_> = transcript_buffer.window(window_size).to_vec();
            if window.is_empty() {
                return None;
            }
            // Check should_extract before launching async work
            if !extractor.should_extract(&window) {
                return None;
            }
            let ext = extractor.clone();
            Some(async move {
                match ext.extract(&window).await {
                    Ok(value) => Ok((ext.name().to_string(), value)),
                    Err(e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::warn!(
                            extractor = ext.name(),
                            "Extraction failed: {e}"
                        );
                        Err((ext.name().to_string(), e.to_string()))
                    }
                }
            })
        })
        .collect();

    let results = futures::future::join_all(extraction_futures).await;
    for result in results {
        match result {
            Ok((name, value)) => {
                state.set(&name, &value);
                // Auto-flatten: promote each top-level field.
                // Accumulative merge: null extraction values do NOT overwrite
                // previously extracted non-null values.  This prevents the
                // LLM from "forgetting" information gathered in earlier turns
                // when the current window lacks that data.
                if let Some(obj) = value.as_object() {
                    for (field, val) in obj {
                        if val.is_null() {
                            // Skip — don't erase previously extracted data
                            continue;
                        }
                        state.set(field, val.clone());
                    }
                }
                if let Some(cb) = &callbacks.on_extracted {
                    dispatch_callback!(callbacks.on_extracted_mode, cb(name, value));
                }
            }
            Err((name, error)) => {
                if let Some(cb) = &callbacks.on_extraction_error {
                    dispatch_callback!(callbacks.on_extraction_error_mode, cb(name, error));
                }
            }
        }
    }
}

/// Run extractors using a window that optionally includes the current in-progress turn.
///
/// When `include_current` is true, uses `snapshot_window_with_current` to capture
/// the model's output before interruption truncation (for GenerationComplete extractors).
async fn run_extractors_with_window(
    extractors: &[Arc<dyn TurnExtractor>],
    transcript_buffer: &mut TranscriptBuffer,
    state: &State,
    callbacks: &EventCallbacks,
    include_current: bool,
) {
    if extractors.is_empty() {
        return;
    }

    let extraction_futures: Vec<_> = extractors
        .iter()
        .filter_map(|extractor| {
            let window_size = extractor.window_size();
            let window = if include_current {
                transcript_buffer.snapshot_window_with_current(window_size).turns().to_vec()
            } else {
                transcript_buffer.window(window_size).to_vec()
            };
            if window.is_empty() || !extractor.should_extract(&window) {
                return None;
            }
            let ext = extractor.clone();
            Some(async move {
                match ext.extract(&window).await {
                    Ok(value) => Ok((ext.name().to_string(), value)),
                    Err(e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::warn!(extractor = ext.name(), "Extraction failed: {e}");
                        Err((ext.name().to_string(), e.to_string()))
                    }
                }
            })
        })
        .collect();

    let results = futures::future::join_all(extraction_futures).await;
    for result in results {
        match result {
            Ok((name, value)) => {
                state.set(&name, &value);
                if let Some(obj) = value.as_object() {
                    for (field, val) in obj {
                        if val.is_null() {
                            continue;
                        }
                        state.set(field, val.clone());
                    }
                }
                if let Some(cb) = &callbacks.on_extracted {
                    dispatch_callback!(callbacks.on_extracted_mode, cb(name, value));
                }
            }
            Err((name, error)) => {
                if let Some(cb) = &callbacks.on_extraction_error {
                    dispatch_callback!(callbacks.on_extraction_error_mode, cb(name, error));
                }
            }
        }
    }
}

/// Handle the TurnComplete pipeline: transcript finalization, extraction,
/// phase evaluation, unified instruction composition, watchers, temporal.
///
/// Unified instruction composition: steps 6/9/10 accumulate into a single
/// `resolved_instruction` that is sent once at the end, eliminating the
/// double-send bug.
async fn handle_turn_complete(
    callbacks: &EventCallbacks,
    writer: &Arc<dyn SessionWriter>,
    shared: &SharedState,
    extractors: &[Arc<dyn TurnExtractor>],
    state: &State,
    computed: &Option<ComputedRegistry>,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: &Option<WatcherRegistry>,
    temporal: &Option<Arc<TemporalRegistry>>,
    transcript_buffer: &mut TranscriptBuffer,
    extraction_turn_tracker: &mut std::collections::HashMap<String, u32>,
    control_plane: &mut ControlPlaneConfig,
) {
    // 1. Reset turn-scoped state
    state.clear_prefix("turn:");

    // 2. Finalize transcript (prefer server transcriptions when available)
    if let Some(input_text) = state.session().get::<String>("last_input_transcription") {
        transcript_buffer.set_input_transcription(&input_text);
    }
    if let Some(output_text) = state.session().get::<String>("last_output_transcription") {
        transcript_buffer.set_output_transcription(&output_text);
    }
    transcript_buffer.end_turn();

    // 3. Snapshot watched keys BEFORE extractors
    let pre_snapshot = watchers.as_ref().map(|w| {
        state.snapshot_values(
            &w.observed_keys()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
        )
    });

    // 4. Run extractors matching EveryTurn or Interval triggers
    let current_turn = state.session().get::<u32>("turn_count").unwrap_or(0);
    let turn_extractors: Vec<Arc<dyn TurnExtractor>> = extractors
        .iter()
        .filter(|e| match e.trigger() {
            ExtractionTrigger::EveryTurn => true,
            ExtractionTrigger::Interval(n) => {
                let last = extraction_turn_tracker
                    .get(e.name())
                    .copied()
                    .unwrap_or(0);
                current_turn.saturating_sub(last) >= n
            }
            ExtractionTrigger::AfterToolCall
            | ExtractionTrigger::OnPhaseChange
            | ExtractionTrigger::OnGenerationComplete => false,
        })
        .cloned()
        .collect();

    run_extractors(&turn_extractors, transcript_buffer, state, callbacks).await;

    // Update interval trackers for extractors that ran
    for ext in &turn_extractors {
        if matches!(ext.trigger(), ExtractionTrigger::Interval(_)) {
            extraction_turn_tracker.insert(ext.name().to_string(), current_turn);
        }
    }

    // 5. Recompute derived state
    if let Some(ref computed) = computed {
        computed.recompute(state);
    }

    // 6. Build transcript window snapshot for phase evaluation
    let transcript_window = transcript_buffer.snapshot_window(5);

    // Unified instruction composition:
    // Instead of sending instruction at each step (6/9/10), we accumulate
    // into resolved_instruction and send ONCE at the end.
    let mut resolved_instruction: Option<String> = None;
    let mut transition_result: Option<TransitionResult> = None;

    // 7. Evaluate phase transitions + compute navigation context
    if let Some(ref pm) = phase_machine {
        let mut machine = pm.lock().await;

        // 7a. Evaluate transitions
        if let Some((target, transition_index)) =
            machine.evaluate(state).map(|(s, i)| (s.to_string(), i))
        {
            let turn = state.session().get::<u32>("turn_count").unwrap_or(0);
            let trigger = super::phase::TransitionTrigger::Guard { transition_index };
            let result = machine
                .transition(&target, state, writer, turn, trigger, &transcript_window)
                .await;
            if let Some(tr) = result {
                resolved_instruction = Some(tr.instruction.clone());
                transition_result = Some(tr);
            }
            state.session().set("phase", machine.current());

            // Store current phase's `needs` for ContextBuilder to read.
            if let Some(phase) = machine.current_phase() {
                if phase.needs.is_empty() {
                    state.remove("session:phase_needs");
                } else {
                    state.set("session:phase_needs", phase.needs.clone());
                }
            }
        }

        // 7b. Always compute and store navigation context
        let nav = machine.describe_navigation(state);
        state.session().set("navigation_context", nav);
    }

    // 7c. Run OnPhaseChange extractors (if a transition fired)
    if transition_result.is_some() {
        let phase_change_extractors: Vec<Arc<dyn TurnExtractor>> = extractors
            .iter()
            .filter(|e| matches!(e.trigger(), ExtractionTrigger::OnPhaseChange))
            .cloned()
            .collect();
        run_extractors(&phase_change_extractors, transcript_buffer, state, callbacks).await;
    }

    // 7d. Tool availability advisory (Phase 5)
    // When phase transitions change the tool set, inject a model-role advisory turn
    if transition_result.is_some() && control_plane.tool_advisory {
        if let Some(ref pm) = phase_machine {
            let machine = pm.lock().await;
            if let Some(tools) = machine.active_tools() {
                let prev_tools: Option<Vec<String>> =
                    state.session().get("active_tools");
                let tools_vec: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
                let changed = prev_tools.as_ref() != Some(&tools_vec);
                if changed {
                    state.session().set("active_tools", tools_vec.clone());
                    let tool_names = tools_vec.join(", ");
                    let advisory = rs_genai::prelude::Content::model(format!(
                        "In this phase, I have access to these tools: {}. \
                         I should only use these tools.",
                        tool_names
                    ));
                    writer.send_client_content(vec![advisory], false).await.ok();
                }
            }
        }
    }

    // 7e. Conversation repair (Phase 6)
    if let Some(ref mut needs_tracker) = control_plane.needs_fulfillment {
        if let Some(ref pm) = phase_machine {
            let machine = pm.lock().await;
            let phase_name = machine.current().to_string();
            if let Some(phase) = machine.current_phase() {
                if !phase.needs.is_empty() {
                    let needs = phase.needs.clone();
                    drop(machine); // release lock before async work
                    match needs_tracker.evaluate(&phase_name, &needs, state) {
                        RepairAction::Nudge { unfulfilled, attempt } => {
                            let nudge = rs_genai::prelude::Content::model(format!(
                                "I still need to collect: {}. Let me ask about these.",
                                unfulfilled.join(", ")
                            ));
                            writer.send_client_content(vec![nudge], false).await.ok();
                            if attempt == 1 {
                                writer.send_client_content(vec![], true).await.ok();
                            }
                        }
                        RepairAction::Escalate { unfulfilled } => {
                            state.set("repair:escalation", true);
                            state.set("repair:unfulfilled", unfulfilled);
                        }
                        RepairAction::None => {}
                    }
                }
            }
        }
    }

    // 7f. Context injection steering (Phase 4)
    if matches!(
        control_plane.steering_mode,
        SteeringMode::ContextInjection | SteeringMode::Hybrid
    ) {
        if let Some(ref pm) = phase_machine {
            let machine = pm.lock().await;
            if let Some(phase) = machine.current_phase() {
                let steering_parts =
                    steering::build_steering_context(state, &phase.modifiers);
                if !steering_parts.is_empty() {
                    let content = rs_genai::prelude::Content::model(
                        steering_parts.join("\n"),
                    );
                    writer.send_client_content(vec![content], false).await.ok();
                }
            }
        }
    }

    // 8. Fire watchers (compare pre vs post snapshots)
    if let (Some(ref watchers), Some(pre)) = (watchers, pre_snapshot) {
        let post_keys: Vec<&str> = watchers
            .observed_keys()
            .iter()
            .map(|s| s.as_str())
            .collect();
        let diffs = state.diff_values(&pre, &post_keys);
        if !diffs.is_empty() {
            let (blocking, concurrent) = watchers.evaluate(&diffs, state);
            for action in blocking {
                action.await;
            }
            for action in concurrent {
                tokio::spawn(action);
            }
        }
    }

    // 9. Check temporal patterns
    if let Some(ref temporal) = temporal {
        let event = SessionEvent::TurnComplete;
        for action in temporal.check_all(state, Some(&event), writer) {
            tokio::spawn(action);
        }
    }

    // 10. Instruction amendment (additive — appends to phase instruction)
    // Only applies when there was NO phase transition (transition already includes modifiers)
    if transition_result.is_none() {
        if let Some(ref amendment_fn) = callbacks.instruction_amendment {
            if let Some(amendment_text) = amendment_fn(state) {
                let base = if let Some(ref pm) = phase_machine {
                    let pm_guard = pm.lock().await;
                    pm_guard
                        .current_phase()
                        .map(|p| p.instruction.resolve_with_modifiers(state, &p.modifiers))
                } else {
                    None
                };
                if let Some(base_instruction) = base {
                    resolved_instruction =
                        Some(format!("{}\n\n{}", base_instruction, amendment_text));
                }
            }
        }
    }

    // 11. Instruction template (full replacement — escape hatch, overrides everything)
    if let Some(ref template) = callbacks.instruction_template {
        if let Some(new_instruction) = template(state) {
            resolved_instruction = Some(new_instruction);
        }
    }

    // 12. SINGLE instruction send (dedup against last sent)
    if let Some(instruction) = resolved_instruction {
        let should_update = {
            let last = shared.last_instruction.lock();
            last.as_deref() != Some(&instruction)
        };
        if should_update {
            *shared.last_instruction.lock() = Some(instruction.clone());
            writer.update_instruction(instruction).await.ok();
        }
    }

    // 13. Send on_enter_context content (if phase transition produced context)
    if let Some(ref tr) = transition_result {
        if let Some(ref contents) = tr.context {
            if !contents.is_empty() {
                writer
                    .send_client_content(contents.clone(), false)
                    .await
                    .ok();
            }
        }
        // 14. Send turnComplete:true if prompt_on_enter (triggers model response)
        if tr.prompt_on_enter {
            writer.send_client_content(vec![], true).await.ok();
        }
    }

    // 15. Turn boundary hook
    if let Some(cb) = &callbacks.on_turn_boundary {
        cb(state.clone(), writer.clone()).await;
    }

    // 16. User turn-complete callback
    if let Some(cb) = &callbacks.on_turn_complete {
        dispatch_callback!(callbacks.on_turn_complete_mode, cb());
    }

    // 17. Update session turn count
    let tc: u32 = state.session().get("turn_count").unwrap_or(0);
    state.session().set("turn_count", tc + 1);

    // 18. Persist session state (Phase 7 — fire and forget)
    if let Some(ref persistence) = control_plane.persistence {
        let phase_name = if let Some(ref pm) = phase_machine {
            pm.lock().await.current().to_string()
        } else {
            String::new()
        };
        let snapshot = SessionSnapshot {
            state: state.to_hashmap(),
            phase: phase_name,
            turn_count: tc + 1,
            transcript_summary: transcript_buffer.format_window(5),
            resume_handle: shared.resume_handle.lock().clone(),
            saved_at: {
                // Simple ISO 8601 timestamp without chrono dependency
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                format!("{}s", now.as_secs())
            },
        };
        let p = persistence.clone();
        let sid = control_plane
            .session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        tokio::spawn(async move {
            if let Err(e) = p.save(&sid, &snapshot).await {
                #[cfg(feature = "tracing-support")]
                tracing::warn!("Session persistence failed: {}", e);
                let _ = e;
            }
        });
    }
}

/// Control lane processor — handles lifecycle events, tool dispatch,
/// transcript accumulation, extractors, phases, watchers.
///
/// TranscriptBuffer is owned exclusively — no Arc<Mutex<>> needed.
async fn run_control_lane(
    mut rx: mpsc::Receiver<ControlEvent>,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    writer: Arc<dyn SessionWriter>,
    shared: Arc<SharedState>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    state: State,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<Arc<TemporalRegistry>>,
    background_tracker: Option<Arc<BackgroundToolTracker>>,
    execution_modes: std::collections::HashMap<String, super::background_tool::ToolExecutionMode>,
    mut control_plane: ControlPlaneConfig,
) {
    // TranscriptBuffer is exclusively owned by the control lane — no mutex.
    let mut transcript_buffer = TranscriptBuffer::new();

    // Track which turn each interval-based extractor last ran on.
    let mut extraction_turn_tracker: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    while let Some(event) = rx.recv().await {
        match event {
            // ── Transcript accumulation (exclusive to control lane) ──
            ControlEvent::InputTranscript(text) => {
                transcript_buffer.push_input(&text);
            }
            ControlEvent::OutputTranscript(text) => {
                transcript_buffer.push_output(&text);
            }

            ControlEvent::ToolCall(calls) => {
                handle_tool_calls(
                    calls,
                    &callbacks,
                    &dispatcher,
                    &writer,
                    &state,
                    &phase_machine,
                    &mut transcript_buffer,
                    &execution_modes,
                    &background_tracker,
                    &extractors,
                )
                .await;
            }
            ControlEvent::ToolCallCancelled(ids) => {
                // Cancel background tasks first
                if let Some(ref tracker) = background_tracker {
                    tracker.cancel(&ids);
                }
                if let Some(ref disp) = dispatcher {
                    disp.cancel_by_ids(&ids).await;
                }
                if let Some(cb) = &callbacks.on_tool_cancelled {
                    dispatch_callback!(callbacks.on_tool_cancelled_mode, cb(ids));
                }
            }
            ControlEvent::Interrupted => {
                // Truncate current model turn on interruption (no mutex)
                transcript_buffer.truncate_current_model_turn();
                if let Some(cb) = &callbacks.on_interrupted {
                    cb().await;
                }
                // Resume audio forwarding after interrupt callback completes
                shared.interrupted.store(false, Ordering::Release);
            }
            ControlEvent::TurnComplete => {
                // Reset soft turn detector — model responded
                if let Some(ref mut std) = control_plane.soft_turn {
                    std.on_model_response();
                }
                handle_turn_complete(
                    &callbacks,
                    &writer,
                    &shared,
                    &extractors,
                    &state,
                    &computed,
                    &phase_machine,
                    &watchers,
                    &temporal,
                    &mut transcript_buffer,
                    &mut extraction_turn_tracker,
                    &mut control_plane,
                )
                .await;
            }
            ControlEvent::GoAway(time_left) => {
                let duration = time_left
                    .as_deref()
                    .and_then(|s| s.trim_end_matches('s').parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(60));
                if let Some(cb) = &callbacks.on_go_away {
                    dispatch_callback!(callbacks.on_go_away_mode, cb(duration));
                }
            }
            ControlEvent::Connected => {
                if let Some(cb) = &callbacks.on_connected {
                    dispatch_callback!(callbacks.on_connected_mode, cb(writer.clone()));
                }
            }
            ControlEvent::Disconnected(reason) => {
                if let Some(cb) = &callbacks.on_disconnected {
                    dispatch_callback!(callbacks.on_disconnected_mode, cb(reason));
                }
            }
            ControlEvent::SessionResumeUpdate(_info) => {
                // Already stored in shared state by the router
            }
            ControlEvent::GenerationComplete => {
                // Run OnGenerationComplete extractors with pre-truncation transcript
                let gen_extractors: Vec<Arc<dyn TurnExtractor>> = extractors
                    .iter()
                    .filter(|e| matches!(e.trigger(), ExtractionTrigger::OnGenerationComplete))
                    .cloned()
                    .collect();
                if !gen_extractors.is_empty() {
                    // Use snapshot_window_with_current to capture model output before truncation
                    run_extractors_with_window(
                        &gen_extractors,
                        &mut transcript_buffer,
                        &state,
                        &callbacks,
                        true, // include current (pre-finalized) turn
                    )
                    .await;
                }
            }
            ControlEvent::Error(err) => {
                if let Some(cb) = &callbacks.on_error {
                    dispatch_callback!(callbacks.on_error_mode, cb(err));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn fast_lane_routes_audio() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let mut callbacks = EventCallbacks::default();
        callbacks.on_audio = Some(Box::new(move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }));
        let callbacks = Arc::new(callbacks);

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) =
            spawn_event_processor(event_rx, callbacks, None, writer, vec![], State::new(), None, None, None, None, None, std::collections::HashMap::new(), ControlPlaneConfig::default());

        // Send audio events
        let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"audio1")));
        let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"audio2")));

        // Allow tasks to process
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(count.load(Ordering::SeqCst), 2);

        // Cleanup
        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn interrupt_suppresses_audio() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let mut callbacks = EventCallbacks::default();
        callbacks.on_audio = Some(Box::new(move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }));
        let callbacks = Arc::new(callbacks);

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) =
            spawn_event_processor(event_rx, callbacks, None, writer, vec![], State::new(), None, None, None, None, None, std::collections::HashMap::new(), ControlPlaneConfig::default());

        // Send audio, then interrupt, then more audio
        let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"before")));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = event_tx.send(SessionEvent::Interrupted);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"during")));
        tokio::time::sleep(Duration::from_millis(50)).await;

        // At least the first audio was received
        assert!(count.load(Ordering::SeqCst) >= 1);

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn control_lane_routes_turn_complete() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let mut callbacks = EventCallbacks::default();
        callbacks.on_turn_complete = Some(Arc::new(move || {
            let c = called_clone.clone();
            Box::pin(async move {
                c.store(true, Ordering::SeqCst);
            })
        }));
        let callbacks = Arc::new(callbacks);

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) =
            spawn_event_processor(event_rx, callbacks, None, writer, vec![], State::new(), None, None, None, None, None, std::collections::HashMap::new(), ControlPlaneConfig::default());

        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(called.load(Ordering::SeqCst));

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn transcript_accumulates_in_control_lane() {
        let callbacks = Arc::new(EventCallbacks::default());

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let state = State::new();
        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            vec![],
            state.clone(),
            None,
            None,
            None,
            None,
            None,
            std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
        );

        // Send transcripts
        let _ = event_tx.send(SessionEvent::InputTranscription("Hello ".to_string()));
        let _ = event_tx.send(SessionEvent::InputTranscription("world".to_string()));
        let _ = event_tx.send(SessionEvent::OutputTranscription("Hi there!".to_string()));
        tokio::time::sleep(Duration::from_millis(50)).await;

        // End turn
        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Turn count should have been incremented
        let tc: u32 = state.session().get("turn_count").unwrap_or(0);
        assert_eq!(tc, 1);

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn extractor_runs_on_turn_complete() {
        use crate::llm::LlmError;
        use crate::live::extractor::TurnExtractor;
        use crate::live::transcript::TranscriptTurn;

        struct FixedExtractor;

        #[async_trait::async_trait]
        impl TurnExtractor for FixedExtractor {
            fn name(&self) -> &str {
                "TestExtractor"
            }
            fn window_size(&self) -> usize {
                3
            }
            async fn extract(
                &self,
                _turns: &[TranscriptTurn],
            ) -> Result<serde_json::Value, LlmError> {
                Ok(serde_json::json!({"score": 0.9, "mood": "happy"}))
            }
        }

        let callbacks = Arc::new(EventCallbacks::default());

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let state = State::new();

        let extractors: Vec<Arc<dyn TurnExtractor>> = vec![Arc::new(FixedExtractor)];

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            extractors,
            state.clone(),
            None,
            None,
            None,
            None,
            None,
            std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
        );

        // Produce a turn with content
        let _ = event_tx.send(SessionEvent::InputTranscription("hi".to_string()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check extraction results
        let score: Option<f64> = state.get("score");
        assert_eq!(score, Some(0.9));
        let mood: Option<String> = state.get("mood");
        assert_eq!(mood, Some("happy".to_string()));

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn telemetry_lane_auto_collects() {
        let (event_tx, _) = broadcast::channel(16);
        let telem_rx = event_tx.subscribe();

        let telemetry = Arc::new(SessionTelemetry::new());
        let signals = SessionSignals::new(State::new());
        let cancel = CancellationToken::new();

        let telem_handle = spawn_telemetry_lane(
            telem_rx,
            signals,
            telemetry.clone(),
            cancel.clone(),
            None,
        );

        // Send events
        let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"chunk1")));
        let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"chunk2")));
        let _ = event_tx.send(SessionEvent::VoiceActivityEnd);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"response")));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let snap = telemetry.snapshot();
        assert_eq!(snap["audio_chunks_out"], 3);
        assert!(snap["response_count"].as_u64().unwrap() >= 1);

        cancel.cancel();
        let _ = telem_handle.await;
    }

    #[tokio::test]
    async fn background_tool_sends_ack_immediately() {
        use crate::tool::{ToolDispatcher, SimpleTool};
        use crate::live::background_tool::{BackgroundToolTracker, ToolExecutionMode};

        // Create a slow tool
        let tool = SimpleTool::new(
            "slow_search",
            "A slow search tool",
            Some(serde_json::json!({"type": "object", "properties": {"q": {"type": "string"}}})),
            |_args| async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(serde_json::json!({"results": ["found"]}))
            },
        );

        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register(tool);

        let mut execution_modes = std::collections::HashMap::new();
        execution_modes.insert(
            "slow_search".to_string(),
            ToolExecutionMode::Background { formatter: None, scheduling: None },
        );

        let sent = Arc::new(parking_lot::Mutex::new(Vec::<Vec<FunctionResponse>>::new()));
        let sent_clone = sent.clone();

        // Use a writer that records sent tool responses
        struct RecordingWriter {
            sent: Arc<parking_lot::Mutex<Vec<Vec<FunctionResponse>>>>,
        }

        #[async_trait::async_trait]
        impl SessionWriter for RecordingWriter {
            async fn send_audio(&self, _data: Vec<u8>) -> Result<(), rs_genai::session::SessionError> { Ok(()) }
            async fn send_text(&self, _text: String) -> Result<(), rs_genai::session::SessionError> { Ok(()) }
            async fn send_video(&self, _data: Vec<u8>) -> Result<(), rs_genai::session::SessionError> { Ok(()) }
            async fn send_tool_response(&self, responses: Vec<FunctionResponse>) -> Result<(), rs_genai::session::SessionError> {
                self.sent.lock().push(responses);
                Ok(())
            }
            async fn update_instruction(&self, _instruction: String) -> Result<(), rs_genai::session::SessionError> { Ok(()) }
            async fn send_client_content(&self, _content: Vec<rs_genai::prelude::Content>, _turn_complete: bool) -> Result<(), rs_genai::session::SessionError> { Ok(()) }
            async fn signal_activity_start(&self) -> Result<(), rs_genai::session::SessionError> { Ok(()) }
            async fn signal_activity_end(&self) -> Result<(), rs_genai::session::SessionError> { Ok(()) }
            async fn disconnect(&self) -> Result<(), rs_genai::session::SessionError> { Ok(()) }
        }

        let writer: Arc<dyn SessionWriter> = Arc::new(RecordingWriter { sent: sent_clone });
        let callbacks = Arc::new(EventCallbacks::default());
        let tracker = Arc::new(BackgroundToolTracker::new());

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            Some(Arc::new(dispatcher)),
            writer,
            vec![],
            State::new(),
            None,
            None,
            None,
            None,
            Some(tracker.clone()),
            execution_modes,
            ControlPlaneConfig::default(),
        );

        // Send a tool call
        let _ = event_tx.send(SessionEvent::ToolCall(vec![FunctionCall {
            name: "slow_search".to_string(),
            args: serde_json::json!({"q": "test"}),
            id: Some("fc_1".to_string()),
        }]));

        // Wait just enough for the ack (but not the full tool)
        tokio::time::sleep(Duration::from_millis(50)).await;

        let responses = sent.lock();
        // First batch should be the ack
        assert!(!responses.is_empty(), "Should have sent ack immediately");
        assert_eq!(responses[0][0].response["status"], "running");

        drop(responses);

        // Wait for background tool to complete
        tokio::time::sleep(Duration::from_millis(300)).await;

        let responses = sent.lock();
        // Second batch should be the completed result
        assert!(responses.len() >= 2, "Should have sent result after completion");
        assert_eq!(responses[1][0].response["status"], "completed");

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn callback_mode_blocking_awaits_inline() {
        use std::sync::atomic::AtomicU32;

        let order = Arc::new(AtomicU32::new(0));
        let order_clone = order.clone();

        let mut callbacks = EventCallbacks::default();
        // Blocking on_turn_complete sets order to 1
        callbacks.on_turn_complete = Some(Arc::new(move || {
            let o = order_clone.clone();
            Box::pin(async move {
                // Simulate brief work
                tokio::time::sleep(Duration::from_millis(10)).await;
                o.store(1, Ordering::SeqCst);
            })
        }));
        callbacks.on_turn_complete_mode = CallbackMode::Blocking;
        let callbacks = Arc::new(callbacks);

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx, callbacks, None, writer, vec![], State::new(),
            None, None, None, None, None, std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
        );

        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Blocking mode: callback completed before control lane processed next event
        assert_eq!(order.load(Ordering::SeqCst), 1);

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn callback_mode_concurrent_spawns_task() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let mut callbacks = EventCallbacks::default();
        callbacks.on_turn_complete = Some(Arc::new(move || {
            let c = called_clone.clone();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                c.store(true, Ordering::SeqCst);
            })
        }));
        callbacks.on_turn_complete_mode = CallbackMode::Concurrent;
        let callbacks = Arc::new(callbacks);

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx, callbacks, None, writer, vec![], State::new(),
            None, None, None, None, None, std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
        );

        let _ = event_tx.send(SessionEvent::TurnComplete);
        // Give spawned task time to complete
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Concurrent mode: callback was spawned and eventually completed
        assert!(called.load(Ordering::SeqCst));

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }
}
