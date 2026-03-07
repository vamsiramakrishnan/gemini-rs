//! Session error types.
//!
//! Structured error hierarchy: [`SessionError`] wraps [`WebSocketError`],
//! [`SetupError`], and [`AuthError`] for fine-grained matching.

use super::state::SessionPhase;
use thiserror::Error;

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
}
