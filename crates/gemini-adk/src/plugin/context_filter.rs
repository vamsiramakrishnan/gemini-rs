//! Context filter plugin — filters conversation context before model calls.
//!
//! Mirrors ADK-Python's `context_filter_plugin`. Allows trimming,
//! filtering, or transforming the conversation history before it
//! reaches the model.

use async_trait::async_trait;

use super::{Plugin, PluginResult};
use crate::context::InvocationContext;

/// Plugin that filters or transforms conversation context before model calls.
///
/// Can be used to:
/// - Limit context window size
/// - Remove tool call/result turns
/// - Filter out specific content types
/// - Inject additional context
pub struct ContextFilterPlugin {
    /// Maximum number of turns to keep in context.
    max_turns: Option<usize>,
    /// Whether to exclude tool call/result turns from context.
    exclude_tool_turns: bool,
}

impl ContextFilterPlugin {
    /// Create a new context filter plugin.
    pub fn new() -> Self {
        Self {
            max_turns: None,
            exclude_tool_turns: false,
        }
    }

    /// Set the maximum number of turns to keep.
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// Set whether to exclude tool call/result turns.
    pub fn with_exclude_tool_turns(mut self, exclude: bool) -> Self {
        self.exclude_tool_turns = exclude;
        self
    }

    /// Returns the configured max turns limit.
    pub fn max_turns(&self) -> Option<usize> {
        self.max_turns
    }

    /// Returns whether tool turns are excluded.
    pub fn exclude_tool_turns(&self) -> bool {
        self.exclude_tool_turns
    }
}

impl Default for ContextFilterPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ContextFilterPlugin {
    fn name(&self) -> &str {
        "context_filter"
    }

    async fn before_model(
        &self,
        _request: &crate::llm::LlmRequest,
        _ctx: &InvocationContext,
    ) -> PluginResult {
        // The actual context filtering is applied by the runtime
        // when constructing the LLM request, using this plugin's
        // configuration. The plugin signals Continue to allow
        // the (already filtered) request to proceed.
        PluginResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let plugin = ContextFilterPlugin::new();
        assert!(plugin.max_turns().is_none());
        assert!(!plugin.exclude_tool_turns());
    }

    #[test]
    fn custom_config() {
        let plugin = ContextFilterPlugin::new()
            .with_max_turns(10)
            .with_exclude_tool_turns(true);
        assert_eq!(plugin.max_turns(), Some(10));
        assert!(plugin.exclude_tool_turns());
    }

    #[test]
    fn plugin_name() {
        let plugin = ContextFilterPlugin::new();
        assert_eq!(plugin.name(), "context_filter");
    }
}
