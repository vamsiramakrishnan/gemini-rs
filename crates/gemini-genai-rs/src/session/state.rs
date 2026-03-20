//! Session phase finite state machine and shared session state.
//!
//! [`SessionPhase`] — lifecycle phase enum with validated transitions.
//! [`SessionState`] — shared state struct (phase, turns, resume handle).
//!
//! Invalid transitions return `Err(SessionError::InvalidTransition)`.
//! The phase is observable via a `watch::Receiver<SessionPhase>` channel.

use std::fmt;
use std::time::Instant;
use tokio::sync::{broadcast, watch};

use super::errors::SessionError;
use super::events::{SessionEvent, Turn};

/// The lifecycle phase of a Gemini Live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionPhase {
    /// Not connected to the server.
    Disconnected,
    /// WebSocket connection in progress.
    Connecting,
    /// Setup message sent, awaiting setupComplete.
    SetupSent,
    /// Session is active and ready for interaction.
    Active,
    /// User is currently speaking (client VAD or server signal).
    UserSpeaking,
    /// Model is currently generating a response.
    ModelSpeaking,
    /// Model was interrupted by user barge-in.
    Interrupted,
    /// Model requested tool calls, awaiting dispatch.
    ToolCallPending,
    /// Tool calls are executing.
    ToolCallExecuting,
    /// Session is shutting down gracefully.
    Disconnecting,
}

impl fmt::Display for SessionPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => write!(f, "Disconnected"),
            Self::Connecting => write!(f, "Connecting"),
            Self::SetupSent => write!(f, "SetupSent"),
            Self::Active => write!(f, "Active"),
            Self::UserSpeaking => write!(f, "UserSpeaking"),
            Self::ModelSpeaking => write!(f, "ModelSpeaking"),
            Self::Interrupted => write!(f, "Interrupted"),
            Self::ToolCallPending => write!(f, "ToolCallPending"),
            Self::ToolCallExecuting => write!(f, "ToolCallExecuting"),
            Self::Disconnecting => write!(f, "Disconnecting"),
        }
    }
}

impl SessionPhase {
    /// Check whether a transition from this phase to `to` is valid.
    pub fn can_transition_to(&self, to: &SessionPhase) -> bool {
        matches!(
            (self, to),
            // Connection lifecycle
            (SessionPhase::Disconnected, SessionPhase::Connecting)
                | (SessionPhase::Connecting, SessionPhase::SetupSent)
                | (SessionPhase::SetupSent, SessionPhase::Active)
                // Conversation flow
                | (SessionPhase::Active, SessionPhase::UserSpeaking)
                | (SessionPhase::Active, SessionPhase::ModelSpeaking)
                | (SessionPhase::Active, SessionPhase::ToolCallPending)
                // User speaking transitions
                | (SessionPhase::UserSpeaking, SessionPhase::Active)
                | (SessionPhase::UserSpeaking, SessionPhase::ModelSpeaking)
                // Model speaking transitions
                | (SessionPhase::ModelSpeaking, SessionPhase::Active)
                | (SessionPhase::ModelSpeaking, SessionPhase::Interrupted)
                | (SessionPhase::ModelSpeaking, SessionPhase::ToolCallPending)
                // Barge-in recovery
                | (SessionPhase::Interrupted, SessionPhase::Active)
                | (SessionPhase::Interrupted, SessionPhase::UserSpeaking)
                // Tool flow
                | (SessionPhase::ToolCallPending, SessionPhase::ToolCallExecuting)
                | (SessionPhase::ToolCallExecuting, SessionPhase::Active)
                | (SessionPhase::ToolCallExecuting, SessionPhase::ModelSpeaking)
                // Graceful shutdown
                | (SessionPhase::Active, SessionPhase::Disconnecting)
                | (SessionPhase::UserSpeaking, SessionPhase::Disconnecting)
                | (SessionPhase::ModelSpeaking, SessionPhase::Disconnecting)
                | (SessionPhase::Interrupted, SessionPhase::Disconnecting)
                | (SessionPhase::ToolCallPending, SessionPhase::Disconnecting)
                | (SessionPhase::ToolCallExecuting, SessionPhase::Disconnecting)
                | (SessionPhase::Disconnecting, SessionPhase::Disconnected)
                // Force-disconnect from any state
                | (_, SessionPhase::Disconnected)
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_connection_lifecycle() {
        assert!(SessionPhase::Disconnected.can_transition_to(&SessionPhase::Connecting));
        assert!(SessionPhase::Connecting.can_transition_to(&SessionPhase::SetupSent));
        assert!(SessionPhase::SetupSent.can_transition_to(&SessionPhase::Active));
    }

    #[test]
    fn valid_conversation_flow() {
        assert!(SessionPhase::Active.can_transition_to(&SessionPhase::UserSpeaking));
        assert!(SessionPhase::Active.can_transition_to(&SessionPhase::ModelSpeaking));
        assert!(SessionPhase::UserSpeaking.can_transition_to(&SessionPhase::Active));
        assert!(SessionPhase::ModelSpeaking.can_transition_to(&SessionPhase::Active));
    }

    #[test]
    fn valid_barge_in() {
        assert!(SessionPhase::ModelSpeaking.can_transition_to(&SessionPhase::Interrupted));
        assert!(SessionPhase::Interrupted.can_transition_to(&SessionPhase::Active));
        assert!(SessionPhase::Interrupted.can_transition_to(&SessionPhase::UserSpeaking));
    }

    #[test]
    fn valid_tool_flow() {
        assert!(SessionPhase::Active.can_transition_to(&SessionPhase::ToolCallPending));
        assert!(SessionPhase::ModelSpeaking.can_transition_to(&SessionPhase::ToolCallPending));
        assert!(SessionPhase::ToolCallPending.can_transition_to(&SessionPhase::ToolCallExecuting));
        assert!(SessionPhase::ToolCallExecuting.can_transition_to(&SessionPhase::Active));
        assert!(SessionPhase::ToolCallExecuting.can_transition_to(&SessionPhase::ModelSpeaking));
    }

    #[test]
    fn valid_disconnect_from_any() {
        let phases = [
            SessionPhase::Disconnected,
            SessionPhase::Connecting,
            SessionPhase::SetupSent,
            SessionPhase::Active,
            SessionPhase::UserSpeaking,
            SessionPhase::ModelSpeaking,
            SessionPhase::Interrupted,
            SessionPhase::ToolCallPending,
            SessionPhase::ToolCallExecuting,
            SessionPhase::Disconnecting,
        ];

        for phase in &phases {
            assert!(
                phase.can_transition_to(&SessionPhase::Disconnected),
                "{phase} should be able to force-disconnect"
            );
        }
    }

    #[test]
    fn invalid_transitions() {
        assert!(!SessionPhase::Disconnected.can_transition_to(&SessionPhase::Active));
        assert!(!SessionPhase::Connecting.can_transition_to(&SessionPhase::Active));
        assert!(!SessionPhase::Active.can_transition_to(&SessionPhase::SetupSent));
        assert!(!SessionPhase::UserSpeaking.can_transition_to(&SessionPhase::ToolCallExecuting));
        assert!(!SessionPhase::Disconnecting.can_transition_to(&SessionPhase::Active));
    }

    #[test]
    fn display_impl() {
        assert_eq!(format!("{}", SessionPhase::Active), "Active");
        assert_eq!(format!("{}", SessionPhase::ModelSpeaking), "ModelSpeaking");
    }
}
