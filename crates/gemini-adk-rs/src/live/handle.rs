//! LiveHandle — runtime interaction with a Live session.

use std::sync::Arc;

use gemini_genai_rs::prelude::{FunctionResponse, SessionEvent, SessionPhase, VadEvent};
use gemini_genai_rs::session::{SessionError, SessionHandle, SessionWriter};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::flow::{FlowExplanation, SharedFlowMonitor};
use crate::state::State;

use super::context_writer::PendingContext;
use super::effect_executor::LiveEffectExecutor;
use super::input_vad::{BackendInputVad, BackendVadSnapshot};
use super::reactor::{LiveReactor, ReactorEvent, VoiceRuntimeState};
use super::telemetry::SessionTelemetry;

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
    _fast_task: Arc<JoinHandle<()>>,
    _ctrl_task: Arc<JoinHandle<()>>,
    state: State,
    telemetry: Arc<SessionTelemetry>,
    event_tx: broadcast::Sender<super::events::LiveEvent>,
    pending_context: Option<Arc<PendingContext>>,
    reactor: Arc<LiveReactor>,
    effect_executor: LiveEffectExecutor,
    input_vad: Arc<Mutex<BackendInputVad>>,
    /// Governed-flow monitor shared with the control lane (None when the
    /// session is not governed by a flow).
    flow: Option<SharedFlowMonitor>,
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
            _fast_task: Arc::new(fast_task),
            _ctrl_task: Arc::new(ctrl_task),
            state,
            telemetry,
            event_tx,
            pending_context,
            reactor,
            effect_executor,
            input_vad: Arc::new(Mutex::new(BackendInputVad::default())),
            flow,
        }
    }

    /// Send audio data (raw PCM16 16kHz bytes).
    ///
    /// When deferred context delivery is enabled, any pending model-role
    /// context turns are flushed to the wire before the audio frame.
    pub async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError> {
        let vad_events = {
            let mut input_vad = self.input_vad.lock();
            input_vad.process_pcm_bytes(&data)
        };

        if vad_events.contains(&VadEvent::SpeechStart) {
            self.user_speech_started().await?;
        }

        self.writer.send_audio(data).await?;

        if vad_events.contains(&VadEvent::SpeechEnd) {
            self.user_speech_ended().await?;
        }

        Ok(())
    }

    /// Send a text message.
    ///
    /// When deferred context delivery is enabled, any pending model-role
    /// context turns are flushed to the wire before the text message.
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), SessionError> {
        self.telemetry.record_text_send();
        self.writer.send_text(text.into()).await
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
    pub async fn disconnect(&self) -> Result<(), SessionError> {
        SessionWriter::disconnect(&self.session).await
    }

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
}
