use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::context::AgentEvent;
use crate::error::AgentError;
use crate::middleware::MiddlewareChain;
use crate::state::State;

/// Runs a text agent repeatedly until max iterations or a state predicate.
pub struct LoopTextAgent {
    name: String,
    body: Arc<dyn TextAgent>,
    max: u32,
    until: Option<Arc<dyn Fn(&State) -> bool + Send + Sync>>,
    middleware: MiddlewareChain,
}

impl LoopTextAgent {
    /// Create a new loop agent that repeats up to `max` iterations.
    pub fn new(name: impl Into<String>, body: Arc<dyn TextAgent>, max: u32) -> Self {
        Self {
            name: name.into(),
            body,
            max,
            until: None,
            middleware: MiddlewareChain::new(),
        }
    }

    /// Add a predicate — loop breaks when predicate returns true.
    pub fn until(mut self, pred: impl Fn(&State) -> bool + Send + Sync + 'static) -> Self {
        self.until = Some(Arc::new(pred));
        self
    }

    /// Attach a middleware chain. `AgentEvent::LoopIteration` is emitted through
    /// it on every iteration, so `on_event` observers (e.g. `M::on_loop`) fire.
    pub fn with_middleware_chain(mut self, chain: MiddlewareChain) -> Self {
        self.middleware = chain;
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

        for iter in 0..self.max {
            let _ = self
                .middleware
                .run_on_event(&AgentEvent::LoopIteration { iteration: iter })
                .await;

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
