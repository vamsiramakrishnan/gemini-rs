//! Logging middleware for agent and tool lifecycle events.

use async_trait::async_trait;

use gemini_live::prelude::FunctionCall;

use super::Middleware;
use crate::context::InvocationContext;
use crate::error::{AgentError, ToolError};

/// Logs agent and tool lifecycle events.
///
/// When the `tracing-support` feature is enabled, uses `tracing` macros for
/// structured logging. Without the feature, all hooks are silent no-ops.
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
        #[cfg(feature = "tracing-support")]
        tracing::info!("Agent starting");
        Ok(())
    }

    async fn after_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        #[cfg(feature = "tracing-support")]
        tracing::info!("Agent completed");
        Ok(())
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        let _ = call; // used only when tracing-support is enabled
        #[cfg(feature = "tracing-support")]
        {
            tracing::info!(tool = %call.name, "Tool call starting");
            tracing::debug!(tool = %call.name, args = %call.args, "Tool call args");
        }
        Ok(())
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        let _ = call;
        #[cfg(feature = "tracing-support")]
        tracing::info!(tool = %call.name, "Tool call completed");
        Ok(())
    }

    async fn on_tool_error(&self, call: &FunctionCall, err: &ToolError) -> Result<(), AgentError> {
        let _ = (call, err);
        #[cfg(feature = "tracing-support")]
        tracing::warn!(tool = %call.name, error = %err, "Tool call failed");
        Ok(())
    }

    async fn on_error(&self, err: &AgentError) -> Result<(), AgentError> {
        let _ = err;
        #[cfg(feature = "tracing-support")]
        tracing::error!(error = %err, "Agent error");
        Ok(())
    }
}
