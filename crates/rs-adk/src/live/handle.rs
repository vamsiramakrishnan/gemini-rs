//! LiveHandle — runtime interaction with a Live session.

use std::sync::Arc;

use rs_genai::prelude::{FunctionResponse, SessionEvent, SessionPhase};
use rs_genai::session::{SessionError, SessionHandle, SessionWriter};
use serde::de::DeserializeOwned;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::state::State;

use super::telemetry::SessionTelemetry;

/// Handle for interacting with a running Live session.
///
/// Provides send methods for audio/text/video, system instruction updates,
/// event subscription, state access, telemetry, and graceful shutdown.
#[derive(Clone)]
pub struct LiveHandle {
    session: SessionHandle,
    _fast_task: Arc<JoinHandle<()>>,
    _ctrl_task: Arc<JoinHandle<()>>,
    state: State,
    telemetry: Arc<SessionTelemetry>,
}

impl LiveHandle {
    pub(crate) fn new(
        session: SessionHandle,
        fast_task: JoinHandle<()>,
        ctrl_task: JoinHandle<()>,
        state: State,
        telemetry: Arc<SessionTelemetry>,
    ) -> Self {
        Self {
            session,
            _fast_task: Arc::new(fast_task),
            _ctrl_task: Arc::new(ctrl_task),
            state,
            telemetry,
        }
    }

    /// Send audio data (raw PCM16 16kHz bytes).
    pub async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError> {
        self.session.send_audio(data).await
    }

    /// Send a text message.
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), SessionError> {
        self.session.send_text(text.into()).await
    }

    /// Send a video/image frame (raw JPEG bytes).
    pub async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        SessionWriter::send_video(&self.session, jpeg_data).await
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

    /// Convenience: get the latest extraction result by extractor name.
    pub fn extracted<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.state.get(name)
    }
}
