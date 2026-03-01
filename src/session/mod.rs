//! Session orchestration — the central coordination layer.
//!
//! Provides [`SessionHandle`] (the public API surface), [`SessionEvent`] (events from the server),
//! [`SessionCommand`] (commands to the server), and turn tracking.

pub mod state;

pub use state::SessionPhase;

use crate::protocol::{FunctionCall, FunctionResponse};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, watch};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during a session.
#[derive(Debug, Error, Clone)]
pub enum SessionError {
    /// WebSocket-level error (transient, may be retried).
    #[error("WebSocket error: {0}")]
    WebSocket(String),

    /// Timeout waiting for handshake or setup.
    #[error("Timeout")]
    Timeout,

    /// Attempted an invalid phase transition.
    #[error("Invalid transition from {from} to {to}")]
    InvalidTransition { from: SessionPhase, to: SessionPhase },

    /// Operation requires an active connection but session is not connected.
    #[error("Not connected")]
    NotConnected,

    /// Server rejected the setup configuration.
    #[error("Setup failed: {0}")]
    SetupFailed(String),

    /// Server requested graceful disconnect.
    #[error("Server sent GoAway")]
    GoAway,

    /// Internal channel was closed unexpectedly.
    #[error("Internal channel closed")]
    ChannelClosed,

    /// Send queue is full.
    #[error("Send queue full")]
    SendQueueFull,
}

// ---------------------------------------------------------------------------
// Events (server → application)
// ---------------------------------------------------------------------------

/// Events emitted by the session, consumed by application code.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Session connected and setup complete.
    Connected,
    /// Incremental text from model response.
    TextDelta(String),
    /// Complete text of a finished model turn.
    TextComplete(String),
    /// Audio data from model response (PCM16 samples, base64-decoded).
    AudioData(Vec<u8>),
    /// Input transcription from server.
    InputTranscription(String),
    /// Output transcription from server.
    OutputTranscription(String),
    /// Model requested tool calls.
    ToolCall(Vec<FunctionCall>),
    /// Server cancelled pending tool calls.
    ToolCallCancelled(Vec<String>),
    /// Model turn is complete.
    TurnComplete,
    /// Model was interrupted by barge-in.
    Interrupted,
    /// Session phase changed.
    PhaseChanged(SessionPhase),
    /// Server sent GoAway signal with optional time remaining.
    GoAway(Option<String>),
    /// Session disconnected (with optional reason).
    Disconnected(Option<String>),
    /// Non-fatal error.
    Error(String),
    /// Session resumption handle received from server.
    SessionResumeHandle(String),
}

// ---------------------------------------------------------------------------
// Commands (application → server)
// ---------------------------------------------------------------------------

/// Commands sent from application code to the session transport.
#[derive(Debug, Clone)]
pub enum SessionCommand {
    /// Send audio data (raw PCM16 bytes, will be base64-encoded).
    SendAudio(Vec<u8>),
    /// Send a text message.
    SendText(String),
    /// Send tool responses.
    SendToolResponse(Vec<FunctionResponse>),
    /// Signal activity start (client VAD detected speech).
    ActivityStart,
    /// Signal activity end (client VAD detected silence).
    ActivityEnd,
    /// Gracefully disconnect.
    Disconnect,
}

// ---------------------------------------------------------------------------
// Turn tracking
// ---------------------------------------------------------------------------

/// Represents a single model response turn.
#[derive(Debug, Clone)]
pub struct Turn {
    /// Unique turn identifier.
    pub id: String,
    /// Accumulated text parts.
    pub text: String,
    /// Whether this turn included audio.
    pub has_audio: bool,
    /// Tool calls requested in this turn.
    pub tool_calls: Vec<FunctionCall>,
    /// When the turn started.
    pub started_at: Instant,
    /// When the turn completed (if complete).
    pub completed_at: Option<Instant>,
    /// Whether the turn was interrupted.
    pub interrupted: bool,
}

