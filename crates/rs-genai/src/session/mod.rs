//! Session orchestration — the central coordination layer.
//!
//! Provides [`SessionHandle`] (the public API surface), [`SessionEvent`] (events from the server),
//! [`SessionCommand`] (commands to the server), and turn tracking.

pub mod state;

pub use state::SessionPhase;

use async_trait::async_trait;
use crate::protocol::{Content, FunctionCall, FunctionResponse, UsageMetadata};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during a session.
#[derive(Debug, Error, Clone)]
pub enum SessionError {
    /// WebSocket-level error (transient, may be retried).
    #[error("WebSocket error: {0}")]
    WebSocket(WebSocketError),

    /// Timeout waiting for handshake or setup.
    #[error("Timeout in {phase} after {elapsed:?}")]
    Timeout {
        /// Which phase timed out.
        phase: SessionPhase,
        /// How long was waited before timing out.
        elapsed: std::time::Duration,
    },

    /// Attempted an invalid phase transition.
    #[error("Invalid transition from {from} to {to}")]
    InvalidTransition {
        /// Phase the session was in.
        from: SessionPhase,
        /// Phase the transition attempted to reach.
        to: SessionPhase,
    },

    /// Operation requires an active connection but session is not connected.
    #[error("Not connected")]
    NotConnected,

    /// Server rejected the setup configuration.
    #[error("Setup failed: {0}")]
    SetupFailed(SetupError),

    /// Server requested graceful disconnect.
    #[error("Server sent GoAway (time left: {time_left:?})")]
    GoAway {
        /// Time remaining before forced disconnect.
        time_left: Option<std::time::Duration>,
    },

    /// Internal channel was closed unexpectedly.
    #[error("Internal channel closed")]
    ChannelClosed,

    /// Send queue is full.
    #[error("Send queue full")]
    SendQueueFull,

    /// Authentication error.
    #[error("Auth error: {0}")]
    Auth(AuthError),
}

/// WebSocket-level errors with structured detail.
#[derive(Debug, Error, Clone)]
pub enum WebSocketError {
    /// Remote server refused the connection.
    #[error("Connection refused: {0}")]
    ConnectionRefused(String),

    /// Protocol-level WebSocket error (frame errors, encoding, etc.).
    #[error("Protocol error: {0}")]
    ProtocolError(String),

    /// Connection was closed with a status code and reason.
    #[error("Connection closed (code={code}, reason={reason})")]
    Closed {
        /// WebSocket close status code.
        code: u16,
        /// Human-readable close reason.
        reason: String,
    },
}

/// Errors during the setup handshake phase.
#[derive(Debug, Error, Clone)]
pub enum SetupError {
    /// The specified model was invalid or not found.
    #[error("Invalid model: {0}")]
    InvalidModel(String),

    /// Authentication failed during setup.
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    /// Server rejected the setup request.
    #[error("Server rejected: {message}")]
    ServerRejected {
        /// Optional error code from the server.
        code: Option<String>,
        /// Error message from the server.
        message: String,
    },

    /// Setup timed out before receiving setupComplete.
    #[error("Setup timed out")]
    Timeout,
}

/// Authentication-specific errors.
#[derive(Debug, Error, Clone)]
pub enum AuthError {
    /// The bearer token has expired.
    #[error("Token expired")]
    TokenExpired,

    /// Failed to fetch a fresh token.
    #[error("Token fetch failed: {0}")]
    TokenFetchFailed(String),

    /// Token lacks required scopes.
    #[error("Insufficient scopes: {0}")]
    InsufficientScopes(String),
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
    ///
    /// Uses [`bytes::Bytes`] for zero-copy fan-out: cloning a `Bytes` handle
    /// bumps an `Arc` refcount instead of copying the underlying data.
    AudioData(bytes::Bytes),
    /// Input transcription from server.
    InputTranscription(String),
    /// Output transcription from server.
    OutputTranscription(String),
    /// Model requested tool calls.
    ToolCall(Vec<FunctionCall>),
    /// Server cancelled pending tool calls.
    ToolCallCancelled(Vec<String>),
    /// Model turn is complete (it's the user's turn now).
    TurnComplete,
    /// Model finished generating its full response.
    ///
    /// Fires even if the generation was interrupted — tells you the model's
    /// internal generation pipeline has stopped. Distinct from `TurnComplete`
    /// which is the turn-taking signal.
    GenerationComplete,
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
    /// Session resumption update with handle, resumability, and consumed index.
    SessionResumeUpdate(ResumeInfo),
    /// Server-side voice activity detected (user started speaking).
    VoiceActivityStart,
    /// Server-side voice activity ended (user stopped speaking).
    VoiceActivityEnd,
    /// Token usage metadata from server (for context window tracking).
    ///
    /// Contains full token breakdown: prompt, response, cached, tool-use,
    /// thinking tokens, plus per-modality details.
    Usage(UsageMetadata),
}

