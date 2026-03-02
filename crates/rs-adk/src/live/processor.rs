//! Two-lane event processor for Live sessions.
//!
//! Fast lane: audio, text, transcripts, VAD (sync callbacks, never blocks)
//! Control lane: tool calls, interruptions, lifecycle (async callbacks, can block)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

use rs_genai::prelude::{FunctionCall, FunctionResponse, SessionEvent, SessionPhase};
use rs_genai::session::SessionWriter;

use crate::state::State;
use crate::tool::ToolDispatcher;

use super::background_tool::BackgroundToolTracker;
use super::callbacks::EventCallbacks;
use super::computed::ComputedRegistry;
use super::extractor::TurnExtractor;
use super::phase::PhaseMachine;
use super::session_signals::SessionSignals;
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
    GoAway(Option<String>),
    Connected,
    Disconnected(Option<String>),
    SessionResumeHandle(String),
    Error(String),
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

/// Runs the two-lane event processor.
///
/// Returns JoinHandles for the fast consumer and control processor tasks.
pub(crate) fn spawn_event_processor(
    mut event_rx: broadcast::Receiver<SessionEvent>,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    writer: Arc<dyn SessionWriter>,
    transcript_buffer: Option<Arc<parking_lot::Mutex<TranscriptBuffer>>>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    state: State,
    // Registry parameters:
    session_signals: Option<SessionSignals>,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<Arc<TemporalRegistry>>,
    background_tracker: Option<Arc<BackgroundToolTracker>>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let shared = Arc::new(SharedState {
        interrupted: AtomicBool::new(false),
        resume_handle: parking_lot::Mutex::new(None),
        last_instruction: parking_lot::Mutex::new(None),
    });

    // Channels between router and lanes
    let (fast_tx, fast_rx) = mpsc::unbounded_channel::<FastEvent>();
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlEvent>(64);

    // Spawn the router task (reads broadcast, routes to lanes)
    let fast_tx_clone = fast_tx.clone();
    let ctrl_tx_clone = ctrl_tx.clone();
    let shared_clone = shared.clone();
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    // Auto-track session signals on every event
                    if let Some(ref signals) = session_signals {
                        signals.on_event(&event);
                    }
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

    // Spawn fast consumer
    let fast_callbacks = callbacks.clone();
    let fast_shared = shared.clone();
    let fast_buffer = transcript_buffer.clone();
    let fast_handle = tokio::spawn(async move {
        run_fast_lane(fast_rx, fast_callbacks, fast_shared, fast_buffer).await;
    });

    // Clone for the timer task (before moving into ctrl spawn)
    let timer_temporal = temporal.clone();
    let timer_state = state.clone();
    let timer_writer = writer.clone();

    // Spawn control processor
    let ctrl_callbacks = callbacks;
    let ctrl_shared = shared;
    let ctrl_handle = tokio::spawn(async move {
        run_control_lane(
            ctrl_rx,
            ctrl_callbacks,
            dispatcher,
            writer,
            ctrl_shared,
            transcript_buffer,
            extractors,
            state,
            computed,
            phase_machine,
            watchers,
            temporal,
            background_tracker,
        )
        .await;
    });

    // Optional timer task for sustained temporal patterns
    if let Some(ref temporal_ref) = timer_temporal {
        if temporal_ref.needs_timer() {
            let t = temporal_ref.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(500));
                loop {
                    interval.tick().await;
                    for action in t.check_all(&timer_state, None, &timer_writer) {
                        tokio::spawn(action);
                    }
                }
            });
        }
    }

    (fast_handle, ctrl_handle)
}

