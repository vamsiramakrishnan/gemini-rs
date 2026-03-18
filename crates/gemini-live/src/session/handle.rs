//! [`SessionHandle`] — the public API surface for a Gemini Live session.
//!
//! Cheaply cloneable (wraps `Arc`). Provides methods to send commands,
//! subscribe to events, and observe session state.

use super::errors::SessionError;
use super::events::{SessionCommand, SessionEvent};
use super::state::{SessionPhase, SessionState};
use super::traits::{SessionReader, SessionWriter};
use crate::protocol::{Content, FunctionResponse};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

/// The public API surface for a Gemini Live session.
///
/// Cheaply cloneable (wraps `Arc`). Provides methods to send commands,
/// subscribe to events, and observe session state.
#[derive(Clone)]
pub struct SessionHandle {
    /// Channel for sending commands to the transport layer.
    pub command_tx: mpsc::Sender<SessionCommand>,
    /// Broadcast channel for session events.
    event_tx: broadcast::Sender<SessionEvent>,
    /// Shared session state.
    pub state: Arc<SessionState>,
    /// Phase watch receiver for async observation.
    phase_rx: watch::Receiver<SessionPhase>,
    /// Handle to the spawned connection loop task.
    ///
    /// Wrapped in `Arc<Mutex<Option<...>>>` so that `SessionHandle` remains
    /// `Clone` (since `JoinHandle` is not `Clone`). The first call to
    /// [`join()`](Self::join) takes the handle; subsequent calls return `Ok(())`.
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl SessionHandle {
    /// Create a new session handle from its components.
    pub fn new(
        command_tx: mpsc::Sender<SessionCommand>,
        event_tx: broadcast::Sender<SessionEvent>,
        state: Arc<SessionState>,
        phase_rx: watch::Receiver<SessionPhase>,
    ) -> Self {
        Self {
            command_tx,
            event_tx,
            state,
            phase_rx,
            task: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Store the connection loop task handle.
    ///
    /// Called by the transport layer after spawning the connection loop.
    pub fn set_task(&self, handle: JoinHandle<()>) {
        // Use try_lock to avoid blocking — this is only called once at startup.
        if let Ok(mut guard) = self.task.try_lock() {
            *guard = Some(handle);
        }
    }

    /// Wait for the session connection loop to complete.
    ///
    /// Returns `Ok(())` when the session disconnects normally.
    /// Returns `Err` if the connection task panicked.
    ///
    /// Only the first call across all clones actually awaits the task;
    /// subsequent calls return `Ok(())` immediately.
    pub async fn join(&self) -> Result<(), tokio::task::JoinError> {
        let task = self.task.lock().await.take();
        if let Some(handle) = task {
            handle.await
        } else {
            Ok(())
        }
    }

    /// Subscribe to session events.
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.event_tx.subscribe()
    }

    /// Get the event sender (for internal use by transport).
    pub fn event_sender(&self) -> &broadcast::Sender<SessionEvent> {
        &self.event_tx
    }

    /// Current session phase.
    pub fn phase(&self) -> SessionPhase {
        self.state.phase()
    }

    /// Session ID.
    pub fn session_id(&self) -> &str {
        &self.state.session_id
    }

    /// Wait for the session to reach a specific phase.
    pub async fn wait_for_phase(&self, target: SessionPhase) {
        let mut rx = self.phase_rx.clone();
        while *rx.borrow_and_update() != target {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// Send audio data (raw PCM16 bytes).
    pub async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendAudio(data)).await
    }

    /// Send a text message.
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendText(text.into()))
            .await
    }

