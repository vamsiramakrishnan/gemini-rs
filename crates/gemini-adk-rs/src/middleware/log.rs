//! Logging middleware for agent and tool lifecycle events.

use async_trait::async_trait;

use gemini_genai_rs::prelude::FunctionCall;

use super::Middleware;
use crate::context::InvocationContext;
use crate::error::{AgentError, ToolError};

/// Logs agent and tool lifecycle events.
///
/// Uses `tracing` macros for structured logging; the events are no-ops
/// until a subscriber is installed.
pub struct LogMiddleware;

impl LogMiddleware {
    /// Create a new log middleware.
    pub fn new() -> Self {
        Self
    }
}

impl Default for LogMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for LogMiddleware {
    fn name(&self) -> &str {
        "log"
    }

    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        tracing::info!("Agent starting");
        Ok(())
    }

    async fn after_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        tracing::info!("Agent completed");
        Ok(())
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        tracing::info!(tool = %call.name, "Tool call starting");
        tracing::debug!(tool = %call.name, args = %call.args, "Tool call args");
        Ok(())
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        tracing::info!(tool = %call.name, "Tool call completed");
        Ok(())
    }

    async fn on_tool_error(&self, call: &FunctionCall, err: &ToolError) -> Result<(), AgentError> {
        tracing::warn!(tool = %call.name, error = %err, "Tool call failed");
        Ok(())
    }

    async fn on_error(&self, err: &AgentError) -> Result<(), AgentError> {
        tracing::error!(error = %err, "Agent error");
        Ok(())
    }
}