impl Turn {
    /// Create a new turn.
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            text: String::new(),
            has_audio: false,
            tool_calls: Vec::new(),
            started_at: Instant::now(),
            completed_at: None,
            interrupted: false,
        }
    }

    /// Duration of the turn.
    pub fn duration(&self) -> std::time::Duration {
        let end = self.completed_at.unwrap_or_else(Instant::now);
        end.duration_since(self.started_at)
    }
}

impl Default for Turn {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Session state (shared, read-mostly)
// ---------------------------------------------------------------------------

/// Shared session state, accessible from the SessionHandle.
#[derive(Debug)]
pub struct SessionState {
    /// Current phase (updated atomically via watch channel).
    phase_tx: watch::Sender<SessionPhase>,
    /// Session ID.
    pub session_id: String,
    /// Session resume handle from server.
    pub resume_handle: parking_lot::Mutex<Option<String>>,
    /// Turn history.
    pub turns: parking_lot::Mutex<Vec<Turn>>,
    /// Current in-progress turn.
    pub current_turn: parking_lot::Mutex<Option<Turn>>,
}

impl SessionState {
    /// Create new session state.
    pub fn new(phase_tx: watch::Sender<SessionPhase>) -> Self {
        Self {
            phase_tx,
            session_id: uuid::Uuid::new_v4().to_string(),
            resume_handle: parking_lot::Mutex::new(None),
            turns: parking_lot::Mutex::new(Vec::new()),
            current_turn: parking_lot::Mutex::new(None),
        }
    }

    /// Get the current phase.
    pub fn phase(&self) -> SessionPhase {
        *self.phase_tx.borrow()
    }

    /// Attempt a validated phase transition.
    pub fn transition_to(&self, to: SessionPhase) -> Result<SessionPhase, SessionError> {
        let from = self.phase();
        if !from.can_transition_to(&to) {
            return Err(SessionError::InvalidTransition { from, to });
        }
        self.phase_tx.send_replace(to);
        Ok(to)
    }

    /// Force transition (bypasses validation — use only for disconnect).
    pub fn force_phase(&self, phase: SessionPhase) {
        self.phase_tx.send_replace(phase);
    }

    /// Start a new turn.
    pub fn start_turn(&self) {
        let mut current = self.current_turn.lock();
        if let Some(prev) = current.take() {
            self.turns.lock().push(prev);
        }
        *current = Some(Turn::new());
    }

    /// Append text to the current turn.
    pub fn append_text(&self, text: &str) {
        if let Some(turn) = self.current_turn.lock().as_mut() {
            turn.text.push_str(text);
        }
    }

    /// Mark audio received in the current turn.
    pub fn mark_audio(&self) {
        if let Some(turn) = self.current_turn.lock().as_mut() {
            turn.has_audio = true;
        }
    }

    /// Complete the current turn.
    pub fn complete_turn(&self) -> Option<Turn> {
        let mut current = self.current_turn.lock();
        if let Some(turn) = current.as_mut() {
            turn.completed_at = Some(Instant::now());
        }
        let completed = current.take();
        if let Some(ref t) = completed {
            self.turns.lock().push(t.clone());
        }
        completed
    }

    /// Mark the current turn as interrupted.
    pub fn interrupt_turn(&self) {
        if let Some(turn) = self.current_turn.lock().as_mut() {
            turn.interrupted = true;
            turn.completed_at = Some(Instant::now());
        }
    }
}

// ---------------------------------------------------------------------------
// Session handle (public API)
// ---------------------------------------------------------------------------

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

    /// Signal activity start (user started speaking).
    pub async fn signal_activity_start(&self) -> Result<(), SessionError> {
        self.send_command(SessionCommand::ActivityStart).await
    }

    /// Signal activity end (user stopped speaking).
    pub async fn signal_activity_end(&self) -> Result<(), SessionError> {
        self.send_command(SessionCommand::ActivityEnd).await
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
