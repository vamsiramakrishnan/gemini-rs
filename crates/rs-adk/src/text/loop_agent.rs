use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Runs a text agent repeatedly until max iterations or a state predicate.
pub struct LoopTextAgent {
    name: String,
    body: Arc<dyn TextAgent>,
    max: u32,
    until: Option<Arc<dyn Fn(&State) -> bool + Send + Sync>>,
}

impl LoopTextAgent {
    /// Create a new loop agent that repeats up to `max` iterations.
    pub fn new(name: impl Into<String>, body: Arc<dyn TextAgent>, max: u32) -> Self {
        Self {
            name: name.into(),
            body,
            max,
            until: None,
        }
    }

    /// Add a predicate — loop breaks when predicate returns true.
    pub fn until(
        mut self,
        pred: impl Fn(&State) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.until = Some(Arc::new(pred));
        self
    }
}

#[async_trait]
impl TextAgent for LoopTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut last_output = String::new();

        for _iter in 0..self.max {
            last_output = self.body.run(state).await?;

            if let Some(pred) = &self.until {
                if pred(state) {
                    break;
                }
            }
        }

        Ok(last_output)
    }
}