/// Session resumption information from the server.
#[derive(Debug, Clone)]
pub struct ResumeInfo {
    /// Opaque handle for session resumption.
    pub handle: String,
    /// Whether the session is currently resumable.
    pub resumable: bool,
    /// Index of the last client message consumed by the server.
    pub last_consumed_index: Option<String>,
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
    /// Send client content (conversation history or context injection).
    SendClientContent {
        /// Conversation turns to include.
        turns: Vec<Content>,
        /// Whether this completes the client's turn.
        turn_complete: bool,
    },
    /// Send video/image data (raw JPEG bytes, will be base64-encoded).
    SendVideo(Vec<u8>),
    /// Update system instruction mid-session (sends client_content with role=system).
    UpdateInstruction(String),
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
    /// Optional broadcast sender to emit `PhaseChanged` events on transitions.
    event_tx: Option<broadcast::Sender<SessionEvent>>,
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
    /// Create new session state (no `PhaseChanged` event emission).
    pub fn new(phase_tx: watch::Sender<SessionPhase>) -> Self {
        Self {
            phase_tx,
            event_tx: None,
            session_id: uuid::Uuid::new_v4().to_string(),
            resume_handle: parking_lot::Mutex::new(None),
            turns: parking_lot::Mutex::new(Vec::new()),
            current_turn: parking_lot::Mutex::new(None),
        }
    }

    /// Create new session state that emits `PhaseChanged` events on transitions.
    pub fn with_events(
        phase_tx: watch::Sender<SessionPhase>,
        event_tx: broadcast::Sender<SessionEvent>,
    ) -> Self {
        Self {
            phase_tx,
            event_tx: Some(event_tx),
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
    ///
    /// If an `event_tx` was provided via [`with_events`](Self::with_events),
    /// a [`SessionEvent::PhaseChanged`] is broadcast after a successful transition.
    pub fn transition_to(&self, to: SessionPhase) -> Result<SessionPhase, SessionError> {
        let from = self.phase();
        if !from.can_transition_to(&to) {
            return Err(SessionError::InvalidTransition { from, to });
        }
        self.phase_tx.send_replace(to);
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(SessionEvent::PhaseChanged(to));
        }
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
// Session traits (for testability and middleware injection)
// ---------------------------------------------------------------------------

/// Write-side of a session — send commands without owning the full handle.
#[async_trait]
pub trait SessionWriter: Send + Sync + 'static {
    /// Send raw PCM16 audio bytes.
    async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError>;
    /// Send a text message.
    async fn send_text(&self, text: String) -> Result<(), SessionError>;
    /// Send tool/function call responses back to the model.
    async fn send_tool_response(&self, responses: Vec<FunctionResponse>) -> Result<(), SessionError>;
    /// Send client content (conversation history or context).
    async fn send_client_content(&self, turns: Vec<Content>, turn_complete: bool) -> Result<(), SessionError>;
    /// Send a video/image frame (raw JPEG bytes).
    async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError>;
    /// Update the system instruction mid-session.
    async fn update_instruction(&self, instruction: String) -> Result<(), SessionError>;
    /// Signal that user speech activity has started.
    async fn signal_activity_start(&self) -> Result<(), SessionError>;
    /// Signal that user speech activity has ended.
    async fn signal_activity_end(&self) -> Result<(), SessionError>;
    /// Gracefully disconnect the session.
    async fn disconnect(&self) -> Result<(), SessionError>;
}

/// Read-side of a session — subscribe to events and observe phase.
pub trait SessionReader: Send + Sync + 'static {
    /// Subscribe to the session event broadcast stream.
    fn subscribe(&self) -> broadcast::Receiver<SessionEvent>;
    /// Returns the current session phase.
    fn phase(&self) -> SessionPhase;
    /// Returns the unique session ID.
    fn session_id(&self) -> &str;
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
        self.send_command(SessionCommand::SendVideo(jpeg_data)).await
    }

