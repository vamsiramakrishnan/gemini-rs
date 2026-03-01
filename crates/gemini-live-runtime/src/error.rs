//! Error types for the agent runtime.

use gemini_live_wire::session::SessionError;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Session error: {0}")]
    Session(#[from] SessionError),

    #[error("Tool error: {0}")]
    Tool(#[from] ToolError),

    #[error("Unknown agent: {0}")]
    UnknownAgent(String),

    #[error("Transfer requested to agent: {0}")]
    TransferRequested(String),

    #[error("Agent transfer failed: {0}")]
    TransferFailed(String),

    #[error("Agent session closed")]
    SessionClosed,

    #[error("Timeout")]
    Timeout,

    #[error("{0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Tool not found: {0}")]
    NotFound(String),

    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("Tool cancelled")]
    Cancelled,

    #[error("Tool execution timed out after {0:?}")]
    Timeout(std::time::Duration),

    #[error("{0}")]
    Other(String),
}
