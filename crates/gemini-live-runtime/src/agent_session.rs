//! AgentSession — intercepting wrapper around SessionHandle.
//!
//! Replaces ADK Python's LiveRequestQueue. Instead of adding a second queue
//! on top of SessionHandle's existing mpsc channel, this wraps SessionHandle
//! and intercepts sends for: (1) input fan-out to streaming tools,
//! (2) middleware hooks, (3) state tracking.
//!
//! Data flow: App → AgentSession → SessionHandle → WebSocket
//!                                ↘ broadcast to input-streaming tools
//!
//! ONE queue, ONE consumer task, zero-copy on the hot path.

use gemini_live_wire::prelude::{Content, FunctionResponse};
use gemini_live_wire::session::{SessionEvent, SessionHandle};
use tokio::sync::broadcast;

use crate::error::AgentError;
use crate::state::State;

/// Input events broadcast to input-streaming tools.
/// Distinct from SessionCommand — this is observation-only.
#[derive(Debug, Clone)]
pub enum InputEvent {
    /// Raw PCM16 audio bytes.
    Audio(Vec<u8>),
    /// Text content.
    Text(String),
    /// User started speaking.
    ActivityStart,
    /// User stopped speaking.
    ActivityEnd,
}

/// Intercepting wrapper around SessionHandle.
///
/// Adds input fan-out, middleware hooks, and state tracking without
/// introducing a second queue (avoids double-queuing).
#[derive(Clone)]
pub struct AgentSession {
    /// The underlying wire-level session (Layer 0).
    session: SessionHandle,
    /// Fan-out for input-streaming tools.
    /// Zero-cost when no tools are subscribed (receiver_count == 0).
    input_broadcast: broadcast::Sender<InputEvent>,
    /// Conversation state container.
    state: State,
}

impl AgentSession {
    /// Create a new AgentSession wrapping a SessionHandle.
    pub fn new(session: SessionHandle) -> Self {
        let (input_broadcast, _) = broadcast::channel(256);
        Self {
            session,
            input_broadcast,
            state: State::new(),
        }
    }

    /// Send audio data. Fans out to input-streaming tools ONLY if listeners exist.
    pub async fn send_audio(&self, data: Vec<u8>) -> Result<(), AgentError> {
        // Fan-out ONLY if input-streaming tools are listening
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::Audio(data.clone()));
        }
        // Forward directly to Layer 0 (ONE hop to WebSocket)
        self.session.send_audio(data).await.map_err(AgentError::Session)
    }

    /// Send a text message.
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), AgentError> {
        let t = text.into();
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::Text(t.clone()));
        }
        self.session.send_text(t).await.map_err(AgentError::Session)
    }

    /// Send tool responses.
    pub async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), AgentError> {
        self.session
            .send_tool_response(responses)
            .await
            .map_err(AgentError::Session)
    }

    /// Send client content (conversation history or context injection).
    pub async fn send_client_content(
        &self,
        turns: Vec<Content>,
        turn_complete: bool,
    ) -> Result<(), AgentError> {
        self.session
            .send_client_content(turns, turn_complete)
            .await
            .map_err(AgentError::Session)
    }

    /// Signal activity start (user started speaking).
    pub async fn signal_activity_start(&self) -> Result<(), AgentError> {
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::ActivityStart);
        }
        self.session
            .signal_activity_start()
            .await
            .map_err(AgentError::Session)
    }

    /// Signal activity end (user stopped speaking).
    pub async fn signal_activity_end(&self) -> Result<(), AgentError> {
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::ActivityEnd);
        }
        self.session
            .signal_activity_end()
            .await
            .map_err(AgentError::Session)
    }

    /// Gracefully disconnect.
    pub async fn disconnect(&self) -> Result<(), AgentError> {
        self.session.disconnect().await.map_err(AgentError::Session)
    }

    /// Subscribe to input events (for input-streaming tools).
    pub fn subscribe_input(&self) -> broadcast::Receiver<InputEvent> {
        self.input_broadcast.subscribe()
    }

    /// Subscribe to session events (delegates to SessionHandle).
    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
        self.session.subscribe()
    }

    /// Access the underlying SessionHandle for advanced wire-level control.
    pub fn wire(&self) -> &SessionHandle {
        &self.session
    }

    /// Access conversation state.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Number of input-streaming subscribers (for diagnostics).
    pub fn input_subscriber_count(&self) -> usize {
        self.input_broadcast.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_live_wire::session::{SessionHandle, SessionPhase, SessionState};
    use std::sync::Arc;
    use tokio::sync::{broadcast, mpsc, watch};

    fn mock_session_handle() -> SessionHandle {
        let (cmd_tx, _cmd_rx) = mpsc::channel(16);
        let (evt_tx, _) = broadcast::channel(16);
        let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);
        let state = Arc::new(SessionState::new(phase_tx));
        SessionHandle::new(cmd_tx, evt_tx, state, phase_rx)
    }

    #[tokio::test]
    async fn send_audio_without_subscribers_no_broadcast() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        assert_eq!(session.input_subscriber_count(), 0);
    }

    #[tokio::test]
    async fn send_audio_with_subscriber_broadcasts() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        let mut input_rx = session.subscribe_input();
        assert_eq!(session.input_subscriber_count(), 1);

        // send_audio will fail at SessionHandle level (no real WS), but
        // the broadcast should still fire
        let data = vec![1, 2, 3, 4];
        let _ = session.send_audio(data.clone()).await;

        match input_rx.try_recv() {
            Ok(InputEvent::Audio(received)) => assert_eq!(received, data),
            other => panic!("expected Audio, got {:?}", other),
        }
    }

    #[test]
    fn agent_session_is_clone() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        let _clone = session.clone();
    }

    #[test]
    fn state_accessible() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        session.state().set("key", "value");
        assert_eq!(
            session.state().get::<String>("key"),
            Some("value".to_string())
        );
    }

    #[tokio::test]
    async fn text_broadcast() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        let mut input_rx = session.subscribe_input();

        let _ = session.send_text("hello").await;

        match input_rx.try_recv() {
            Ok(InputEvent::Text(t)) => assert_eq!(t, "hello"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn activity_signals_broadcast() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        let mut input_rx = session.subscribe_input();

        let _ = session.signal_activity_start().await;
        let _ = session.signal_activity_end().await;

        assert!(matches!(input_rx.try_recv(), Ok(InputEvent::ActivityStart)));
        assert!(matches!(input_rx.try_recv(), Ok(InputEvent::ActivityEnd)));
    }
}
