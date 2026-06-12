//! Three-lane event processor for Live sessions.
//!
//! **Fast lane**: audio, text, VAD (sync callbacks, never blocks)
//! **Control lane**: tool calls, interruptions, lifecycle, transcript accumulation,
//!   extractors, phases, watchers (async callbacks, can block)
//! **Telemetry lane**: SessionSignals + SessionTelemetry (debounced state writes,
//!   runs on its own broadcast receiver — zero work on the router hot path)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use gemini_genai_rs::prelude::{SessionEvent, SessionPhase};
use gemini_genai_rs::session::SessionWriter;

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
use super::steering::{ContextDelivery, SteeringMode};
use super::telemetry::SessionTelemetry;
use super::temporal::TemporalRegistry;
use super::watcher::WatcherRegistry;

/// Backpressure (delivery) policy for a single class of fast-lane events.
///
/// The event router forwards fast-lane frames (audio, text, transcripts,
/// thoughts, VAD, phase) over a bounded channel to the fast-lane consumer. When
/// that consumer falls behind and the channel fills, the policy decides what the
/// router does — and crucially, whether the router *blocks*. Because the router
/// is shared by both the fast lane and the control lane, a blocking fast-lane
/// send stalls routing for *all* events, including control-lane lifecycle and
/// tool events. The policy lets callers trade frame durability for router
/// responsiveness on a per-class basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delivery {
    /// Never drop a frame: `tx.send(ev).await` — the router awaits when the
    /// channel is full. This is the historical (and default) behavior and is
    /// byte-for-byte identical to the pre-policy code path. Use it when every
    /// frame matters and a slow consumer applying backpressure to the router is
    /// acceptable.
    #[default]
    Lossless,
    /// Drop the *newest* frame on overflow: `tx.try_send(ev)` and, on
    /// [`TrySendError::Full`](tokio::sync::mpsc::error::TrySendError::Full),
    /// discard the just-produced frame and bump a dropped-frame counter. The
    /// router never blocks on this class, so a slow fast-lane consumer can no
    /// longer stall control-lane routing. Use it for high-frequency, loss-
    /// tolerant streams (e.g. partial transcripts, thoughts) where freshness of
    /// already-queued frames matters less than keeping the router moving.
    ///
    /// A drop-oldest / latest-only variant is intentionally *not* provided:
    /// tokio's `mpsc` has no clean "evict the oldest queued item" primitive, so
    /// implementing it correctly would require a custom ring buffer. That is
    /// left as future work rather than shipped half-working.
    LossyDropNewest,
}

/// Per-event-class delivery (backpressure) policy for the fast lane.
///
/// Each fast-lane event class carries its own [`Delivery`] policy. The
/// [`Default`] impl sets **every** class to [`Delivery::Lossless`], which makes
/// the whole feature behavior-preserving: with the default config the router
/// uses the same `send().await` path it always has. Callers opt into lossy
/// behavior per class via the builder setters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryConfig {
    /// Policy for raw PCM audio frames.
    pub audio: Delivery,
    /// Policy for incremental text deltas (and text-complete frames).
    pub text: Delivery,
    /// Policy for input/output transcript frames (fast-lane callback copy only;
    /// control-lane accumulation is unaffected and always lossless).
    pub transcript: Delivery,
    /// Policy for thought-summary frames.
    pub thought: Delivery,
    /// Policy for VAD start/end frames.
    pub vad: Delivery,
    /// Policy for phase-changed frames.
    pub phase: Delivery,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            audio: Delivery::Lossless,
            text: Delivery::Lossless,
            transcript: Delivery::Lossless,
            thought: Delivery::Lossless,
            vad: Delivery::Lossless,
            phase: Delivery::Lossless,
        }
    }
}

impl DeliveryConfig {
    /// A config with every class set to [`Delivery::Lossless`] (same as
    /// [`Default`]).
    pub fn lossless() -> Self {
        Self::default()
    }

    /// Set the audio policy.
    pub fn audio(mut self, d: Delivery) -> Self {
        self.audio = d;
        self
    }

    /// Set the text policy.
    pub fn text(mut self, d: Delivery) -> Self {
        self.text = d;
        self
    }

