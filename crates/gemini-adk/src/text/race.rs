use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Runs agents concurrently, returns the first to complete. Cancels the rest.
pub struct RaceTextAgent {
    name: String,
    agents: Vec<Arc<dyn TextAgent>>,
}

impl RaceTextAgent {
    /// Create a new race agent that runs agents concurrently and returns the first result.
    pub fn new(name: impl Into<String>, agents: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            agents,
        }
    }
}

#[async_trait]
impl TextAgent for RaceTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        if self.agents.is_empty() {
            return Err(AgentError::Other("No agents in race".into()));
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<String, AgentError>>(1);
        let cancel = tokio_util::sync::CancellationToken::new();

        let mut handles = Vec::with_capacity(self.agents.len());
        for agent in &self.agents {
            let agent = agent.clone();
            let state = state.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();

            handles.push(tokio::spawn(async move {
                tokio::select! {
                    result = agent.run(&state) => {
                        let _ = tx.send(result).await;
                    }
                    _ = cancel.cancelled() => {}
                }
            }));
        }
        drop(tx); // Close our sender so rx completes when all are done.

        let result = rx
            .recv()
            .await
            .unwrap_or(Err(AgentError::Other("All race agents failed".into())));

        // Cancel remaining agents.
        cancel.cancel();
        for handle in handles {
            handle.abort();
        }

        result
    }
}
