//! Logging plugin — structured logging for agent/tool lifecycle.

use async_trait::async_trait;

use rs_genai::prelude::FunctionCall;

use super::{Plugin, PluginResult};
use crate::context::InvocationContext;
use crate::events::Event;

/// Plugin that logs agent and tool lifecycle events.
///
/// When the `tracing-support` feature is enabled, uses `tracing` macros for
/// structured logging. Without the feature, all hooks are silent no-ops.
pub struct LoggingPlugin;

impl LoggingPlugin {
    /// Create a new logging plugin.
    pub fn new() -> Self {
        Self
    }
}

impl Default for LoggingPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for LoggingPlugin {
    fn name(&self) -> &str {
        "logging"
    }

    async fn before_agent(&self, _ctx: &InvocationContext) -> PluginResult {
        #[cfg(feature = "tracing-support")]
        tracing::info!("[plugin:logging] Agent starting");
        PluginResult::Continue
    }

    async fn after_agent(&self, _ctx: &InvocationContext) -> PluginResult {
        #[cfg(feature = "tracing-support")]
        tracing::info!("[plugin:logging] Agent completed");
        PluginResult::Continue
    }

    async fn before_tool(&self, call: &FunctionCall, _ctx: &InvocationContext) -> PluginResult {
        let _ = call;
        #[cfg(feature = "tracing-support")]
        {
            tracing::info!(tool = %call.name, "[plugin:logging] Tool call starting");
            tracing::debug!(tool = %call.name, args = %call.args, "[plugin:logging] Tool call args");
        }
        PluginResult::Continue
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        _result: &serde_json::Value,
        _ctx: &InvocationContext,
    ) -> PluginResult {
        let _ = call;
        #[cfg(feature = "tracing-support")]
        tracing::info!(tool = %call.name, "[plugin:logging] Tool call completed");
        PluginResult::Continue
    }

    async fn on_event(&self, event: &Event, _ctx: &InvocationContext) -> PluginResult {
        let _ = event;
        #[cfg(feature = "tracing-support")]
        tracing::debug!(
            event_id = %event.id,
            author = %event.author,
            "[plugin:logging] Event emitted"
        );
        PluginResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_plugin_name() {
        let p = LoggingPlugin::new();
        assert_eq!(p.name(), "logging");
    }

    #[test]
    fn logging_plugin_default() {
        let p = LoggingPlugin;
        assert_eq!(p.name(), "logging");
    }
}
