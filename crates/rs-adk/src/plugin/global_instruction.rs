//! Global instruction plugin — injects instructions into every agent request.
//!
//! Mirrors ADK-Python's `global_instruction_plugin`. Prepends or appends
//! global instructions to every LLM request regardless of the agent.

use async_trait::async_trait;

use super::{Plugin, PluginResult};
use crate::context::InvocationContext;

/// Plugin that injects global instructions into every LLM request.
///
/// Useful for enforcing organization-wide policies, safety guidelines,
/// or behavioral constraints across all agents.
pub struct GlobalInstructionPlugin {
    /// Instructions to prepend to the system instruction.
    prepend: Option<String>,
    /// Instructions to append to the system instruction.
    append: Option<String>,
}

impl GlobalInstructionPlugin {
    /// Create a new global instruction plugin.
    pub fn new() -> Self {
        Self {
            prepend: None,
            append: None,
        }
    }

    /// Set instructions to prepend to every system instruction.
    pub fn with_prepend(mut self, instruction: impl Into<String>) -> Self {
        self.prepend = Some(instruction.into());
        self
    }

    /// Set instructions to append to every system instruction.
    pub fn with_append(mut self, instruction: impl Into<String>) -> Self {
        self.append = Some(instruction.into());
        self
    }

    /// Get the prepend instruction, if any.
    pub fn prepend(&self) -> Option<&str> {
        self.prepend.as_deref()
    }

    /// Get the append instruction, if any.
    pub fn append(&self) -> Option<&str> {
        self.append.as_deref()
    }
}

impl Default for GlobalInstructionPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for GlobalInstructionPlugin {
    fn name(&self) -> &str {
        "global_instruction"
    }

    async fn before_model(
        &self,
        _request: &crate::llm::LlmRequest,
        _ctx: &InvocationContext,
    ) -> PluginResult {
        // The actual instruction injection is applied by the runtime
        // when constructing the LLM request. The plugin configuration
        // is read by the request builder.
        PluginResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_empty() {
        let plugin = GlobalInstructionPlugin::new();
        assert!(plugin.prepend().is_none());
        assert!(plugin.append().is_none());
    }

    #[test]
    fn with_instructions() {
        let plugin = GlobalInstructionPlugin::new()
            .with_prepend("Safety first.")
            .with_append("Always be helpful.");
        assert_eq!(plugin.prepend(), Some("Safety first."));
        assert_eq!(plugin.append(), Some("Always be helpful."));
    }

    #[test]
    fn plugin_name() {
        let plugin = GlobalInstructionPlugin::new();
        assert_eq!(plugin.name(), "global_instruction");
    }
}
