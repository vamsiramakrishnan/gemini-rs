use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Runs text agents sequentially. Each agent sees state mutations from
/// previous agents. The final agent's output is the pipeline's output.
pub struct SequentialTextAgent {
    name: String,
    children: Vec<Arc<dyn TextAgent>>,
}

impl SequentialTextAgent {
    /// Create a new sequential agent that runs children in order.
    pub fn new(name: impl Into<String>, children: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            children,
        }
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
            last_output = child.run(state).await?;
            // Feed output as input for the next agent.
            state.set("input", &last_output);
        }
        Ok(last_output)
    }
}
