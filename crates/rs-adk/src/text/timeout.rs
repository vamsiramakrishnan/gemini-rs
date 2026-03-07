use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Wraps an agent with a time limit. Returns `AgentError::Timeout` if exceeded.
pub struct TimeoutTextAgent {
    name: String,
    inner: Arc<dyn TextAgent>,
    timeout: Duration,
}

impl TimeoutTextAgent {
    /// Create a new timeout agent wrapping an inner agent with a time limit.
    pub fn new(
        name: impl Into<String>,
        inner: Arc<dyn TextAgent>,
        timeout: Duration,
    ) -> Self {
        Self {
            name: name.into(),
            inner,
            timeout,
        }
    }
}

#[async_trait]
impl TextAgent for TimeoutTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        match tokio::time::timeout(self.timeout, self.inner.run(state)).await {
            Ok(result) => result,
            Err(_) => Err(AgentError::Timeout),
        }
    }
}
