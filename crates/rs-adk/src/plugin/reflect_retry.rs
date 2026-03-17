//! Reflect-retry plugin — retries failed tool calls with reflection.
//!
//! Mirrors ADK-Python's `reflect_retry_tool_plugin`. When a tool call
//! fails, injects the error as context and asks the model to retry.

use async_trait::async_trait;

use rs_genai::prelude::FunctionCall;

use super::{Plugin, PluginResult};
use crate::context::InvocationContext;

/// Plugin that handles tool failures by reflecting on errors.
///
/// When a tool call fails, this plugin injects the error message
/// as context so the model can learn from the failure and try
/// a different approach.
pub struct ReflectRetryToolPlugin {
    /// Maximum number of retries per tool call.
    max_retries: u32,
}

impl ReflectRetryToolPlugin {
    /// Create a new reflect-retry plugin with the given max retries.
    pub fn new(max_retries: u32) -> Self {
        Self { max_retries }
    }

    /// Returns the maximum retry count.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }
}

impl Default for ReflectRetryToolPlugin {
    fn default() -> Self {
        Self::new(2)
    }
}

#[async_trait]
impl Plugin for ReflectRetryToolPlugin {
    fn name(&self) -> &str {
        "reflect_retry_tool"
    }

    async fn on_tool_error(
        &self,
        call: &FunctionCall,
        error: &str,
        _ctx: &InvocationContext,
    ) -> PluginResult {
        // Signal that the error should be reflected back to the model
        // for retry. The runtime handles the actual retry loop.
        let reflection = serde_json::json!({
            "tool_error": {
                "tool_name": call.name,
                "error": error,
                "action": "reflect_and_retry",
                "max_retries": self.max_retries
            }
        });

        PluginResult::ShortCircuit(reflection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_retries() {
        let plugin = ReflectRetryToolPlugin::default();
        assert_eq!(plugin.max_retries(), 2);
    }

    #[test]
    fn custom_retries() {
        let plugin = ReflectRetryToolPlugin::new(5);
        assert_eq!(plugin.max_retries(), 5);
    }

    #[test]
    fn plugin_name() {
        let plugin = ReflectRetryToolPlugin::default();
        assert_eq!(plugin.name(), "reflect_retry_tool");
    }

    #[tokio::test]
    async fn on_tool_error_returns_short_circuit() {
        use tokio::sync::broadcast;

        let plugin = ReflectRetryToolPlugin::new(3);

        let (evt_tx, _) = broadcast::channel(16);
        let writer: std::sync::Arc<dyn rs_genai::session::SessionWriter> =
            std::sync::Arc::new(crate::test_helpers::MockWriter);
        let session = crate::agent_session::AgentSession::from_writer(writer, evt_tx);
        let ctx = InvocationContext::new(session);

        let call = FunctionCall {
            name: "search".into(),
            args: serde_json::json!({"query": "test"}),
            id: None,
        };

        let result = plugin.on_tool_error(&call, "connection timeout", &ctx).await;
        assert!(result.is_short_circuit());
    }
}
