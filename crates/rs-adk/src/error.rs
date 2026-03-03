//! Error types for the agent runtime.

use rs_genai::session::SessionError;

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
