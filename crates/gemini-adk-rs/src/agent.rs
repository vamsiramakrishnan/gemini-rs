//! The core Agent trait.

use async_trait::async_trait;
use gemini_genai_rs::prelude::Tool;

use crate::context::InvocationContext;
use crate::error::AgentError;

/// The fundamental agent trait. Everything that can process a live session
/// implements this — LLM agents, function agents, pipelines, routers.
#[async_trait]
pub trait Agent: Send + Sync + 'static {
    /// Human-readable name for routing, logging, and debugging.
    fn name(&self) -> &str;

    /// Run this agent on a live session. Returns when the agent is done
    /// (turn complete, transfer, or disconnect).
    async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError>;

    /// Declare tools this agent provides (sent in the setup message).
    fn tools(&self) -> Vec<Tool> {
        vec![]
    }

    /// Sub-agents this agent can transfer control to.
    fn sub_agents(&self) -> Vec<std::sync::Arc<dyn Agent>> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verify the trait is object-safe
    fn _assert_object_safe(_: &dyn Agent) {}

    #[test]
    fn agent_trait_is_object_safe() {
        // If this compiles, Agent is object-safe
    }
}