    /// Set the transcript policy.
    pub fn transcript(mut self, d: Delivery) -> Self {
        self.transcript = d;
        self
    }

    /// Set the thought policy.
    pub fn thought(mut self, d: Delivery) -> Self {
        self.thought = d;
        self
    }

    /// Set the VAD policy.
    pub fn vad(mut self, d: Delivery) -> Self {
        self.vad = d;
        self
    }

    /// Set the phase policy.
    pub fn phase(mut self, d: Delivery) -> Self {
        self.phase = d;
        self
    }
}

/// Per-class counters for fast-lane frames dropped under a lossy policy.
///
/// Incremented with a single relaxed atomic add on the router hot path when a
/// [`Delivery::LossyDropNewest`] send overflows. Reads are for observability /
/// tests and never gate the hot path.
#[derive(Debug, Default)]
pub(crate) struct DroppedFrames {
    pub audio: AtomicU64,
    pub text: AtomicU64,
    pub transcript: AtomicU64,
    pub thought: AtomicU64,
    pub vad: AtomicU64,
    pub phase: AtomicU64,
}

impl DroppedFrames {
    /// Total dropped frames across all classes.
    ///
    /// Currently only consumed by tests; the per-class atomics are read
    /// directly elsewhere. Kept test-gated until a handle accessor surfaces it.
    #[cfg(test)]
    pub fn total(&self) -> u64 {
        self.audio.load(Ordering::Relaxed)
            + self.text.load(Ordering::Relaxed)
            + self.transcript.load(Ordering::Relaxed)
            + self.thought.load(Ordering::Relaxed)
            + self.vad.load(Ordering::Relaxed)
            + self.phase.load(Ordering::Relaxed)
    }
}

/// Forward one fast-lane frame according to its class delivery policy.
///
/// - [`Delivery::Lossless`]: `tx.send(ev).await` — awaits when the channel is
///   full (identical to the pre-policy behavior).
/// - [`Delivery::LossyDropNewest`]: `tx.try_send(ev)` — on a full channel, drop
///   the frame and increment `dropped`.
///
/// Returns without ever blocking the router under a lossy policy.
async fn deliver_fast(
    tx: &mpsc::Sender<FastEvent>,
    ev: FastEvent,
    policy: Delivery,
    dropped: &AtomicU64,
) {
    match policy {
        Delivery::Lossless => {
            let _ = tx.send(ev).await;
        }
        Delivery::LossyDropNewest => {
            if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(ev) {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
            // `TrySendError::Closed` is ignored, matching the `let _ = send`
            // pattern used elsewhere (the consumer is gone; nothing to do).
        }
    }
}

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
    ToolCall(Vec<gemini_genai_rs::prelude::FunctionCall>),
    ToolCallCancelled(Vec<String>),
    /// A background tool finished. Posted by the detached background task (which
    /// can't reach the synchronous `FlowMonitor`) so the control lane can advance
    /// the governed flow through the same gate as inline tools (#7).
    ToolCompleted {
        /// The tool call's correlation id (for once-per-call_id flow dedup).
        call_id: String,
        /// The tool name (matches `FunctionCall::name`).
        name: String,
        /// Whether the tool completed successfully.
        ok: bool,
    },
    Interrupted,
    TurnComplete,
    /// Model finished generating (even if interrupted). Fires before TurnComplete.
    GenerationComplete,
    GoAway(Option<String>),
    Connected,
    Disconnected(Option<String>),
    SessionResumeUpdate(gemini_genai_rs::session::ResumeInfo),
    Error(String),
    /// Transcript accumulation — pushed from router, exclusive to control lane.
    InputTranscript(String),
    OutputTranscript(String),
}

