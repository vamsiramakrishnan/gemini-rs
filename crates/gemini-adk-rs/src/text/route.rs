use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::context::AgentEvent;
use crate::error::AgentError;
use crate::middleware::MiddlewareChain;
use crate::state::State;

/// A routing rule: predicate over state → target agent.
pub struct RouteRule {
    predicate: Box<dyn Fn(&State) -> bool + Send + Sync>,
    agent: Arc<dyn TextAgent>,
}

impl RouteRule {
    /// Create a new route rule with a predicate and target agent.
    pub fn new(
        predicate: impl Fn(&State) -> bool + Send + Sync + 'static,
        agent: Arc<dyn TextAgent>,
    ) -> Self {
        Self {
            predicate: Box::new(predicate),
            agent,
        }
    }
}

/// State-driven deterministic branching — evaluates predicates in order,
/// dispatches to the first matching agent. Falls back to default if none match.
pub struct RouteTextAgent {
    name: String,
    rules: Vec<RouteRule>,
    default: Arc<dyn TextAgent>,
    middleware: MiddlewareChain,
}

impl RouteTextAgent {
    /// Create a new route agent with rules and a default fallback.
    pub fn new(
        name: impl Into<String>,
        rules: Vec<RouteRule>,
        default: Arc<dyn TextAgent>,
    ) -> Self {
        Self {
            name: name.into(),
            rules,
            default,
            middleware: MiddlewareChain::new(),
        }
    }

    /// Attach a middleware chain. `AgentEvent::RouteSelected` is emitted through
    /// it with the chosen branch, so `on_event` observers (`M::on_route`) fire.
    pub fn with_middleware_chain(mut self, chain: MiddlewareChain) -> Self {
        self.middleware = chain;
        self
    }
}

#[async_trait]
impl TextAgent for RouteTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        for rule in &self.rules {
            if (rule.predicate)(state) {
                let _ = self
                    .middleware
                    .run_on_event(&AgentEvent::RouteSelected {
                        agent_name: rule.agent.name().to_string(),
                    })
                    .await;
                return rule.agent.run(state).await;
            }
        }
        let _ = self
            .middleware
            .run_on_event(&AgentEvent::RouteSelected {
                agent_name: self.default.name().to_string(),
            })
            .await;
        self.default.run(state).await
    }
}
