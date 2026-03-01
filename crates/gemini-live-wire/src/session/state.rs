//! Session phase finite state machine with validated transitions.
//!
//! Invalid transitions return `Err(SessionError::InvalidTransition)`.
//! The phase is observable via a `watch::Receiver<SessionPhase>` channel.

use std::fmt;

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
