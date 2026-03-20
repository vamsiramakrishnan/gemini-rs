//! Error types for the agent runtime.

use gemini_genai_rs::session::SessionError;

/// Errors that can occur during agent execution.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// A wire-level session error (WebSocket, auth, setup).
    #[error("Session error: {0}")]
    Session(#[from] SessionError),

    /// A tool execution error.
    #[error("Tool error: {0}")]
    Tool(#[from] ToolError),

    /// The requested agent was not found in the registry.
    #[error("Unknown agent: {0}")]
    UnknownAgent(String),

    /// The agent requested a transfer to another agent.
    #[error("Transfer requested to agent: {0}")]
    TransferRequested(String),

    /// An agent transfer was attempted but failed.
    #[error("Agent transfer failed: {0}")]
    TransferFailed(String),

    /// The underlying session has been closed.
    #[error("Agent session closed")]
    SessionClosed,

    /// The operation timed out.
    #[error("Timeout")]
    Timeout,

    /// A configuration error.
    #[error("Configuration error: {0}")]
    Config(String),

    /// A catch-all for other errors.
    #[error("{0}")]
    Other(String),
}

/// Errors that can occur during tool execution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolError {
    /// The tool's execution logic failed.
    #[error("Tool execution failed: {0}")]
    ExecutionFailed(String),

    /// No tool with this name is registered.
    #[error("Tool not found: {0}")]
    NotFound(String),

    /// The arguments provided to the tool were invalid.
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),

    /// The tool call was cancelled before completion.
    #[error("Tool cancelled")]
    Cancelled,

    /// The tool call exceeded its timeout.
    #[error("Tool execution timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// A catch-all for other tool errors.
    #[error("{0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn agent_error_display_messages() {
        let err = AgentError::UnknownAgent("foo".into());
        assert_eq!(err.to_string(), "Unknown agent: foo");

        let err = AgentError::TransferRequested("bar".into());
        assert_eq!(err.to_string(), "Transfer requested to agent: bar");

        let err = AgentError::TransferFailed("baz".into());
        assert_eq!(err.to_string(), "Agent transfer failed: baz");

        let err = AgentError::SessionClosed;
        assert_eq!(err.to_string(), "Agent session closed");

        let err = AgentError::Timeout;
        assert_eq!(err.to_string(), "Timeout");

        let err = AgentError::Config("bad value".into());
        assert_eq!(err.to_string(), "Configuration error: bad value");

        let err = AgentError::Other("something".into());
        assert_eq!(err.to_string(), "something");
    }

    #[test]
    fn agent_error_from_session_error() {
        use gemini_genai_rs::session::SessionError;
        use gemini_genai_rs::session::WebSocketError;

        let ws_err = SessionError::WebSocket(WebSocketError::ConnectionRefused("refused".into()));
        let agent_err: AgentError = ws_err.into();
        let msg = agent_err.to_string();
        assert!(msg.contains("Session error"), "got: {msg}");
    }

    #[test]
    fn agent_error_from_tool_error() {
        let tool_err = ToolError::NotFound("my_tool".into());
        let agent_err: AgentError = tool_err.into();
        let msg = agent_err.to_string();
        assert!(msg.contains("Tool error"), "got: {msg}");
        assert!(msg.contains("my_tool"), "got: {msg}");
    }

    #[test]
    fn tool_error_display_messages() {
        assert_eq!(
            ToolError::ExecutionFailed("boom".into()).to_string(),
            "Tool execution failed: boom"
        );
        assert_eq!(
            ToolError::NotFound("x".into()).to_string(),
            "Tool not found: x"
        );
        assert_eq!(
            ToolError::InvalidArgs("bad".into()).to_string(),
            "Invalid arguments: bad"
        );
        assert_eq!(ToolError::Cancelled.to_string(), "Tool cancelled");
        assert_eq!(ToolError::Other("misc".into()).to_string(), "misc");
    }

    #[test]
    fn tool_error_timeout_shows_duration() {
        let err = ToolError::Timeout(Duration::from_secs(5));
        let msg = err.to_string();
        assert!(msg.contains("5s"), "got: {msg}");
        assert!(msg.contains("timed out"), "got: {msg}");
    }

    #[test]
    fn tool_error_is_clone() {
        let err = ToolError::ExecutionFailed("test".into());
        let cloned = err.clone();
        assert_eq!(err.to_string(), cloned.to_string());

        let err2 = ToolError::Timeout(Duration::from_millis(100));
        let cloned2 = err2.clone();
        assert_eq!(err2.to_string(), cloned2.to_string());
    }
}
