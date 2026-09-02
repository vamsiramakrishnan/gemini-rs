use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::context::AgentEvent;
use crate::error::AgentError;
use crate::middleware::MiddlewareChain;
use crate::state::State;

/// Runs text agents concurrently. All branches share state. Results are
/// collected and joined with newlines.
pub struct ParallelTextAgent {
    name: String,
    branches: Vec<Arc<dyn TextAgent>>,
    middleware: MiddlewareChain,
}

impl ParallelTextAgent {
    /// Create a new parallel agent that runs branches concurrently.
    pub fn new(name: impl Into<String>, branches: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            branches,
            middleware: MiddlewareChain::new(),
        }
    }

    /// Attach a middleware chain. `AgentEvent::AgentStarted` is emitted
    /// through it as each branch is spawned and `AgentEvent::AgentCompleted`
    /// as each branch is joined (in branch order), so `on_event` observers see
    /// the fan-out and fan-in.
    pub fn with_middleware_chain(mut self, chain: MiddlewareChain) -> Self {
        self.middleware = chain;
        self
    }
}

#[async_trait]
impl TextAgent for ParallelTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut handles = Vec::with_capacity(self.branches.len());

        for branch in &self.branches {
            let _ = self
                .middleware
                .run_on_event(&AgentEvent::AgentStarted {
                    name: branch.name().to_string(),
                })
                .await;
            let branch = branch.clone();
            let state = state.clone();
            handles.push(tokio::spawn(async move { branch.run(&state).await }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for (branch, handle) in self.branches.iter().zip(handles) {
            let result = handle
                .await
                .map_err(|e| AgentError::Other(format!("Join error: {e}")))?;
            results.push(result?);
            let _ = self
                .middleware
                .run_on_event(&AgentEvent::AgentCompleted {
                    name: branch.name().to_string(),
                })
                .await;
        }

        let combined = results.join("\n");
        let _ = state.set("output", &combined);
        Ok(combined)
    }
}
