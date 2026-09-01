//! LiveHandle — runtime interaction with a Live session.

use std::sync::Arc;

use gemini_genai_rs::prelude::{FunctionResponse, SessionEvent, SessionPhase, VadEvent};
use gemini_genai_rs::session::{SessionError, SessionHandle, SessionWriter};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::flow::{FlowExplanation, SharedFlowMonitor};
use crate::state::State;

use super::background_tool::BackgroundToolTracker;
use super::context_writer::PendingContext;
use super::effect_executor::LiveEffectExecutor;
use super::input_vad::{
    ActivityAuthority, BackendInputVad, BackendVadSnapshot, InputAudioProcessor,
};
use super::processor::ControlEvent;
use super::reactor::{LiveReactor, ReactorEvent, VoiceRuntimeState};
use super::telemetry::SessionTelemetry;
use super::turn_commit::TurnSignal;

/// Handle for interacting with a running Live session.
///
/// Provides send methods for audio/text/video, system instruction updates,
/// event subscription, state access, telemetry, and graceful shutdown.
///
/// When [`ContextDelivery::Deferred`](super::steering::ContextDelivery::Deferred) is
/// enabled, `send_audio`, `send_text`, and `send_video` automatically flush
/// any pending context turns before forwarding the user content.
#[derive(Clone)]
pub struct LiveHandle {
    session: SessionHandle,
    /// Writer used for user-facing sends.  When deferred context delivery is
    /// enabled, this is a `DeferredWriter` that flushes pending context.
    /// Otherwise it's the raw `SessionHandle`.
    writer: Arc<dyn SessionWriter>,
    /// Fast-lane task. Held in `Arc<Mutex<Option<..>>>` so `LiveHandle` stays
    /// `Clone` while [`disconnect`](Self::disconnect) can take ownership to
    /// grace-await and then abort the lane.
    fast_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Control-lane task (same ownership scheme as `fast_task`).
    ctrl_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Cancellation token for the telemetry lane, cancelled on disconnect.
    telem_cancel: CancellationToken,
    state: State,
    telemetry: Arc<SessionTelemetry>,
    event_tx: broadcast::Sender<super::events::LiveEvent>,
    pending_context: Option<Arc<PendingContext>>,
    reactor: Arc<LiveReactor>,
    effect_executor: LiveEffectExecutor,
    input_vad: Arc<Mutex<BackendInputVad>>,
    /// Mic-chain processors run over outgoing audio inside `send_audio`.
    input_processors: Arc<Mutex<Vec<Box<dyn InputAudioProcessor>>>>,
    /// Whether this client's VAD sends activityStart/activityEnd marks.
    client_activity_authority: Arc<std::sync::atomic::AtomicBool>,
    /// Turn-commit policy between VAD edges and activity marks (None = raw
    /// edge forwarding). See [`set_turn_commit`](Self::set_turn_commit).
    turn_commit: Arc<Mutex<Option<super::turn_commit::TurnCommitPolicy>>>,
    /// Monotonic audio clock in milliseconds, advanced by each chunk's
    /// duration — the policy's time base (deterministic, not wall time).
    audio_clock_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Governed-flow monitor shared with the control lane (None when the
    /// session is not governed by a flow).
    flow: Option<SharedFlowMonitor>,
    /// Tracker for in-flight background tool tasks. Shared with the control
    /// lane (which spawns/cancels per-call tasks) so [`disconnect`](Self::disconnect)
    /// can cancel every outstanding background tool — otherwise orphaned tasks
    /// could keep running and post stale `ToolCompleted` events after shutdown.
    background_tracker: Arc<BackgroundToolTracker>,
    /// Control-lane sender used by [`send_text`](Self::send_text) to record the
    /// typed turn on the transcript, so a text-driven session is visible to
    /// extractors exactly as a spoken one is.
    ///
    /// Deliberately a [`WeakSender`](mpsc::WeakSender): the control channel must
    /// still close once the router drops its strong sender, or the lane would
    /// never drain and shut down.
    ctrl_tx: Option<mpsc::WeakSender<ControlEvent>>,
}

impl LiveHandle {
    #[allow(
        clippy::too_many_arguments,
        reason = "crate-internal constructor called once from spawn_lanes; the runtime parts are deliberately enumerated rather than re-bundled"
    )]
    pub(crate) fn new(
        session: SessionHandle,
        writer: Arc<dyn SessionWriter>,
        fast_task: JoinHandle<()>,
        ctrl_task: JoinHandle<()>,
        state: State,
        telemetry: Arc<SessionTelemetry>,
        event_tx: broadcast::Sender<super::events::LiveEvent>,
        pending_context: Option<Arc<PendingContext>>,
        flow: Option<SharedFlowMonitor>,
        background_tracker: Arc<BackgroundToolTracker>,
        telem_cancel: CancellationToken,
    ) -> Self {
        let reactor = Arc::new(LiveReactor::voice_defaults());
        let effect_executor = LiveEffectExecutor::new(
            Arc::new(session.clone()),
            pending_context.clone(),
            event_tx.clone(),
        );

        Self {
            session,
            writer,
            fast_task: Arc::new(Mutex::new(Some(fast_task))),
            ctrl_task: Arc::new(Mutex::new(Some(ctrl_task))),
            telem_cancel,
            state,
            telemetry,
            event_tx,
            pending_context,
            reactor,
            effect_executor,
            input_vad: Arc::new(Mutex::new(BackendInputVad::default())),
            input_processors: Arc::new(Mutex::new(Vec::new())),
            client_activity_authority: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            turn_commit: Arc::new(Mutex::new(None)),
            audio_clock_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            flow,
            background_tracker,
            ctrl_tx: None,
        }
    }

    /// Attach the control-lane sender used to record typed turns on the
    /// transcript. Called once from the builder's `spawn_lanes`.
    pub(crate) fn with_control_sender(mut self, ctrl_tx: mpsc::WeakSender<ControlEvent>) -> Self {
        self.ctrl_tx = Some(ctrl_tx);
        self
    }

    /// Send audio data (raw PCM16 16kHz bytes).
    ///
    /// Configured input processors (see [`configure_input_audio`](Self::configure_input_audio))
    /// run over the frame first; the backend input VAD then sees the
    /// processed stream, and under client activity authority its speech
    /// edges are forwarded to the server as activityStart/activityEnd.
    /// When deferred context delivery is enabled, any pending model-role
    /// context turns are flushed to the wire before the audio frame.
    pub async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError> {
        let data = {
            let mut processors = self.input_processors.lock();
            if processors.is_empty() {
                data
            } else {
                let mut frame: Vec<i16> = data
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect();
                for processor in processors.iter_mut() {
                    processor.process_frame(&mut frame);
                }
                frame.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
        };
        let (vad_events, sample_rate) = {
            let mut input_vad = self.input_vad.lock();
            (input_vad.process_pcm_bytes(&data), input_vad.sample_rate())
        };
        let client_authority = self
            .client_activity_authority
            .load(std::sync::atomic::Ordering::Relaxed);

        // Advance the audio clock by this chunk's duration and run the
        // turn-commit policy (if configured) over the observed edges. The
        // policy decides which activity marks reach the wire; raw-edge
        // callbacks below stay untouched so watchers and transcript
        // bookkeeping see VAD truth either way.
        let commit_signals = {
            let samples = (data.len() / 2) as u64;
            let chunk_ms = samples * 1000 / u64::from(sample_rate.max(1));
            let now_ms = chunk_ms
                + self
                    .audio_clock_ms
                    .fetch_add(chunk_ms, std::sync::atomic::Ordering::Relaxed);
            let mut policy = self.turn_commit.lock();
            policy.as_mut().map(|p| {
                let model_speaking = self
                    .state
                    .session()
                    .get::<bool>("is_model_speaking")
                    .unwrap_or(false);
                p.advance(now_ms, &vad_events, model_speaking)
            })
        };

        if vad_events.contains(&VadEvent::SpeechStart) {
            if client_authority && commit_signals.is_none() {
                self.writer.signal_activity_start().await?;
            }
            self.user_speech_started().await?;
        }
        if let Some(signals) = &commit_signals {
            // Start-type commits go out before the audio, like raw marks.
            for signal in signals {
                if client_authority
                    && matches!(
                        signal,
                        TurnSignal::ActivityStart | TurnSignal::InterruptionStart
                    )
                {
                    self.writer.signal_activity_start().await?;
                }
            }
        }

        self.writer.send_audio(data).await?;

        if vad_events.contains(&VadEvent::SpeechEnd) {
            if client_authority && commit_signals.is_none() {
                self.writer.signal_activity_end().await?;
            }
            self.user_speech_ended().await?;
        }
        if let Some(signals) = &commit_signals {
            for signal in signals {
                if client_authority && matches!(signal, TurnSignal::ActivityEnd) {
                    self.writer.signal_activity_end().await?;
                }
            }
        }

        Ok(())
    }

    /// Install a turn-commit policy between the input VAD's speech edges and
    /// the activity marks sent under client activity authority.
    ///
    /// Raw edges make two measured mistakes as turn signals (TurnBench dev
    /// set): committing end-of-turn during mid-turn pauses, and treating
    /// backchannels ("mm-hm") over model speech as barge-ins. The policy's
    /// end-hold and interruption-sustain rules suppress both — see
    /// [`TurnCommitConfig`](super::turn_commit::TurnCommitConfig) for the
    /// measured operating points. Without a
    /// policy, edges forward to the wire unchanged.
    pub fn set_turn_commit(&self, config: super::turn_commit::TurnCommitConfig) {
        *self.turn_commit.lock() = Some(super::turn_commit::TurnCommitPolicy::new(config));
    }

    /// Configure the input audio path: mic-chain processors applied inside
    /// [`send_audio`](Self::send_audio), an optional replacement input-VAD
    /// configuration, and the interruption authority. Call once after
    /// connect, before streaming audio; a mid-stream call resets the VAD's
    /// adaptive state.
    pub fn configure_input_audio(
        &self,
        processors: Vec<Box<dyn InputAudioProcessor>>,
        vad: Option<gemini_genai_rs::vad::VadConfig>,
        authority: ActivityAuthority,
    ) {
        *self.input_processors.lock() = processors;
        if let Some(config) = vad {
            *self.input_vad.lock() = BackendInputVad::new(config);
        }
        self.client_activity_authority.store(
            authority == ActivityAuthority::Client,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Send a text message.
    ///
    /// When deferred context delivery is enabled, any pending model-role
    /// context turns are flushed to the wire before the text message.
    ///
    /// The text is also recorded on the session transcript as the user side of
    /// the current turn, through the same internal control event that ASR of
    /// audio produces. Without this a text-driven session would hand every
    /// [`TurnExtractor`](super::extractor::TurnExtractor) an empty user turn,
    /// since the transcript's user side is otherwise written only by ASR.
    /// Routing through the control event rather than poking the buffer keeps a
    /// typed turn and a spoken one the *same* event downstream.
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), SessionError> {
        let text = text.into();
        self.telemetry.record_text_send();
        self.writer.send_text(text.clone()).await?;

        // Record only after a *successful* send: a turn the model never
        // received is not part of the conversation.
        if let Some(tx) = self.ctrl_tx.as_ref().and_then(mpsc::WeakSender::upgrade) {
            let _ = tx.send(ControlEvent::InputTranscript(text)).await;
        }

        Ok(())
    }

    /// Send a video/image frame (raw JPEG bytes).
    ///
    /// When deferred context delivery is enabled, any pending model-role
    /// context turns are flushed to the wire before the video frame.
    pub async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        self.writer.send_video(jpeg_data).await
    }

    /// Update the system instruction mid-session.
    pub async fn update_instruction(
        &self,
        instruction: impl Into<String>,
    ) -> Result<(), SessionError> {
        SessionWriter::update_instruction(&self.session, instruction.into()).await
    }

    /// Send tool responses manually (if not using auto-dispatch).
    pub async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), SessionError> {
        self.session.send_tool_response(responses).await
    }

    /// Notify the runtime that client-side playback has drained.
    ///
    /// Voice UIs should call this only when it is safe for the model to speak,
    /// for example after browser speaker playback has drained and the user is
    /// not actively speaking. User audio/text sends intentionally flush context
    /// only and leave the prompt armed.
    pub async fn playback_drained(&self) -> Result<(), SessionError> {
        let prompt_pending = self
            .pending_context
            .as_ref()
            .is_some_and(|pending| pending.has_prompt());
        let reactions = self
            .reactor
            .react(&ReactorEvent::PlaybackDrained { prompt_pending });
        self.effect_executor.execute_reactions(reactions).await
    }

    /// Notify the runtime that client-side user speech has started.
    ///
    /// This is the barge-in edge for voice clients: pending model prompts are
    /// cancelled before they can race with user audio, while queued context is
    /// kept so the next user send can still carry it.
    pub async fn user_speech_started(&self) -> Result<(), SessionError> {
        let reactions = self.reactor.react(&ReactorEvent::UserSpeechStarted);
        self.effect_executor.execute_reactions(reactions).await
    }

    /// Notify the runtime that client-side user speech has ended.
    pub async fn user_speech_ended(&self) -> Result<(), SessionError> {
        let prompt_pending = self
            .pending_context
            .as_ref()
            .is_some_and(|pending| pending.has_prompt());
        let reactions = self
            .reactor
            .react(&ReactorEvent::UserSpeechEnded { prompt_pending });
        self.effect_executor.execute_reactions(reactions).await
    }

    /// Snapshot the reactor-owned voice runtime state.
    pub fn voice_state(&self) -> VoiceRuntimeState {
        self.reactor.voice_state()
    }

    /// Snapshot backend input VAD state.
    pub fn input_vad_state(&self) -> BackendVadSnapshot {
        self.input_vad.lock().snapshot()
    }

    /// Flush deferred context and any pending model prompt.
    ///
    /// Prefer [`Self::playback_drained`] for voice clients. This compatibility
    /// method routes through the same reactor/effect executor path.
    pub async fn flush_deferred_prompt(&self) -> Result<(), SessionError> {
        self.playback_drained().await
    }

    /// Get the user-facing session writer.
    ///
    /// When deferred context delivery is enabled, this returns the
    /// `DeferredWriter` that flushes pending context before sends.
    pub fn writer(&self) -> Arc<dyn SessionWriter> {
        self.writer.clone()
    }

    /// Subscribe to raw session events (for custom processing).
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.session.subscribe()
    }

    /// Get the current session phase.
    pub fn phase(&self) -> SessionPhase {
        self.session.phase()
    }

    /// Gracefully disconnect the session.
    ///
    /// Shutdown sequence:
    /// 1. Cancel all in-flight background tool tasks (they are aborted at an
    ///    await point; tool futures must therefore be drop-safe).
    /// 2. Close the L0 session. The terminal `Disconnected` event makes the
    ///    event router exit, which closes the lane channels.
    /// 3. Grace-await the fast and control lanes (~250 ms each) so they can
    ///    drain queued events and run their final persistence drain, then
    ///    abort whatever is still stuck (e.g. a lane blocked in a slow tool).
    /// 4. Cancel the telemetry lane.
    pub async fn disconnect(&self) -> Result<(), SessionError> {
        // Cancel background tool tasks FIRST: once the session is closing,
        // their results can no longer be delivered, and leaving them running
        // would let them post stale ToolCompleted events to a dead lane.
        self.background_tracker.cancel_all();
        let result = SessionWriter::disconnect(&self.session).await;

        // Grace-await the lanes, then abort. Taking the JoinHandles out of
        // their mutexes gives us the ownership `await` requires; a second
        // disconnect (or a clone's disconnect) simply finds them gone.
        for lane in [&self.fast_task, &self.ctrl_task] {
            let task = lane.lock().take();
            if let Some(mut task) = task {
                if tokio::time::timeout(Self::LANE_SHUTDOWN_GRACE, &mut task)
                    .await
                    .is_err()
                {
                    task.abort();
                }
            }
        }

        // Stop the telemetry lane (it runs on its own broadcast receiver and
        // would otherwise idle on its debounce timer for the handle's lifetime).
        self.telem_cancel.cancel();
        result
    }

    /// How long [`disconnect`](Self::disconnect) waits for each lane to drain
    /// before aborting it.
    const LANE_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

    /// Wait for the session to end (disconnect, GoAway, or error).
    pub async fn done(&self) -> Result<(), SessionError> {
        self.session
            .join()
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Get the underlying SessionHandle for advanced usage.
    pub fn session(&self) -> &SessionHandle {
        &self.session
    }

    /// Latest session-resumption handle issued by the server, if any.
    ///
    /// While session resumption is enabled
    /// ([`SessionConfig::session_resumption`](gemini_genai_rs::prelude::SessionConfig::session_resumption);
    /// L2: `Live::builder().session_resume(true)`), the Gemini server
    /// periodically sends `SessionResumptionUpdate` messages; this returns the
    /// most recent handle (also captured in persistence snapshots as
    /// [`SessionSnapshot::resume_handle`](crate::live::persistence::SessionSnapshot::resume_handle)).
    ///
    /// To survive a server-initiated `GoAway` or a planned restart, read this
    /// handle (e.g. from the `on_go_away` callback) and pass it to
    /// `session_resumption(Some(handle))` on the next connect's
    /// [`SessionConfig`](gemini_genai_rs::prelude::SessionConfig). No
    /// automatic reconnect is performed — resumption is an explicit caller
    /// decision.
    ///
    /// Returns `None` when resumption is disabled or no update has arrived yet.
    pub fn resume_handle(&self) -> Option<String> {
        self.session.state.resume_handle.lock().clone()
    }

    /// Access the shared State container.
    ///
    /// Extraction results from `TurnExtractor`s are stored here under the
    /// extractor's name. Use `state().get::<T>(name)` to read typed values.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Access the session telemetry (auto-collected by the telemetry lane).
    ///
    /// Use `telemetry().snapshot()` to get a JSON snapshot of all metrics.
    pub fn telemetry(&self) -> &Arc<SessionTelemetry> {
        &self.telemetry
    }

    /// Subscribe to semantic events from the processor.
    ///
    /// Returns a broadcast receiver. Call multiple times for independent
    /// subscribers. Zero-cost when no subscribers exist.
    pub fn events(&self) -> broadcast::Receiver<super::events::LiveEvent> {
        self.event_tx.subscribe()
    }

    /// Subscribe to semantic events as a [`futures_util::Stream`].
    ///
    /// Stream-flavored sibling of [`events`](Self::events): each call creates
    /// an independent subscriber starting from the current point in the event
    /// flow. If the subscriber falls behind the broadcast buffer, the missed
    /// events are skipped and the stream continues; the stream ends when the
    /// session's event channel closes. See
    /// [`LiveEventStream`](super::events::LiveEventStream).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use futures_util::StreamExt;
    ///
    /// let mut stream = handle.stream();
    /// while let Some(ev) = stream.next().await {
    ///     match ev {
    ///         LiveEvent::TextDelta(t) => print!("{t}"),
    ///         LiveEvent::TurnComplete => println!(),
    ///         _ => {}
    ///     }
    /// }
    /// ```
    pub fn stream(&self) -> super::events::LiveEventStream {
        super::events::LiveEventStream::new(self.event_tx.subscribe())
    }

    /// Convenience: get the latest extraction result by extractor name.
    pub fn extracted<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.state.get(name)
    }

    /// Snapshot the governed flow's control-plane state: active steps, which
    /// tools are admitted vs blocked (with reasons), and unmet requirements.
    ///
    /// The deterministic answer to "why did the assistant ask that?" — computed
    /// against the live [`State`] and the marking the control lane maintains.
    /// Returns `None` when the session is not governed by a flow
    /// (`Live::govern`/`observe` was not used).
    ///
    /// This is a synchronous snapshot: it briefly locks the shared
    /// [`FlowMonitor`](crate::flow::FlowMonitor) and never blocks on session
    /// I/O.
    pub fn explain(&self) -> Option<FlowExplanation> {
        self.flow
            .as_ref()
            .map(|mon| mon.lock().explain(&self.state))
    }

    /// Why the governed flow is blocked right now — alias of
    /// [`explain`](Self::explain), named for the common debugging question.
    /// Returns `None` when the session is not governed by a flow.
    pub fn why_blocked(&self) -> Option<FlowExplanation> {
        self.explain()
    }

    /// Replace a governed step's posture mid-session. Returns `true` when the
    /// session is governed and the step exists.
    ///
    /// Postures are re-projected at every turn boundary, so the edit steers
    /// the very next turn. This is the *safe* subset of live spec editing:
    /// the DAG, guards, and tool gates stay fixed (tool declarations cannot
    /// change mid-session at the wire level anyway).
    pub fn update_step_posture(&self, step_id: &str, posture: Option<String>) -> bool {
        self.flow
            .as_ref()
            .map(|mon| mon.lock().set_posture(step_id, posture))
            .unwrap_or(false)
    }

    /// Replace a governed step's grounding template mid-session. Same
    /// semantics as [`update_step_posture`](Self::update_step_posture).
    pub fn update_step_ground(&self, step_id: &str, ground: Option<String>) -> bool {
        self.flow
            .as_ref()
            .map(|mon| mon.lock().set_ground(step_id, ground))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::telemetry::SessionTelemetry;
    use gemini_genai_rs::session::{SessionCommand, SessionState};
    use tokio_util::sync::CancellationToken;

    /// Build a LiveHandle wired to an in-memory SessionHandle (no transport).
    /// The command receiver is returned so `disconnect()` sends succeed.
    fn make_handle_with_lanes(
        fast: JoinHandle<()>,
        ctrl: JoinHandle<()>,
    ) -> (LiveHandle, tokio::sync::mpsc::Receiver<SessionCommand>) {
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(8);
        let (event_tx, _) = broadcast::channel(16);
        let (phase_tx, phase_rx) = tokio::sync::watch::channel(SessionPhase::Active);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));
        let session = SessionHandle::new(command_tx, event_tx, state, phase_rx);
        let writer: Arc<dyn SessionWriter> = Arc::new(session.clone());
        let (live_tx, _) = broadcast::channel(16);
        let handle = LiveHandle::new(
            session,
            writer,
            fast,
            ctrl,
            State::new(),
            Arc::new(SessionTelemetry::new()),
            live_tx,
            None,
            None,
            Arc::new(BackgroundToolTracker::new()),
            CancellationToken::new(),
        );
        (handle, command_rx)
    }

    fn make_handle() -> (LiveHandle, tokio::sync::mpsc::Receiver<SessionCommand>) {
        make_handle_with_lanes(tokio::spawn(async {}), tokio::spawn(async {}))
    }

    /// Sets a flag when dropped — observes that an aborted task's future was
    /// actually torn down.
    struct SetOnDrop(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn disconnect_cancels_background_tool_tasks() {
        let (handle, _cmd_rx) = make_handle();
        let tracker = handle.background_tracker.clone();

        // Register a never-finishing background tool task.
        let token = CancellationToken::new();
        let t = token.clone();
        let task = tokio::spawn(async move {
            t.cancelled().await;
            std::future::pending::<()>().await;
        });
        tracker.spawn("call-1".into(), task, token.clone());
        assert_eq!(tracker.active_count(), 1);

        handle.disconnect().await.expect("disconnect");

        assert_eq!(
            tracker.active_count(),
            0,
            "disconnect must cancel all tracked background tool tasks"
        );
        assert!(token.is_cancelled(), "cooperative token must be cancelled");
    }

    #[tokio::test]
    async fn disconnect_aborts_stuck_lanes_within_grace_period() {
        use std::sync::atomic::{AtomicBool, Ordering};

        // Lanes that never finish on their own (simulating a lane blocked in a
        // slow tool); drop guards record that abort tore the futures down.
        let fast_dropped = Arc::new(AtomicBool::new(false));
        let ctrl_dropped = Arc::new(AtomicBool::new(false));
        let f = fast_dropped.clone();
        let c = ctrl_dropped.clone();
        let fast = tokio::spawn(async move {
            let _guard = SetOnDrop(f);
            std::future::pending::<()>().await;
        });
        let ctrl = tokio::spawn(async move {
            let _guard = SetOnDrop(c);
            std::future::pending::<()>().await;
        });

        let (handle, _cmd_rx) = make_handle_with_lanes(fast, ctrl);
        let telem_cancel = handle.telem_cancel.clone();

        // disconnect() must return in bounded time even with stuck lanes.
        tokio::time::timeout(std::time::Duration::from_secs(2), handle.disconnect())
            .await
            .expect("disconnect must not hang on stuck lanes")
            .expect("disconnect");

        // Give the aborts a beat to take effect, then verify teardown.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            fast_dropped.load(Ordering::SeqCst),
            "fast lane must be aborted after the grace period"
        );
        assert!(
            ctrl_dropped.load(Ordering::SeqCst),
            "control lane must be aborted after the grace period"
        );
        assert!(
            telem_cancel.is_cancelled(),
            "telemetry lane must be cancelled on disconnect"
        );
    }

    #[tokio::test]
    async fn resume_handle_surfaces_latest_server_handle() {
        let (handle, _cmd_rx) = make_handle();
        assert_eq!(handle.resume_handle(), None, "no update yet");

        // Simulate the L0 transport storing a SessionResumptionUpdate.
        *handle.session.state.resume_handle.lock() = Some("rh-42".into());
        assert_eq!(handle.resume_handle(), Some("rh-42".to_string()));
    }

    #[tokio::test]
    async fn disconnect_is_idempotent_across_clones() {
        let (handle, _cmd_rx) = make_handle();
        let clone = handle.clone();
        handle.disconnect().await.expect("first disconnect");
        // The clone's disconnect finds the lane handles already taken.
        clone.disconnect().await.expect("second disconnect");
    }
}
