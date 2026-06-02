use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::context::AgentEvent;
use crate::error::AgentError;
use crate::middleware::MiddlewareChain;
use crate::state::State;

/// Tries each child agent in sequence. Returns the first successful result.
/// If all fail, returns the last error.
pub struct FallbackTextAgent {
    name: String,
    candidates: Vec<Arc<dyn TextAgent>>,
    middleware: MiddlewareChain,
}

impl FallbackTextAgent {
    /// Create a new fallback agent that tries candidates in order.
    pub fn new(name: impl Into<String>, candidates: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            candidates,
            middleware: MiddlewareChain::new(),
        }
    }

    /// Attach a middleware chain. `AgentEvent::FallbackActivated` is emitted
    /// through it when a fallback branch (any candidate after the first) is
    /// tried, so `on_event` observers (`M::on_fallback`) fire.
    pub fn with_middleware_chain(mut self, chain: MiddlewareChain) -> Self {
        self.middleware = chain;
        self
    }
}

#[async_trait]
impl TextAgent for FallbackTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut last_err = AgentError::Other("No candidates in fallback".into());

        for (i, candidate) in self.candidates.iter().enumerate() {
            // The first candidate is the primary; subsequent ones are fallbacks.
            if i > 0 {
                let _ = self
                    .middleware
                    .run_on_event(&AgentEvent::FallbackActivated {
                        agent_name: candidate.name().to_string(),
                    })
                    .await;
            }
            match candidate.run(state).await {
                Ok(result) => return Ok(result),
                Err(e) => last_err = e,
            }
        }

        Err(last_err)
    }
}
