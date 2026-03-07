use async_trait::async_trait;

use super::TextAgent;
use crate::error::AgentError;
use crate::state::State;

/// Read-only observation agent. Calls a function with the state but
/// cannot mutate it. Returns empty string. No LLM call.
pub struct TapTextAgent {
    name: String,
    func: Box<dyn Fn(&State) + Send + Sync>,
}

impl TapTextAgent {
    /// Create a new tap agent for read-only observation.
    pub fn new(name: impl Into<String>, f: impl Fn(&State) + Send + Sync + 'static) -> Self {
        Self {
            name: name.into(),
            func: Box::new(f),
        }
    }
}

#[async_trait]
impl TextAgent for TapTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        (self.func)(state);
        Ok(String::new())
    }
}
