//! Error types for the agent runtime.

use gemini_genai_rs::session::SessionError;

/// Convenience alias for fallible agent-runtime operations.
///
/// Lets call sites write `AgentResult<T>` instead of the more verbose
/// `Result<T, AgentError>`.
pub type AgentResult<T> = std::result::Result<T, AgentError>;

/// Errors that can occur during agent execution.
///
/// A model failure arrives as [`AgentError::Llm`] with its kind intact, so a
/// caller can branch on it:
///
/// ```
/// use gemini_adk_rs::error::AgentError;
/// use gemini_adk_rs::llm::LlmError;
///
/// let err = AgentError::from(LlmError::Api { status: 429, message: "slow down".into() });
/// assert!(err.as_llm().is_some_and(LlmError::is_rate_limited));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    /// A wire-level session error (WebSocket, auth, setup).
    #[error("Session error: {0}")]
    Session(#[from] SessionError),

    /// A tool execution error.
    #[error("Tool error: {0}")]
    Tool(#[from] ToolError),

    /// The model call failed; the [`LlmError`](crate::llm::LlmError) says how.
    #[error("model call failed: {0}")]
    Llm(#[from] crate::llm::LlmError),

    /// Reading or writing session state failed.
    #[error(transparent)]
    State(#[from] crate::state::StateError),

    /// The model's reply did not match the requested output type, even after
    /// it was asked to correct it.
    #[error("the model's reply is not a valid {expected}: {reason}")]
    InvalidOutput {
        /// The Rust type the reply had to deserialize into.
        expected: &'static str,
        /// Why it did not.
        reason: String,
        /// The reply as received.
        text: String,
    },

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

impl AgentError {
    /// The model error behind this one, if a model call failed.
    pub fn as_llm(&self) -> Option<&crate::llm::LlmError> {
        match self {
            Self::Llm(e) => Some(e),
            _ => None,
        }
    }
}

/// A build-time configuration error: one or more problems found while
/// validating user-supplied configuration (a [`Flow`](crate::flow::Flow), a
/// [`PhaseMachine`](crate::live::PhaseMachine), a
/// [`ComputedRegistry`](crate::live::ComputedRegistry), …).
///
/// Every issue is reported, not just the first; `Display` joins them with
/// `"; "`. Converts into [`AgentError::Config`] via `?`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", issues.join("; "))]
pub struct ConfigError {
    /// Every problem found, in discovery order. Never empty.
    pub issues: Vec<String>,
}

impl ConfigError {
    /// A single-issue error.
    pub fn new(issue: impl Into<String>) -> Self {
        Self {
            issues: vec![issue.into()],
        }
    }

    /// Collect a list of issues into an error, or `Ok(())` when there are none.
    pub fn from_issues(issues: Vec<String>) -> Result<(), Self> {
        if issues.is_empty() {
            Ok(())
        } else {
            Err(Self { issues })
        }
    }
}

impl From<ConfigError> for AgentError {
    fn from(err: ConfigError) -> Self {
        AgentError::Config(err.to_string())
    }
}

/// Errors that can occur during tool execution.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
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

    /// A confirmation-gated call was declined; carries the reason given, so
    /// the model can tell the user why and what to do instead.
    #[error("Tool call declined: {0}")]
    Declined(String),

    /// The tool call exceeded its timeout.
    #[error("Tool execution timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// A catch-all for other tool errors.
    #[error("{0}")]
    Other(String),
}

impl ToolError {
    /// Turn any error into a `ToolError`, the way `?` would if it could.
    ///
    /// A `ToolError` passes through unchanged, so a tool that returns
    /// `ToolError::InvalidArgs` keeps that meaning. Anything else — an
    /// `io::Error`, a `reqwest::Error`, a `String`, an `anyhow::Error` —
    /// becomes [`ToolError::ExecutionFailed`] carrying its message, which is
    /// what the model is shown.
    ///
    /// ```
    /// use gemini_adk_rs::error::ToolError;
    ///
    /// let io = std::io::Error::other("disk full");
    /// assert!(matches!(ToolError::from_error(io), ToolError::ExecutionFailed(m) if m == "disk full"));
    /// assert!(matches!(ToolError::from_error(ToolError::Cancelled), ToolError::Cancelled));
    /// assert!(matches!(ToolError::from_error("no such city"), ToolError::ExecutionFailed(_)));
    /// ```
    pub fn from_error(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        match error.into().downcast::<ToolError>() {
            Ok(tool_error) => *tool_error,
            Err(other) => ToolError::ExecutionFailed(other.to_string()),
        }
    }
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
    fn config_error_joins_issues_and_converts() {
        let err = ConfigError {
            issues: vec!["a".into(), "b".into()],
        };
        assert_eq!(err.to_string(), "a; b");
        assert!(ConfigError::from_issues(vec![]).is_ok());
        let agent_err: AgentError = ConfigError::new("bad").into();
        assert_eq!(agent_err.to_string(), "Configuration error: bad");
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
