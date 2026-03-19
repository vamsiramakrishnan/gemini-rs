use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Tries each child agent in sequence. Returns the first successful result.
/// If all fail, returns the last error.
pub struct FallbackTextAgent {
    name: String,
    candidates: Vec<Arc<dyn TextAgent>>,
}

impl FallbackTextAgent {
    /// Create a new fallback agent that tries candidates in order.
    pub fn new(name: impl Into<String>, candidates: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            candidates,
        }
    }
}

#[async_trait]
impl TextAgent for FallbackTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut last_err = AgentError::Other("No candidates in fallback".into());

        for candidate in &self.candidates {
            match candidate.run(state).await {
                Ok(result) => return Ok(result),
                Err(e) => last_err = e,
            }
        }

        Err(last_err)
    }
}
