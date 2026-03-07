use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Runs text agents concurrently. All branches share state. Results are
/// collected and joined with newlines.
pub struct ParallelTextAgent {
    name: String,
    branches: Vec<Arc<dyn TextAgent>>,
}

impl ParallelTextAgent {
    /// Create a new parallel agent that runs branches concurrently.
    pub fn new(name: impl Into<String>, branches: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            branches,
        }
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
            let branch = branch.clone();
            let state = state.clone();
            handles.push(tokio::spawn(async move { branch.run(&state).await }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            let result = handle
                .await
                .map_err(|e| AgentError::Other(format!("Join error: {e}")))?;
            results.push(result?);
        }

        let combined = results.join("\n");
        state.set("output", &combined);
        Ok(combined)
    }
}