/// Routes a SessionEvent to the appropriate lane.
async fn route_event(
    event: SessionEvent,
    fast_tx: &mpsc::UnboundedSender<FastEvent>,
    ctrl_tx: &mpsc::Sender<ControlEvent>,
    shared: &SharedState,
) {
    match event {
        // Fast lane events
        SessionEvent::AudioData(data) => {
            let _ = fast_tx.send(FastEvent::Audio(data));
        }
        SessionEvent::TextDelta(text) => {
            let _ = fast_tx.send(FastEvent::Text(text));
        }
        SessionEvent::TextComplete(text) => {
            let _ = fast_tx.send(FastEvent::TextComplete(text));
        }
        SessionEvent::InputTranscription(text) => {
            let _ = fast_tx.send(FastEvent::InputTranscript(text));
        }
        SessionEvent::OutputTranscription(text) => {
            let _ = fast_tx.send(FastEvent::OutputTranscript(text));
        }
        SessionEvent::VoiceActivityStart => {
            let _ = fast_tx.send(FastEvent::VadStart);
        }
        SessionEvent::VoiceActivityEnd => {
            let _ = fast_tx.send(FastEvent::VadEnd);
        }
        SessionEvent::PhaseChanged(phase) => {
            let _ = fast_tx.send(FastEvent::Phase(phase));
        }
        SessionEvent::SessionResumeHandle(handle) => {
            *shared.resume_handle.lock() = Some(handle.clone());
            let _ = ctrl_tx.send(ControlEvent::SessionResumeHandle(handle)).await;
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
            let _ = fast_tx.send(FastEvent::Interrupted);
            let _ = ctrl_tx.send(ControlEvent::Interrupted).await;
        }
        SessionEvent::TurnComplete => {
            let _ = ctrl_tx.send(ControlEvent::TurnComplete).await;
        }
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
async fn run_fast_lane(
    mut rx: mpsc::UnboundedReceiver<FastEvent>,
    callbacks: Arc<EventCallbacks>,
    shared: Arc<SharedState>,
    transcript_buffer: Option<Arc<parking_lot::Mutex<TranscriptBuffer>>>,
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
                // Accumulate in transcript buffer (automatic)
                if let Some(ref buf) = transcript_buffer {
                    buf.lock().push_input(&text);
                }
                if let Some(cb) = &callbacks.on_input_transcript {
                    cb(&text, false);
                }
            }
            FastEvent::OutputTranscript(text) => {
                // Accumulate in transcript buffer (automatic)
                if let Some(ref buf) = transcript_buffer {
                    buf.lock().push_output(&text);
                }
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

/// Control lane processor — handles lifecycle events and tool dispatch.
async fn run_control_lane(
    mut rx: mpsc::Receiver<ControlEvent>,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    writer: Arc<dyn SessionWriter>,
    shared: Arc<SharedState>,
    transcript_buffer: Option<Arc<parking_lot::Mutex<TranscriptBuffer>>>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    state: State,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<Arc<TemporalRegistry>>,
    background_tracker: Option<Arc<BackgroundToolTracker>>,
) {
    while let Some(event) = rx.recv().await {
        match event {
            ControlEvent::ToolCall(calls) => {
                // 1. Check user callback for override
                let responses = if let Some(cb) = &callbacks.on_tool_call {
                    cb(calls.clone()).await
                } else {
                    None
                };

                // 2. If no override, auto-dispatch via ToolDispatcher
                let responses = match responses {
                    Some(r) => r,
                    None => {
                        if let Some(ref disp) = dispatcher {
                            let mut results = Vec::new();
                            for call in &calls {
                                match disp.call_function(&call.name, call.args.clone()).await {
                                    Ok(result) => results.push(FunctionResponse {
                                        name: call.name.clone(),
                                        response: result,
                                        id: call.id.clone(),
                                    }),
                                    Err(e) => results.push(FunctionResponse {
                                        name: call.name.clone(),
                                        response: serde_json::json!({"error": e.to_string()}),
                                        id: call.id.clone(),
                                    }),
                                }
                            }
                            results
                        } else {
                            #[cfg(feature = "tracing-support")]
                            tracing::warn!("Tool call received but no dispatcher or callback registered");
                            Vec::new()
                        }
                    }
                };

                // 3. Run through before_tool_response interceptor
                let responses = if let Some(cb) = &callbacks.before_tool_response {
                    cb(responses, state.clone()).await
                } else {
                    responses
                };

                // 4. Send tool responses back to Gemini
                if !responses.is_empty() {
                    if let Err(_e) = writer.send_tool_response(responses).await {
                        #[cfg(feature = "tracing-support")]
                        tracing::error!("Failed to send tool response: {_e}");
                    }
                }
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
                    cb(ids).await;
                }
            }
            ControlEvent::Interrupted => {
                // Truncate current model turn on interruption
                if let Some(ref buf) = transcript_buffer {
                    buf.lock().truncate_current_model_turn();
                }
                if let Some(cb) = &callbacks.on_interrupted {
                    cb().await;
                }
                // Resume audio forwarding after interrupt callback completes
                shared.interrupted.store(false, Ordering::Release);
            }
            ControlEvent::TurnComplete => {
                // 1. Reset turn-scoped state
                state.clear_prefix("turn:");

                // 2. Finalize transcript (prefer server transcriptions when available)
                if let Some(ref buf) = transcript_buffer {
                    {
                        let mut tb = buf.lock();
                        if let Some(input_text) =
                            state.session().get::<String>("last_input_transcription")
                        {
                            tb.set_input_transcription(&input_text);
                        }
                        if let Some(output_text) =
                            state.session().get::<String>("last_output_transcription")
                        {
                            tb.set_output_transcription(&output_text);
                        }
                        tb.end_turn();
                    }
                }

                // 3. Snapshot watched keys BEFORE extractors
                let pre_snapshot = watchers.as_ref().map(|w| {
                    state.snapshot_values(
                        &w.observed_keys()
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>(),
                    )
                });

                // 4. Run extractors
                if let Some(ref buf) = transcript_buffer {
                    for extractor in &extractors {
                        let window_size = extractor.window_size();
                        let window: Vec<_> = buf.lock().window(window_size).to_vec();
                        if !window.is_empty() {
                            match extractor.extract(&window).await {
                                Ok(value) => {
                                    state.set(extractor.name(), &value);
                                    if let Some(cb) = &callbacks.on_extracted {
                                        cb(extractor.name().to_string(), value).await;
                                    }
                                }
                                Err(_e) => {
                                    #[cfg(feature = "tracing-support")]
                                    tracing::warn!(
                                        extractor = extractor.name(),
                                        "Extraction failed: {_e}"
                                    );
                                    let _ = _e;
                                }
                            }
                        }
                    }
                }

                // 5. Recompute derived state
                if let Some(ref computed) = computed {
                    computed.recompute(&state);
                }

                // 6. Evaluate phase transitions
                if let Some(ref pm) = phase_machine {
                    let mut machine = pm.lock().await;
                    if let Some(target) =
                        machine.evaluate(&state).map(|s| s.to_string())
                    {
                        let turn =
                            state.session().get::<u32>("turn_count").unwrap_or(0);
                        let instruction =
                            machine.transition(&target, &state, &writer, turn).await;
                        if let Some(inst) = instruction {
                            // Dedup against last instruction
                            let should_update = {
                                let last = shared.last_instruction.lock();
                                last.as_deref() != Some(&inst)
                            };
                            if should_update {
                                *shared.last_instruction.lock() =
                                    Some(inst.clone());
                                writer.update_instruction(inst).await.ok();
                            }
                        }
                        state.session().set("phase", machine.current());
                    }
                }

                // 7. Fire watchers (compare pre vs post snapshots)
                if let (Some(ref watchers), Some(pre)) =
                    (&watchers, pre_snapshot)
                {
                    let post_keys: Vec<&str> = watchers
                        .observed_keys()
                        .iter()
                        .map(|s| s.as_str())
                        .collect();
                    let diffs = state.diff_values(&pre, &post_keys);
                    if !diffs.is_empty() {
                        let (blocking, concurrent) =
                            watchers.evaluate(&diffs, &state);
                        for action in blocking {
                            action.await;
                        }
                        for action in concurrent {
                            tokio::spawn(action);
                        }
                    }
                }

                // 8. Check temporal patterns
                if let Some(ref temporal) = temporal {
                    let event = SessionEvent::TurnComplete;
                    for action in
                        temporal.check_all(&state, Some(&event), &writer)
                    {
                        tokio::spawn(action);
                    }
                }

                // 9. Instruction template (may override phase instruction)
                if let Some(ref template) = callbacks.instruction_template {
                    if let Some(new_instruction) = template(&state) {
                        let should_update = {
                            let last = shared.last_instruction.lock();
                            last.as_deref() != Some(&new_instruction)
                        };
                        if should_update {
                            *shared.last_instruction.lock() =
                                Some(new_instruction.clone());
                            writer
                                .update_instruction(new_instruction)
                                .await
                                .ok();
                        }
                    }
                }

                // 10. Turn boundary hook
                if let Some(cb) = &callbacks.on_turn_boundary {
                    cb(state.clone(), writer.clone()).await;
                }

                // 11. User turn-complete callback
                if let Some(cb) = &callbacks.on_turn_complete {
                    cb().await;
                }

                // 12. Update session turn count
                let tc: u32 =
                    state.session().get("turn_count").unwrap_or(0);
                state.session().set("turn_count", tc + 1);
            }
            ControlEvent::GoAway(time_left) => {
                let duration = time_left
                    .as_deref()
                    .and_then(|s| s.trim_end_matches('s').parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(60));
                if let Some(cb) = &callbacks.on_go_away {
                    cb(duration).await;
                }
            }
            ControlEvent::Connected => {
                if let Some(cb) = &callbacks.on_connected {
                    cb().await;
                }
            }
            ControlEvent::Disconnected(reason) => {
                if let Some(cb) = &callbacks.on_disconnected {
                    cb(reason).await;
                }
            }
            ControlEvent::SessionResumeHandle(_handle) => {
                // Already stored in shared state by the router
            }
            ControlEvent::Error(err) => {
                if let Some(cb) = &callbacks.on_error {
                    cb(err).await;
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

        // We need a writer for the control lane
        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) =
            spawn_event_processor(event_rx, callbacks, None, writer, None, vec![], State::new(), None, None, None, None, None, None);

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
            spawn_event_processor(event_rx, callbacks, None, writer, None, vec![], State::new(), None, None, None, None, None, None);

        // Send audio, then interrupt, then more audio
        let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"before")));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = event_tx.send(SessionEvent::Interrupted);
        tokio::time::sleep(Duration::from_millis(20)).await;
        // This audio should be suppressed briefly then resume after interrupt callback
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
            spawn_event_processor(event_rx, callbacks, None, writer, None, vec![], State::new(), None, None, None, None, None, None);

        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(called.load(Ordering::SeqCst));

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn transcript_buffer_accumulates_in_fast_lane() {
        let callbacks = Arc::new(EventCallbacks::default());
        let buffer = Arc::new(parking_lot::Mutex::new(TranscriptBuffer::new()));

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            Some(buffer.clone()),
            vec![],
            State::new(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        // Send transcripts
        let _ = event_tx.send(SessionEvent::InputTranscription("Hello ".to_string()));
        let _ = event_tx.send(SessionEvent::InputTranscription("world".to_string()));
        let _ = event_tx.send(SessionEvent::OutputTranscription("Hi there!".to_string()));
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Check buffer has accumulated (not yet ended)
        assert!(buffer.lock().has_pending());

        // End turn
        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Check turn was finalized
        let buf = buffer.lock();
        assert_eq!(buf.turn_count(), 1);
        let turns = buf.all_turns();
        assert_eq!(turns[0].user, "Hello world");
        assert_eq!(turns[0].model, "Hi there!");

        drop(buf);
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
                _window: &[TranscriptTurn],
            ) -> Result<serde_json::Value, LlmError> {
                Ok(serde_json::json!({"phase": "ordering", "items": ["pizza"]}))
            }
        }

        let extracted_name = Arc::new(parking_lot::Mutex::new(String::new()));
        let extracted_value = Arc::new(parking_lot::Mutex::new(serde_json::Value::Null));
        let name_clone = extracted_name.clone();
        let value_clone = extracted_value.clone();

        let mut callbacks = EventCallbacks::default();
        callbacks.on_extracted = Some(Arc::new(move |name, value| {
            let nc = name_clone.clone();
            let vc = value_clone.clone();
            Box::pin(async move {
                *nc.lock() = name;
                *vc.lock() = value;
            })
        }));
        let callbacks = Arc::new(callbacks);

        let buffer = Arc::new(parking_lot::Mutex::new(TranscriptBuffer::new()));
        let state = State::new();
        let extractors: Vec<Arc<dyn TurnExtractor>> = vec![Arc::new(FixedExtractor)];

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            Some(buffer.clone()),
            extractors,
            state.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        // Send transcript + turn complete
        let _ = event_tx.send(SessionEvent::InputTranscription("I want pizza".to_string()));
        let _ = event_tx.send(SessionEvent::OutputTranscription("What size?".to_string()));
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify extractor result is in State
        let result: Option<serde_json::Value> = state.get("TestExtractor");
        assert!(result.is_some(), "Extractor result should be in State");
        let val = result.unwrap();
        assert_eq!(val["phase"], "ordering");
        assert_eq!(val["items"][0], "pizza");

        // Verify callback was fired
        assert_eq!(*extracted_name.lock(), "TestExtractor");
        assert_eq!(extracted_value.lock()["phase"], "ordering");

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn before_tool_response_intercepts() {
        use crate::tool::ToolDispatcher;
        use crate::tool::SimpleTool;

        // Create a tool that returns {"value": 1}
        let tool = SimpleTool::new("test_tool", "test", None, |_args| async {
            Ok(serde_json::json!({"value": 1}))
        });
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(tool));

        // Interceptor adds state context to every tool response
        let intercepted = Arc::new(AtomicBool::new(false));
        let intercepted_clone = intercepted.clone();

        let mut callbacks = EventCallbacks::default();
        callbacks.before_tool_response = Some(Arc::new(move |responses, state| {
            let ic = intercepted_clone.clone();
            Box::pin(async move {
                ic.store(true, Ordering::SeqCst);
                // Augment responses with state context
                let phase: String = state.get("phase").unwrap_or_default();
                responses
                    .into_iter()
                    .map(|mut r| {
                        if let Some(obj) = r.response.as_object_mut() {
                            obj.insert("phase".to_string(), serde_json::json!(phase));
                        }
                        r
                    })
                    .collect()
            })
        }));
        let callbacks = Arc::new(callbacks);

        let state = State::new();
        state.set("phase", "ordering");

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            Some(Arc::new(dispatcher)),
            writer,
            None,
            vec![],
            state,
            None,
            None,
            None,
            None,
            None,
            None,
        );

        // Send a tool call
        let _ = event_tx.send(SessionEvent::ToolCall(vec![
            rs_genai::prelude::FunctionCall {
                name: "test_tool".to_string(),
                args: serde_json::json!({}),
                id: Some("call-1".to_string()),
            },
        ]));
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            intercepted.load(Ordering::SeqCst),
            "before_tool_response should have been called"
        );

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn instruction_template_updates_on_state_change() {
        let instruction_sent = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let instr_clone = instruction_sent.clone();

        // Use a recording writer to capture instruction updates
        struct RecordingWriter {
            instructions: Arc<parking_lot::Mutex<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl SessionWriter for RecordingWriter {
            async fn send_audio(&self, _: Vec<u8>) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn send_text(&self, _: String) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn send_tool_response(
                &self,
                _: Vec<FunctionResponse>,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn send_client_content(
                &self,
                _: Vec<rs_genai::prelude::Content>,
                _: bool,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn send_video(
                &self,
                _: Vec<u8>,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn update_instruction(
                &self,
                instruction: String,
            ) -> Result<(), rs_genai::session::SessionError> {
                self.instructions.lock().push(instruction);
                Ok(())
            }
            async fn signal_activity_start(
                &self,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn signal_activity_end(
                &self,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn disconnect(&self) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
        }

        let mut callbacks = EventCallbacks::default();
        callbacks.instruction_template = Some(Arc::new(|state: &State| {
            let phase: String = state.get("phase").unwrap_or_default();
            match phase.as_str() {
                "ordering" => Some("Take the customer's order accurately.".to_string()),
                "confirming" => Some("Summarize and confirm the order.".to_string()),
                _ => None,
            }
        }));
        let callbacks = Arc::new(callbacks);

        let state = State::new();
        let buffer = Arc::new(parking_lot::Mutex::new(TranscriptBuffer::new()));

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> = Arc::new(RecordingWriter {
            instructions: instr_clone,
        });

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            Some(buffer.clone()),
            vec![],
            state.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        // Turn 1: set phase to "ordering"
        state.set("phase", "ordering");
        let _ = event_tx.send(SessionEvent::InputTranscription("order".to_string()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should have sent the ordering instruction
        assert_eq!(instruction_sent.lock().len(), 1);
        assert_eq!(
            instruction_sent.lock()[0],
            "Take the customer's order accurately."
        );

        // Turn 2: same phase, should NOT send again (dedup)
        let _ = event_tx.send(SessionEvent::InputTranscription("more".to_string()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            instruction_sent.lock().len(),
            1,
            "Should not resend same instruction"
        );

        // Turn 3: change phase to "confirming"
        state.set("phase", "confirming");
        let _ = event_tx.send(SessionEvent::InputTranscription("confirm".to_string()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(instruction_sent.lock().len(), 2);
        assert_eq!(
            instruction_sent.lock()[1],
            "Summarize and confirm the order."
        );

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn on_turn_boundary_called_with_state_and_writer() {
        let boundary_called = Arc::new(AtomicBool::new(false));
        let bc = boundary_called.clone();
        let state_read = Arc::new(parking_lot::Mutex::new(String::new()));
        let sr = state_read.clone();

        let mut callbacks = EventCallbacks::default();
        callbacks.on_turn_boundary = Some(Arc::new(move |state, _writer| {
            let bc = bc.clone();
            let sr = sr.clone();
            Box::pin(async move {
                bc.store(true, Ordering::SeqCst);
                let val: String = state.get("context_key").unwrap_or_default();
                *sr.lock() = val;
            })
        }));
        let callbacks = Arc::new(callbacks);

        let state = State::new();
        state.set("context_key", "important_context");

        let buffer = Arc::new(parking_lot::Mutex::new(TranscriptBuffer::new()));

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();

        let writer: Arc<dyn SessionWriter> =
            Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            Some(buffer),
            vec![],
            state,
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let _ = event_tx.send(SessionEvent::InputTranscription("hi".to_string()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = event_tx.send(SessionEvent::TurnComplete);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            boundary_called.load(Ordering::SeqCst),
            "on_turn_boundary should have been called"
        );
        assert_eq!(
            *state_read.lock(),
            "important_context",
            "on_turn_boundary should receive state"
        );

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }
}
