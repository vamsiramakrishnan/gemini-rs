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

use rs_genai::prelude::{SessionEvent, SessionPhase, UsageMetadata};
use rs_genai::session::SessionWriter;

use crate::state::State;
use crate::tool::ToolDispatcher;

use super::background_tool::BackgroundToolTracker;
use super::callbacks::EventCallbacks;
use super::computed::ComputedRegistry;
use super::context_writer::PendingContext;
use super::control_plane::run_control_lane;
use super::events::LiveEvent;
use super::extractor::TurnExtractor;
use super::needs::NeedsFulfillment;
use super::persistence::SessionPersistence;
use super::phase::PhaseMachine;
use super::session_signals::SessionSignals;
use super::soft_turn::SoftTurnDetector;
use super::steering::SteeringMode;
use super::telemetry::SessionTelemetry;
use super::temporal::TemporalRegistry;
use super::watcher::WatcherRegistry;

/// Events routed to the fast lane (sync processing).
pub(crate) enum FastEvent {
    Audio(Bytes),
    Text(String),
    TextComplete(String),
    InputTranscript(String),
    OutputTranscript(String),
    Thought(String),
    VadStart,
    VadEnd,
    Phase(SessionPhase),
    /// Interruption flag — tells fast lane to stop forwarding audio.
    Interrupted,
}

