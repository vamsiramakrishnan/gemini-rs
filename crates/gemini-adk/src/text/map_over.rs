use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Iterates a single agent over each item in a state list.
/// Reads `state[list_key]`, runs agent per item (setting `state[item_key]`),
/// collects results into `state[output_key]`.
pub struct MapOverTextAgent {
    name: String,
    agent: Arc<dyn TextAgent>,
    list_key: String,
    item_key: String,
    output_key: String,
}

impl MapOverTextAgent {
    /// Create a new map-over agent that iterates over a list in state.
    pub fn new(
        name: impl Into<String>,
        agent: Arc<dyn TextAgent>,
        list_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            agent,
            list_key: list_key.into(),
            item_key: "_item".into(),
            output_key: "_results".into(),
        }
    }

    /// Set the state key for the current item (default: "_item").
    pub fn item_key(mut self, key: impl Into<String>) -> Self {
        self.item_key = key.into();
        self
    }

    /// Set the state key for the output list (default: "_results").
    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = key.into();
        self
    }
}

#[async_trait]
impl TextAgent for MapOverTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let items: Vec<serde_json::Value> = state.get(&self.list_key).unwrap_or_default();

        let mut results = Vec::with_capacity(items.len());

        for item in &items {
            state.set(&self.item_key, item);
            state.set("input", item.to_string());
            let result = self.agent.run(state).await?;
            results.push(result);
        }

        state.set(&self.output_key, &results);
        Ok(results.join("\n"))
    }
}