/// Shared state between the two lanes.
pub(crate) struct SharedState {
    /// When true, fast lane suppresses audio callbacks.
    pub interrupted: AtomicBool,
    /// Barge-in signal for in-flight inline tool dispatch.
    ///
    /// Cancelled by the ROUTER the moment an `Interrupted` event arrives,
    /// then re-armed (replaced with a fresh token) by the control lane once
    /// it has processed the interruption. The control lane races inline tool
    /// dispatch against this token, so a user barge-in is never stuck waiting
    /// behind a slow tool.
    pub barge_in: parking_lot::Mutex<CancellationToken>,
    /// Latest resume handle from server.
    pub resume_handle: parking_lot::Mutex<Option<String>>,
    /// Last instruction sent via instruction_template (for dedup).
    pub last_instruction: parking_lot::Mutex<Option<String>>,
    /// Pending context buffer for deferred delivery (None when Immediate mode).
    pub pending_context: Option<Arc<PendingContext>>,
    /// Fast-lane delivery policy per event class.
    pub delivery: DeliveryConfig,
    /// Per-class counters for frames dropped under a lossy delivery policy.
    pub dropped: DroppedFrames,
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
    /// When to deliver context turns to the wire.
    /// Deferred = synchronize with user activity (speech, interruption);
    /// Immediate = send during TurnComplete processing.
    pub context_delivery: ContextDelivery,
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
    /// Middleware layers run around tool dispatch in the control lane
    /// (`before_tool` / `after_tool` / `on_tool_error`).
    pub middleware: Arc<crate::middleware::MiddlewareChain>,
    /// Optional governed-flow monitor: gates tool calls, projects active-step
    /// postures into steering, and drives repair from unmet requirements.
    /// Shared (`Arc<Mutex<..>>`) so the [`LiveHandle`](super::handle::LiveHandle)
    /// can snapshot `explain`/`why_blocked` while the control lane advances it.
    /// Lock briefly; never hold the guard across an `await`.
    pub flow: Option<crate::flow::SharedFlowMonitor>,
    /// Fast-lane delivery (backpressure) policy per event class. Defaults to
    /// all-`Lossless`, preserving the historical `send().await` behavior.
    pub delivery: DeliveryConfig,
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self {
            soft_turn: None,
            steering_mode: SteeringMode::default(),
            context_delivery: ContextDelivery::default(),
            needs_fulfillment: None,
            persistence: None,
            session_id: None,
            tool_advisory: true,
            pending_context: None,
            middleware: Arc::new(crate::middleware::MiddlewareChain::new()),
            flow: None,
            delivery: DeliveryConfig::default(),
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "lane spawn site: parameters are the owned subsystem handles split between the fast and control lanes"
)]
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
        barge_in: parking_lot::Mutex::new(CancellationToken::new()),
        resume_handle: parking_lot::Mutex::new(None),
        last_instruction: parking_lot::Mutex::new(None),
        pending_context: control_plane.pending_context.clone(),
        delivery: control_plane.delivery,
        dropped: DroppedFrames::default(),
    });

    let timer_cancel = CancellationToken::new();

    // Channels between router and lanes.
    //
    // The control channel matches the fast channel at 512: control events
    // are routed with a lossless `send().await`, so a *full* control queue
    // blocks the shared router — and a blocked router stops forwarding audio
    // frames too, causing playback glitches. Transcript accumulation events
    // (one per ASR chunk) flow through this channel, so a slow control-lane
    // consumer (e.g. a blocking turn-complete pipeline) could realistically
    // fill 64 slots; 512 gives the lane room to fall behind transiently
    // without starving the fast lane.
    let (fast_tx, fast_rx) = mpsc::channel::<FastEvent>(512);
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlEvent>(512);

    // Spawn the router task (reads broadcast, routes to lanes)
    // NOTE: SessionSignals is NOT called here — it runs on the telemetry lane.
    let fast_tx_clone = fast_tx.clone();
    let ctrl_tx_clone = ctrl_tx.clone();
    let shared_clone = shared.clone();
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    // `Disconnected` is terminal in L0 (the session loop returns
                    // after emitting it), so the router exits after routing it.
                    // Dropping the router's lane senders closes the fast/control
                    // channels, letting both lanes drain their queues and shut
                    // down gracefully (final persistence drain, etc.) instead of
                    // idling forever on a broadcast channel that never closes
                    // while the `SessionHandle` is alive.
                    let terminal = matches!(event, SessionEvent::Disconnected(_));
                    route_event(event, &fast_tx_clone, &ctrl_tx_clone, &shared_clone).await;
                    if terminal {
                        break;
                    }
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
    // Weak sender handed to the control lane so background tool tasks can post
    // completions back without keeping the channel open on shutdown (the lane
    // upgrades it per background spawn; the channel closes once the router and
    // all in-flight background tasks drop their strong senders).
    let ctrl_tx_weak = ctrl_tx.downgrade();
    let ctrl_handle = tokio::spawn(async move {
        run_control_lane(
            ctrl_rx,
            ctrl_tx_weak,
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
    on_usage: Option<super::callbacks::UsageCallback>,
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
                                SessionEvent::TextDelta(_) => {
                                    telemetry.record_text_out();
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
    let delivery = &shared.delivery;
    let dropped = &shared.dropped;
    match event {
        // Fast lane events
        SessionEvent::AudioData(data) => {
            deliver_fast(
                fast_tx,
                FastEvent::Audio(data),
                delivery.audio,
                &dropped.audio,
            )
            .await;
        }
        SessionEvent::TextDelta(text) => {
            deliver_fast(fast_tx, FastEvent::Text(text), delivery.text, &dropped.text).await;
        }
        SessionEvent::TextComplete(text) => {
            deliver_fast(
                fast_tx,
                FastEvent::TextComplete(text),
                delivery.text,
                &dropped.text,
            )
            .await;
        }
        // Transcripts: fast lane for callbacks, control lane for accumulation.
        // The control-lane accumulation send keeps its lossless `send().await`.
        SessionEvent::InputTranscription(text) => {
            deliver_fast(
                fast_tx,
                FastEvent::InputTranscript(text.clone()),
                delivery.transcript,
                &dropped.transcript,
            )
            .await;
            let _ = ctrl_tx.send(ControlEvent::InputTranscript(text)).await;
        }
        SessionEvent::OutputTranscription(text) => {
            deliver_fast(
                fast_tx,
                FastEvent::OutputTranscript(text.clone()),
                delivery.transcript,
                &dropped.transcript,
            )
            .await;
            let _ = ctrl_tx.send(ControlEvent::OutputTranscript(text)).await;
        }
        SessionEvent::Thought(text) => {
            deliver_fast(
                fast_tx,
                FastEvent::Thought(text),
                delivery.thought,
                &dropped.thought,
            )
            .await;
        }
        SessionEvent::VoiceActivityStart => {
            deliver_fast(fast_tx, FastEvent::VadStart, delivery.vad, &dropped.vad).await;
        }
        SessionEvent::VoiceActivityEnd => {
            deliver_fast(fast_tx, FastEvent::VadEnd, delivery.vad, &dropped.vad).await;
        }
        SessionEvent::PhaseChanged(phase) => {
            deliver_fast(
                fast_tx,
                FastEvent::Phase(phase),
                delivery.phase,
                &dropped.phase,
            )
            .await;
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
            // Cancel any in-flight inline tool dispatch immediately: the
            // control lane may be blocked awaiting a slow tool and would
            // otherwise not see this interruption until the tool finished.
            shared.barge_in.lock().cancel();
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
    use gemini_genai_rs::prelude::FunctionResponse;

    fn dummy_event_tx() -> broadcast::Sender<LiveEvent> {
        broadcast::channel::<LiveEvent>(16).0
    }

    #[test]
    fn delivery_config_default_is_all_lossless() {
        let cfg = DeliveryConfig::default();
        assert_eq!(cfg.audio, Delivery::Lossless);
        assert_eq!(cfg.text, Delivery::Lossless);
        assert_eq!(cfg.transcript, Delivery::Lossless);
        assert_eq!(cfg.thought, Delivery::Lossless);
        assert_eq!(cfg.vad, Delivery::Lossless);
        assert_eq!(cfg.phase, Delivery::Lossless);
        // The standalone Delivery default must also be Lossless.
        assert_eq!(Delivery::default(), Delivery::Lossless);
    }

    #[tokio::test]
    async fn lossy_drop_newest_does_not_block_and_counts_drops() {
        // Capacity-1 channel that we fill, so the next send would block under
        // Lossless. The receiver is held but never drains.
        let (tx, _rx) = mpsc::channel::<FastEvent>(1);
        tx.send(FastEvent::VadStart).await.unwrap(); // channel now full
        let dropped = AtomicU64::new(0);

        // Under LossyDropNewest this must return immediately (not block) and
        // bump the counter. We bound it with a timeout to prove non-blocking.
        let res = tokio::time::timeout(
            Duration::from_millis(100),
            deliver_fast(&tx, FastEvent::VadEnd, Delivery::LossyDropNewest, &dropped),
        )
        .await;
        assert!(res.is_ok(), "deliver_fast blocked under LossyDropNewest");
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn lossless_delivers_on_non_full_channel() {
        let (tx, mut rx) = mpsc::channel::<FastEvent>(4);
        let dropped = AtomicU64::new(0);

        deliver_fast(
            &tx,
            FastEvent::Text("hello".into()),
            Delivery::Lossless,
            &dropped,
        )
        .await;

        // No drop, and the value arrives on the receiver.
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        match rx.recv().await {
            Some(FastEvent::Text(s)) => assert_eq!(s, "hello"),
            other => panic!("expected Text frame, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn dropped_frames_total_sums_classes() {
        let d = DroppedFrames::default();
        d.audio.fetch_add(2, Ordering::Relaxed);
        d.transcript.fetch_add(3, Ordering::Relaxed);
        assert_eq!(d.total(), 5);
    }

    #[tokio::test]
    async fn fast_lane_routes_audio() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let callbacks = EventCallbacks {
            on_audio: Some(Box::new(move |_| {
                count_clone.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        };
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

        let callbacks = EventCallbacks {
            on_audio: Some(Box::new(move |_| {
                count_clone.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        };
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

        let callbacks = EventCallbacks {
            on_turn_complete: Some(Arc::new(move || {
                let c = called_clone.clone();
                Box::pin(async move {
                    c.store(true, Ordering::SeqCst);
                })
            })),
            ..Default::default()
        };
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
            ) -> Result<(), gemini_genai_rs::session::SessionError> {
                Ok(())
            }
            async fn send_text(
                &self,
                _text: String,
            ) -> Result<(), gemini_genai_rs::session::SessionError> {
                Ok(())
            }
            async fn send_video(
                &self,
                _data: Vec<u8>,
            ) -> Result<(), gemini_genai_rs::session::SessionError> {
                Ok(())
            }
            async fn send_tool_response(
                &self,
                responses: Vec<FunctionResponse>,
            ) -> Result<(), gemini_genai_rs::session::SessionError> {
                self.sent.lock().push(responses);
                Ok(())
            }
            async fn update_instruction(
                &self,
                _instruction: String,
            ) -> Result<(), gemini_genai_rs::session::SessionError> {
                Ok(())
            }
            async fn send_client_content(
                &self,
                _content: Vec<gemini_genai_rs::prelude::Content>,
                _turn_complete: bool,
            ) -> Result<(), gemini_genai_rs::session::SessionError> {
                Ok(())
            }
            async fn signal_activity_start(
                &self,
            ) -> Result<(), gemini_genai_rs::session::SessionError> {
                Ok(())
            }
            async fn signal_activity_end(
                &self,
            ) -> Result<(), gemini_genai_rs::session::SessionError> {
                Ok(())
            }
            async fn disconnect(&self) -> Result<(), gemini_genai_rs::session::SessionError> {
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
            gemini_genai_rs::prelude::FunctionCall {
                name: "slow_search".to_string(),
                args: serde_json::json!({"q": "test"}),
                id: Some("fc_1".to_string()),
            },
        ]));

        // Wait just enough for the ack (but not the full tool)
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Scope the guard so it is never held across an await point.
        {
            let responses = sent.lock();
            // First batch should be the ack
            assert!(!responses.is_empty(), "Should have sent ack immediately");
            assert_eq!(responses[0][0].response["status"], "running");
        }

        // Wait for background tool to complete
        tokio::time::sleep(Duration::from_millis(300)).await;

        {
            let responses = sent.lock();
            // Second batch should be the completed result
            assert!(
                responses.len() >= 2,
                "Should have sent result after completion"
            );
            assert_eq!(responses[1][0].response["status"], "completed");
        }

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

        let callbacks = EventCallbacks {
            // Blocking on_turn_complete sets order to 1
            on_turn_complete: Some(Arc::new(move || {
                let o = order_clone.clone();
                Box::pin(async move {
                    // Simulate brief work
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    o.store(1, Ordering::SeqCst);
                })
            })),
            on_turn_complete_mode: CallbackMode::Blocking,
            ..Default::default()
        };
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
    async fn interruption_beats_slow_inline_tool() {
        use crate::tool::{SimpleTool, ToolDispatcher};

        // A slow inline tool that blocks the control lane for 5s.
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register(SimpleTool::new("slow", "slow", None, |_args| async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(serde_json::json!({"done": true}))
        }));

        let interrupted_at = Arc::new(parking_lot::Mutex::new(None::<std::time::Instant>));
        let flag = interrupted_at.clone();
        let callbacks = EventCallbacks {
            on_interrupted: Some(Arc::new(move || {
                let flag = flag.clone();
                Box::pin(async move {
                    *flag.lock() = Some(std::time::Instant::now());
                })
            })),
            ..Default::default()
        };

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();
        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            Arc::new(callbacks),
            Some(Arc::new(dispatcher)),
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

        // Tool call starts the 5s dispatch, then the user barges in.
        let start = std::time::Instant::now();
        let _ = event_tx.send(SessionEvent::ToolCall(vec![
            gemini_genai_rs::prelude::FunctionCall {
                name: "slow".to_string(),
                args: serde_json::json!({}),
                id: Some("fc_slow".to_string()),
            },
        ]));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = event_tx.send(SessionEvent::Interrupted);

        // The interruption must be processed long before the tool's 5s —
        // before the fix it queued behind the blocking dispatch.
        let mut waited = Duration::ZERO;
        while interrupted_at.lock().is_none() && waited < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(25)).await;
            waited += Duration::from_millis(25);
        }
        let fired = (*interrupted_at.lock()).expect("on_interrupted must fire");
        assert!(
            fired.duration_since(start) < Duration::from_secs(2),
            "interruption must not wait for the slow tool"
        );

        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;
    }

    #[tokio::test]
    async fn control_lane_exit_persists_final_snapshot_synchronously() {
        use crate::live::persistence::{MemoryPersistence, SessionPersistence};

        let persistence = Arc::new(MemoryPersistence::new());
        let control_plane = ControlPlaneConfig {
            persistence: Some(persistence.clone()),
            session_id: Some("final-drain".to_string()),
            ..Default::default()
        };

        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();
        let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            Arc::new(EventCallbacks::default()),
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
            control_plane,
            dummy_event_tx(),
        );

        // Accumulate state mid-turn — but never reach a TurnComplete, so the
        // per-turn (spawn-and-forget) save never fires.
        let _ = event_tx.send(SessionEvent::InputTranscription("last words".to_string()));
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Session ends. The control lane must run a final synchronous save on
        // exit; before the fix nothing was ever persisted in this scenario.
        drop(event_tx);
        let _ = fast_handle.await;
        let _ = ctrl_handle.await;

        let snap = persistence
            .load("final-drain")
            .await
            .unwrap()
            .expect("control-lane exit must persist a final snapshot");
        assert_eq!(snap.turn_count, 0);
    }

    #[tokio::test]
    async fn lanes_exit_after_terminal_disconnected_event() {
        // The Disconnected event is terminal in L0; the router must exit after
        // routing it (dropping its lane senders) so the lanes can drain and
        // shut down gracefully — even though the broadcast sender stays alive
        // for the LiveHandle's lifetime.
        let callbacks = Arc::new(EventCallbacks::default());
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

        let _ = event_tx.send(SessionEvent::Disconnected(None));

        // NOTE: event_tx is intentionally kept alive — before the fix the
        // router only exited on channel close, and both awaits below hung.
        let joined = tokio::time::timeout(Duration::from_secs(2), async {
            let _ = fast_handle.await;
            let _ = ctrl_handle.await;
        })
        .await;
        assert!(
            joined.is_ok(),
            "lanes must exit after the terminal Disconnected event"
        );
        drop(event_tx);
    }

    #[tokio::test]
    async fn callback_mode_concurrent_spawns_task() {
        use crate::live::callbacks::CallbackMode;

        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let callbacks = EventCallbacks {
            on_turn_complete: Some(Arc::new(move || {
                let c = called_clone.clone();
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    c.store(true, Ordering::SeqCst);
                })
            })),
            on_turn_complete_mode: CallbackMode::Concurrent,
            ..Default::default()
        };
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
