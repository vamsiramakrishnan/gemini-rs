//! Agent-level observability — OpenTelemetry tracing, structured logging, Prometheus metrics.
//!
//! Mirrors the wire crate's telemetry layer but for agent lifecycle operations.
//! All components are feature-gated for zero overhead when disabled:
//! - `tracing-support`: OTel spans and structured logging
//! - `metrics`: Prometheus metric definitions

use async_trait::async_trait;

use rs_genai::prelude::FunctionCall;

use crate::context::InvocationContext;
use crate::error::{AgentError, ToolError};
use crate::llm::{LlmRequest, LlmResponse};
use crate::middleware::Middleware;

pub mod logging;
pub mod metrics;
pub mod setup;
pub mod spans;

pub use setup::TelemetrySetup;

/// Auto-registered middleware that calls telemetry functions.
/// Zero-overhead when `tracing-support` and `metrics` features are disabled
/// (all telemetry functions compile to no-ops).
///
/// Automatically prepended to every LlmAgent's middleware chain at build time,
/// so all agents get observability by default.
pub struct TelemetryMiddleware {
    agent_name: String,
}

impl TelemetryMiddleware {
    /// Create a new telemetry middleware for the given agent.
    pub fn new(agent_name: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
        }
    }

    /// Returns the agent name this middleware is tracking.
    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }
}

#[async_trait]
impl Middleware for TelemetryMiddleware {
    fn name(&self) -> &str {
        "telemetry"
    }

    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        metrics::record_agent_started(&self.agent_name);
        logging::log_agent_started(&self.agent_name, 0);
        Ok(())
    }

    async fn after_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        // Duration tracked in LlmAgent::run_live() directly
        Ok(())
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        metrics::record_agent_tool_dispatched(&self.agent_name, &call.name);
        logging::log_tool_dispatch(&self.agent_name, &call.name, "function");
        Ok(())
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        logging::log_tool_result(&self.agent_name, &call.name, true, 0.0);
        Ok(())
    }

    async fn on_tool_error(&self, call: &FunctionCall, _err: &ToolError) -> Result<(), AgentError> {
        logging::log_tool_result(&self.agent_name, &call.name, false, 0.0);
        Ok(())
    }

    async fn on_error(&self, err: &AgentError) -> Result<(), AgentError> {
        metrics::record_agent_error(&self.agent_name, &err.to_string());
        logging::log_agent_error(&self.agent_name, &err.to_string());
        Ok(())
    }

    async fn before_model(&self, _request: &LlmRequest) -> Result<Option<LlmResponse>, AgentError> {
        logging::log_tool_dispatch(&self.agent_name, "llm", "model_call");
        Ok(None)
    }

    async fn after_model(&self, _request: &LlmRequest, _response: &LlmResponse) -> Result<Option<LlmResponse>, AgentError> {
        if let Some(_usage) = &_response.usage {
            metrics::record_agent_completed(&self.agent_name, 0.0); // Duration tracked elsewhere
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn telemetry_middleware_hooks_dont_panic() {
        let mw = TelemetryMiddleware::new("test_agent");
        assert_eq!(mw.name(), "telemetry");
        assert_eq!(mw.agent_name(), "test_agent");

        let call = FunctionCall {
            name: "my_tool".to_string(),
            args: serde_json::json!({}),
            id: None,
        };
        let result = serde_json::json!({"ok": true});
        let tool_err = ToolError::ExecutionFailed("boom".to_string());
        let agent_err = AgentError::Other("oops".to_string());

        // All hooks should complete without panic
        assert!(mw.before_tool(&call).await.is_ok());
        assert!(mw.after_tool(&call, &result).await.is_ok());
        assert!(mw.on_tool_error(&call, &tool_err).await.is_ok());
        assert!(mw.on_error(&agent_err).await.is_ok());
    }
}