    /// Update the system instruction mid-session.
    pub async fn update_instruction(&self, instruction: impl Into<String>) -> Result<(), SessionError> {
        self.send_command(SessionCommand::UpdateInstruction(instruction.into())).await
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
        self.send_command(SessionCommand::SendClientContent { turns, turn_complete })
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

    async fn send_tool_response(&self, responses: Vec<FunctionResponse>) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendToolResponse(responses))
            .await
    }

    async fn send_client_content(&self, turns: Vec<Content>, turn_complete: bool) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendClientContent { turns, turn_complete })
            .await
    }

    async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        self.send_command(SessionCommand::SendVideo(jpeg_data)).await
    }

    async fn update_instruction(&self, instruction: String) -> Result<(), SessionError> {
        self.send_command(SessionCommand::UpdateInstruction(instruction)).await
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

// ---------------------------------------------------------------------------
// Broadcast lag helper
// ---------------------------------------------------------------------------

/// Receive the next event from a broadcast receiver, handling lag gracefully.
///
/// If the receiver falls behind (too slow to keep up with the sender), the
/// skipped events are logged and the next available event is returned.
/// Returns `None` when the channel is closed.
///
/// # Example
///
/// ```ignore
/// let mut events = handle.subscribe();
/// while let Some(event) = recv_event(&mut events).await {
///     // handle event
/// }
/// ```
pub async fn recv_event(rx: &mut broadcast::Receiver<SessionEvent>) -> Option<SessionEvent> {
    loop {
        match rx.recv().await {
            Ok(event) => return Some(event),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                #[cfg(feature = "tracing-support")]
                tracing::warn!(skipped = n, "Event subscriber lagged, skipped {n} events");
                // Without tracing, silently continue
                let _ = n;
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // -----------------------------------------------------------------------
    // WebSocketError Display tests
    // -----------------------------------------------------------------------

    #[test]
    fn websocket_error_connection_refused_display() {
        let err = WebSocketError::ConnectionRefused("host unreachable".into());
        assert_eq!(err.to_string(), "Connection refused: host unreachable");
    }

    #[test]
    fn websocket_error_protocol_error_display() {
        let err = WebSocketError::ProtocolError("invalid frame".into());
        assert_eq!(err.to_string(), "Protocol error: invalid frame");
    }

    #[test]
    fn websocket_error_closed_display() {
        let err = WebSocketError::Closed {
            code: 1006,
            reason: "abnormal closure".into(),
        };
        assert_eq!(
            err.to_string(),
            "Connection closed (code=1006, reason=abnormal closure)"
        );
    }

    // -----------------------------------------------------------------------
    // SetupError Display tests
    // -----------------------------------------------------------------------

    #[test]
    fn setup_error_invalid_model_display() {
        let err = SetupError::InvalidModel("no-such-model".into());
        assert_eq!(err.to_string(), "Invalid model: no-such-model");
    }

    #[test]
    fn setup_error_authentication_failed_display() {
        let err = SetupError::AuthenticationFailed("bad token".into());
        assert_eq!(err.to_string(), "Authentication failed: bad token");
    }

    #[test]
    fn setup_error_server_rejected_display() {
        let err = SetupError::ServerRejected {
            code: Some("400".into()),
            message: "invalid config".into(),
        };
        assert_eq!(err.to_string(), "Server rejected: invalid config");
    }

    #[test]
    fn setup_error_server_rejected_no_code_display() {
        let err = SetupError::ServerRejected {
            code: None,
            message: "closed during setup".into(),
        };
        assert_eq!(err.to_string(), "Server rejected: closed during setup");
    }

    #[test]
    fn setup_error_timeout_display() {
        let err = SetupError::Timeout;
        assert_eq!(err.to_string(), "Setup timed out");
    }

    // -----------------------------------------------------------------------
    // AuthError Display tests
    // -----------------------------------------------------------------------

    #[test]
    fn auth_error_token_expired_display() {
        let err = AuthError::TokenExpired;
        assert_eq!(err.to_string(), "Token expired");
    }

    #[test]
    fn auth_error_token_fetch_failed_display() {
        let err = AuthError::TokenFetchFailed("network error".into());
        assert_eq!(err.to_string(), "Token fetch failed: network error");
    }

    #[test]
    fn auth_error_insufficient_scopes_display() {
        let err = AuthError::InsufficientScopes("cloud-platform".into());
        assert_eq!(err.to_string(), "Insufficient scopes: cloud-platform");
    }

    // -----------------------------------------------------------------------
    // SessionError Display tests
    // -----------------------------------------------------------------------

    #[test]
    fn session_error_websocket_display() {
        let err = SessionError::WebSocket(WebSocketError::ConnectionRefused(
            "host unreachable".into(),
        ));
        assert_eq!(
            err.to_string(),
            "WebSocket error: Connection refused: host unreachable"
        );
    }

    #[test]
    fn session_error_timeout_display() {
        let err = SessionError::Timeout {
            phase: SessionPhase::SetupSent,
            elapsed: Duration::from_secs(15),
        };
        assert_eq!(err.to_string(), "Timeout in SetupSent after 15s");
    }

    #[test]
    fn session_error_timeout_connecting_display() {
        let err = SessionError::Timeout {
            phase: SessionPhase::Connecting,
            elapsed: Duration::from_secs(10),
        };
        assert_eq!(err.to_string(), "Timeout in Connecting after 10s");
    }

    #[test]
    fn session_error_setup_failed_display() {
        let err = SessionError::SetupFailed(SetupError::AuthenticationFailed("bad token".into()));
        assert_eq!(
            err.to_string(),
            "Setup failed: Authentication failed: bad token"
        );
    }

    #[test]
    fn session_error_go_away_with_time_display() {
        let err = SessionError::GoAway {
            time_left: Some(Duration::from_secs(30)),
        };
        assert_eq!(err.to_string(), "Server sent GoAway (time left: Some(30s))");
    }

    #[test]
    fn session_error_go_away_no_time_display() {
        let err = SessionError::GoAway { time_left: None };
        assert_eq!(err.to_string(), "Server sent GoAway (time left: None)");
    }

    #[test]
    fn session_error_auth_display() {
        let err = SessionError::Auth(AuthError::TokenExpired);
        assert_eq!(err.to_string(), "Auth error: Token expired");
    }

    #[test]
    fn session_error_not_connected_display() {
        let err = SessionError::NotConnected;
        assert_eq!(err.to_string(), "Not connected");
    }

    #[test]
    fn session_error_channel_closed_display() {
        let err = SessionError::ChannelClosed;
        assert_eq!(err.to_string(), "Internal channel closed");
    }

    #[test]
    fn session_error_send_queue_full_display() {
        let err = SessionError::SendQueueFull;
        assert_eq!(err.to_string(), "Send queue full");
    }

    #[test]
    fn session_error_invalid_transition_display() {
        let err = SessionError::InvalidTransition {
            from: SessionPhase::Active,
            to: SessionPhase::SetupSent,
        };
        assert_eq!(err.to_string(), "Invalid transition from Active to SetupSent");
    }

    // -----------------------------------------------------------------------
    // SessionWriter / SessionReader trait tests
    // -----------------------------------------------------------------------

    #[test]
    fn session_handle_implements_session_writer() {
        fn assert_impl<T: SessionWriter>() {}
        assert_impl::<SessionHandle>();
    }

    #[test]
    fn session_handle_implements_session_reader() {
        fn assert_impl<T: SessionReader>() {}
        assert_impl::<SessionHandle>();
    }

    #[test]
    fn session_writer_is_object_safe() {
        fn _assert(_: &dyn SessionWriter) {}
    }

    #[test]
    fn session_reader_is_object_safe() {
        fn _assert(_: &dyn SessionReader) {}
    }

    // -----------------------------------------------------------------------
    // PhaseChanged event emission tests
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Clone tests (ensure all error types are Clone)
    // -----------------------------------------------------------------------

    #[test]
    fn error_types_are_clone() {
        let ws_err = WebSocketError::ProtocolError("test".into());
        let _ = ws_err.clone();

        let setup_err = SetupError::InvalidModel("test".into());
        let _ = setup_err.clone();

        let auth_err = AuthError::TokenExpired;
        let _ = auth_err.clone();

        let session_err = SessionError::WebSocket(WebSocketError::ProtocolError("test".into()));
        let _ = session_err.clone();
    }

    // -----------------------------------------------------------------------
    // JoinHandle tracking tests
    // -----------------------------------------------------------------------

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
        assert!(result.is_ok(), "join() should return Ok after task completes");
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

    // -----------------------------------------------------------------------
    // recv_event tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn recv_event_returns_events_normally() {
        let (tx, mut rx) = broadcast::channel(16);

        tx.send(SessionEvent::Connected).unwrap();
        tx.send(SessionEvent::TurnComplete).unwrap();

        let event = recv_event(&mut rx).await;
        assert!(matches!(event, Some(SessionEvent::Connected)));

        let event = recv_event(&mut rx).await;
        assert!(matches!(event, Some(SessionEvent::TurnComplete)));
    }

    #[tokio::test]
    async fn recv_event_returns_none_on_closed_channel() {
        let (tx, mut rx) = broadcast::channel::<SessionEvent>(16);
        drop(tx);

        let event = recv_event(&mut rx).await;
        assert!(event.is_none(), "should return None when channel is closed");
    }

    #[tokio::test]
    async fn recv_event_handles_lag() {
        // Create a tiny broadcast channel (capacity 2)
        let (tx, mut rx) = broadcast::channel(2);

        // Send 4 events — the receiver will lag behind
        for i in 0..4 {
            let _ = tx.send(SessionEvent::TextDelta(format!("msg{i}")));
        }

        // recv_event should skip the lagged events and return the next available
        let event = recv_event(&mut rx).await;
        assert!(event.is_some(), "should get an event after lag");
    }
}