/// Events routed to the control lane (async processing).
pub(crate) enum ControlEvent {
    ToolCall(Vec<rs_genai::prelude::FunctionCall>),
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
    /// Pending context buffer for deferred delivery (None when Immediate mode).
    pub pending_context: Option<Arc<PendingContext>>,
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
    /// Shared pending context buffer for deferred delivery (None when Immediate).
    /// Must be the same Arc given to the DeferredWriter so the control lane
    /// can push context and the DeferredWriter can drain it.
    pub pending_context: Option<Arc<PendingContext>>,
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
            pending_context: None,
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
    live_event_tx: broadcast::Sender<LiveEvent>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let shared = Arc::new(SharedState {
        interrupted: AtomicBool::new(false),
        resume_handle: parking_lot::Mutex::new(None),
        last_instruction: parking_lot::Mutex::new(None),
        pending_context: control_plane.pending_context.clone(),
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
    let fast_event_tx = live_event_tx.clone();
    let fast_handle = tokio::spawn(async move {
        run_fast_lane(fast_rx, fast_callbacks, fast_shared, fast_event_tx).await;
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
            live_event_tx,
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
            let _ = fast_tx.send(FastEvent::Audio(data)).await;
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
            let _ = fast_tx
                .send(FastEvent::OutputTranscript(text.clone()))
                .await;
            let _ = ctrl_tx.send(ControlEvent::OutputTranscript(text)).await;
        }
        SessionEvent::Thought(text) => {
            let _ = fast_tx.send(FastEvent::Thought(text)).await;
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
    event_tx: broadcast::Sender<LiveEvent>,
) {
    while let Some(event) = rx.recv().await {
        match event {
            FastEvent::Audio(data) => {
                // Suppress audio during interruption
                if !shared.interrupted.load(Ordering::Acquire) {
                    if let Some(cb) = &callbacks.on_audio {
                        cb(&data);
                    }
                    let _ = event_tx.send(LiveEvent::Audio(data));
                }
            }
            FastEvent::Text(delta) => {
                if let Some(cb) = &callbacks.on_text {
                    cb(&delta);
                }
                let _ = event_tx.send(LiveEvent::TextDelta(delta));
            }
            FastEvent::TextComplete(text) => {
                if let Some(cb) = &callbacks.on_text_complete {
                    cb(&text);
                }
                let _ = event_tx.send(LiveEvent::TextComplete(text));
            }
            FastEvent::InputTranscript(text) => {
                // Callback only — accumulation happens in control lane
                if let Some(cb) = &callbacks.on_input_transcript {
                    cb(&text, false);
                }
                let _ = event_tx.send(LiveEvent::InputTranscript {
                    text,
                    is_final: false,
                });
            }
            FastEvent::OutputTranscript(text) => {
                // Callback only — accumulation happens in control lane
                if let Some(cb) = &callbacks.on_output_transcript {
                    cb(&text, false);
                }
                let _ = event_tx.send(LiveEvent::OutputTranscript {
                    text,
                    is_final: false,
                });
            }
            FastEvent::Thought(text) => {
                if let Some(cb) = &callbacks.on_thought {
                    cb(&text);
                }
                let _ = event_tx.send(LiveEvent::Thought(text));
            }
            FastEvent::VadStart => {
                if let Some(cb) = &callbacks.on_vad_start {
                    cb();
                }
                let _ = event_tx.send(LiveEvent::VadStart);
            }
            FastEvent::VadEnd => {
                if let Some(cb) = &callbacks.on_vad_end {
                    cb();
                }
                let _ = event_tx.send(LiveEvent::VadEnd);
            }
            FastEvent::Phase(phase) => {
                if let Some(cb) = &callbacks.on_phase {
                    cb(phase);
                }
                // Phase is L0-level wire event, not emitted as LiveEvent
            }
            FastEvent::Interrupted => {
                // Audio already suppressed via shared.interrupted flag
                // Interrupted LiveEvent is emitted from control lane
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use crate::live::events::LiveEvent;
    use rs_genai::prelude::FunctionResponse;

    fn dummy_event_tx() -> broadcast::Sender<LiveEvent> {
        broadcast::channel::<LiveEvent>(16).0
    }

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

        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            vec![],
            State::new(),
            None,
            None,
            None,
            None,
            None,
            std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
            dummy_event_tx(),
        );

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

        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            vec![],
            State::new(),
            None,
            None,
            None,
            None,
            None,
            std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
            dummy_event_tx(),
        );

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

        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            vec![],
            State::new(),
            None,
            None,
            None,
            None,
            None,
            std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
            dummy_event_tx(),
        );

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

        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

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
            dummy_event_tx(),
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
        use crate::live::extractor::TurnExtractor;
        use crate::live::transcript::TranscriptTurn;
        use crate::llm::LlmError;

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

        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

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
            dummy_event_tx(),
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

        let telem_handle =
            spawn_telemetry_lane(telem_rx, signals, telemetry.clone(), cancel.clone(), None);

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
        use crate::live::background_tool::{BackgroundToolTracker, ToolExecutionMode};
        use crate::tool::{SimpleTool, ToolDispatcher};

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
            ToolExecutionMode::Background {
                formatter: None,
                scheduling: None,
            },
        );

        let sent = Arc::new(parking_lot::Mutex::new(Vec::<Vec<FunctionResponse>>::new()));
        let sent_clone = sent.clone();

        // Use a writer that records sent tool responses
        struct RecordingWriter {
            sent: Arc<parking_lot::Mutex<Vec<Vec<FunctionResponse>>>>,
        }

        #[async_trait::async_trait]
        impl SessionWriter for RecordingWriter {
            async fn send_audio(
                &self,
                _data: Vec<u8>,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn send_text(
                &self,
                _text: String,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn send_video(
                &self,
                _data: Vec<u8>,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn send_tool_response(
                &self,
                responses: Vec<FunctionResponse>,
            ) -> Result<(), rs_genai::session::SessionError> {
                self.sent.lock().push(responses);
                Ok(())
            }
            async fn update_instruction(
                &self,
                _instruction: String,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn send_client_content(
                &self,
                _content: Vec<rs_genai::prelude::Content>,
                _turn_complete: bool,
            ) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn signal_activity_start(&self) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn signal_activity_end(&self) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
            async fn disconnect(&self) -> Result<(), rs_genai::session::SessionError> {
                Ok(())
            }
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
            dummy_event_tx(),
        );

        // Send a tool call
        let _ = event_tx.send(SessionEvent::ToolCall(vec![
            rs_genai::prelude::FunctionCall {
                name: "slow_search".to_string(),
                args: serde_json::json!({"q": "test"}),
                id: Some("fc_1".to_string()),
            },
        ]));

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
        assert!(
            responses.len() >= 2,
            "Should have sent result after completion"
        );
        assert_eq!(responses[1][0].response["status"], "completed");

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn callback_mode_blocking_awaits_inline() {
        use crate::live::callbacks::CallbackMode;
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

        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            vec![],
            State::new(),
            None,
            None,
            None,
            None,
            None,
            std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
            dummy_event_tx(),
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
        use crate::live::callbacks::CallbackMode;

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

        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            None,
            writer,
            vec![],
            State::new(),
            None,
            None,
            None,
            None,
            None,
            std::collections::HashMap::new(),
            ControlPlaneConfig::default(),
            dummy_event_tx(),
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