    /// Send tool responses.
    pub async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendToolResponse(responses))
            .await
    }

    /// Send a video/image frame (raw JPEG bytes).
    pub async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendVideo(jpeg_data))
            .await
    }

    /// Update the system instruction mid-session.
    pub async fn update_instruction(
        &self,
        instruction: impl Into<String>,
    ) -> Result<(), SessionError> {
        self.send_command(SessionCommand::UpdateInstruction(instruction.into()))
            .await
    }

    /// Signal activity start (user started speaking).
    pub async fn signal_activity_start(&self) -> Result<(), SessionError> {
        self.send_command(SessionCommand::ActivityStart).await
    }

    /// Signal activity end (user stopped speaking).
    pub async fn signal_activity_end(&self) -> Result<(), SessionError> {
        self.send_command(SessionCommand::ActivityEnd).await
    }

    /// Send client content (turns + turn_complete flag).
    /// Used for injecting conversation history, context, or multi-turn text.
    pub async fn send_client_content(
        &self,
        turns: Vec<Content>,
        turn_complete: bool,
    ) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendClientContent {
            turns,
            turn_complete,
        })
        .await
    }

    /// Gracefully disconnect the session.
    pub async fn disconnect(&self) -> Result<(), SessionError> {
        self.send_command(SessionCommand::Disconnect).await
    }

    /// Send a command to the transport.
    async fn send_command(&self, cmd: SessionCommand) -> Result<(), SessionError> {
        self.command_tx
            .send(cmd)
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("session_id", &self.state.session_id)
            .field("phase", &self.state.phase())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Trait implementations for SessionHandle
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionWriter for SessionHandle {
    async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendAudio(data)).await
    }

    async fn send_text(&self, text: String) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendText(text)).await
    }

    async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendToolResponse(responses))
            .await
    }

    async fn send_client_content(
        &self,
        turns: Vec<Content>,
        turn_complete: bool,
    ) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendClientContent {
            turns,
            turn_complete,
        })
        .await
    }

    async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendVideo(jpeg_data))
            .await
    }

    async fn update_instruction(&self, instruction: String) -> Result<(), SessionError> {
        self.send_command(SessionCommand::UpdateInstruction(instruction))
            .await
    }

    async fn signal_activity_start(&self) -> Result<(), SessionError> {
        self.send_command(SessionCommand::ActivityStart).await
    }

    async fn signal_activity_end(&self) -> Result<(), SessionError> {
        self.send_command(SessionCommand::ActivityEnd).await
    }

    async fn disconnect(&self) -> Result<(), SessionError> {
        self.send_command(SessionCommand::Disconnect).await
    }
}

impl SessionReader for SessionHandle {
    fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.event_tx.subscribe()
    }

    fn phase(&self) -> SessionPhase {
        self.state.phase()
    }

    fn session_id(&self) -> &str {
        &self.state.session_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_handle_join_returns_ok_after_task_completes() {
        let (command_tx, _command_rx) = mpsc::channel(8);
        let (event_tx, _) = broadcast::channel(16);
        let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let handle = SessionHandle::new(command_tx, event_tx, state, phase_rx);

        // Spawn a trivial task that completes immediately
        let task = tokio::spawn(async {});
        handle.set_task(task);

        // join() should return Ok(())
        let result = handle.join().await;
        assert!(
            result.is_ok(),
            "join() should return Ok after task completes"
        );
    }

    #[tokio::test]
    async fn session_handle_join_without_task_returns_ok() {
        let (command_tx, _command_rx) = mpsc::channel(8);
        let (event_tx, _) = broadcast::channel(16);
        let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let handle = SessionHandle::new(command_tx, event_tx, state, phase_rx);

        // join() without set_task should return Ok immediately
        let result = handle.join().await;
        assert!(result.is_ok(), "join() without task should return Ok");
    }

    #[tokio::test]
    async fn session_handle_join_idempotent() {
        let (command_tx, _command_rx) = mpsc::channel(8);
        let (event_tx, _) = broadcast::channel(16);
        let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let handle = SessionHandle::new(command_tx, event_tx, state, phase_rx);

        let task = tokio::spawn(async {});
        handle.set_task(task);

        // First join takes the handle
        assert!(handle.join().await.is_ok());
        // Second join returns Ok immediately (handle already taken)
        assert!(handle.join().await.is_ok());
    }

    #[tokio::test]
    async fn session_handle_join_works_on_clone() {
        let (command_tx, _command_rx) = mpsc::channel(8);
        let (event_tx, _) = broadcast::channel(16);
        let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let handle = SessionHandle::new(command_tx, event_tx, state, phase_rx);
        let handle_clone = handle.clone();

        let task = tokio::spawn(async {});
        handle.set_task(task);

        // join() on clone should work (shares the Arc)
        let result = handle_clone.join().await;
        assert!(result.is_ok(), "join() on clone should work");

        // Original handle's join should now return Ok (handle already taken)
        assert!(handle.join().await.is_ok());
    }

    // PhaseChanged event emission tests

    #[tokio::test]
    async fn phase_changed_event_emitted_on_transition() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Disconnected);
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let state = SessionState::with_events(phase_tx, event_tx);

        state.transition_to(SessionPhase::Connecting).unwrap();

        match event_rx.try_recv() {
            Ok(SessionEvent::PhaseChanged(SessionPhase::Connecting)) => {}
            other => panic!("expected PhaseChanged(Connecting), got {:?}", other),
        }
    }

    #[test]
    fn phase_changed_not_emitted_without_event_tx() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Disconnected);
        let state = SessionState::new(phase_tx);
        // Should not panic even though no event_tx
        state.transition_to(SessionPhase::Connecting).unwrap();
    }
}
