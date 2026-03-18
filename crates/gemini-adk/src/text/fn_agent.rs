use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Zero-cost state transform agent — executes a closure, no LLM call.
pub struct FnTextAgent {
    name: String,
    #[allow(clippy::type_complexity)]
    func: Box<dyn Fn(&State) -> Result<String, AgentError> + Send + Sync>,
}

impl FnTextAgent {
    /// Create a new function agent.
    pub fn new(
        name: impl Into<String>,
        f: impl Fn(&State) -> Result<String, AgentError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            func: Box::new(f),
        }
    }
}

#[async_trait]
impl TextAgent for FnTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        (self.func)(state)
    }
}
