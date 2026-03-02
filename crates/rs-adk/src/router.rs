//! Agent registry and transfer routing.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::Agent;

/// Registry of named agents for transfer routing.
#[derive(Default)]
pub struct AgentRegistry {
    agents: HashMap<String, Arc<dyn Agent>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a named agent.
    pub fn register(&mut self, agent: Arc<dyn Agent>) {
        self.agents.insert(agent.name().to_string(), agent);
    }

    /// Look up an agent by name.
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Agent>> {
        self.agents.get(name).cloned()
    }

    /// List all registered agent names.
    pub fn names(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::InvocationContext;
    use crate::error::AgentError;
    use async_trait::async_trait;

    struct DummyAgent {
        name: String,
    }

    #[async_trait]
    impl Agent for DummyAgent {
        fn name(&self) -> &str {
            &self.name
        }
        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[test]
    fn register_and_resolve() {
        let mut registry = AgentRegistry::new();
        registry.register(Arc::new(DummyAgent {
            name: "billing".into(),
        }));
        registry.register(Arc::new(DummyAgent {
            name: "tech".into(),
        }));
        assert_eq!(registry.len(), 2);
        assert!(registry.resolve("billing").is_some());
        assert!(registry.resolve("nonexistent").is_none());
    }

    #[test]
    fn names_list() {
        let mut registry = AgentRegistry::new();
        registry.register(Arc::new(DummyAgent {
            name: "a".into(),
        }));
        registry.register(Arc::new(DummyAgent {
            name: "b".into(),
        }));
        let mut names = registry.names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn empty_registry() {
        let registry = AgentRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }
}
