use std::sync::Arc;

use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
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
        }
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
                return rule.agent.run(state).await;
            }
        }
        self.default.run(state).await
    }
}
