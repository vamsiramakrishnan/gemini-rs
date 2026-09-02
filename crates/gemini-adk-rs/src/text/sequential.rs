use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::context::AgentEvent;
use crate::error::AgentError;
use crate::middleware::MiddlewareChain;
use crate::state::State;

/// Runs text agents sequentially. Each agent sees state mutations from
/// previous agents. The final agent's output is the pipeline's output.
pub struct SequentialTextAgent {
    name: String,
    children: Vec<Arc<dyn TextAgent>>,
    middleware: MiddlewareChain,
}

impl SequentialTextAgent {
    /// Create a new sequential agent that runs children in order.
    pub fn new(name: impl Into<String>, children: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            children,
            middleware: MiddlewareChain::new(),
        }
    }

    /// Attach a middleware chain. `AgentEvent::AgentStarted` /
    /// `AgentEvent::AgentCompleted` are emitted through it around every child,
    /// so `on_event` observers see each stage of the pipeline.
    pub fn with_middleware_chain(mut self, chain: MiddlewareChain) -> Self {
        self.middleware = chain;
        self
    }
}

#[async_trait]
impl TextAgent for SequentialTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut last_output = String::new();
        for child in &self.children {
            let _ = self
                .middleware
                .run_on_event(&AgentEvent::AgentStarted {
                    name: child.name().to_string(),
                })
                .await;
            last_output = child.run(state).await?;
            let _ = self
                .middleware
                .run_on_event(&AgentEvent::AgentCompleted {
                    name: child.name().to_string(),
                })
                .await;
            // Feed output as input for the next agent.
            let _ = state.set("input", &last_output);
        }
        Ok(last_output)
    }
}
