//! LLM registry — pattern-based resolution of LLM providers.
//!
//! Allows registering factory functions keyed by model name patterns.
//! When resolving, the first matching pattern wins.

use std::sync::Arc;

use super::BaseLlm;

type LlmFactory = Box<dyn Fn(&str) -> Arc<dyn BaseLlm> + Send + Sync>;

/// Registry that maps model name patterns to LLM factory functions.
///
/// Patterns are matched by prefix: a pattern `"gemini"` matches model names
/// `"gemini-2.5-flash"`, `"gemini-2.0-pro"`, etc.
pub struct LlmRegistry {
    factories: Vec<(String, LlmFactory)>,
}

impl LlmRegistry {
    /// Create a new empty LLM registry.
    pub fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// Register a factory for model names matching the given pattern (prefix match).
    pub fn register(
        &mut self,
        pattern: impl Into<String>,
        factory: impl Fn(&str) -> Arc<dyn BaseLlm> + Send + Sync + 'static,
    ) {
        self.factories.push((pattern.into(), Box::new(factory)));
    }

    /// Resolve a model name to an LLM instance.
    /// Returns the first factory whose pattern is a prefix of `model_name`.
    pub fn resolve(&self, model_name: &str) -> Option<Arc<dyn BaseLlm>> {
        for (pattern, factory) in &self.factories {
            if model_name.starts_with(pattern.as_str()) {
                return Some(factory(model_name));
            }
        }
        None
    }

    /// Number of registered factories.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// Whether no factories are registered.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

impl Default for LlmRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmError, LlmRequest, LlmResponse};
    use async_trait::async_trait;

    struct MockLlm {
        model: String,
    }

    #[async_trait]
    impl BaseLlm for MockLlm {
        fn model_id(&self) -> &str {
            &self.model
        }
        async fn generate(&self, _request: LlmRequest) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Other("mock".into()))
        }
    }

    #[test]
    fn register_and_resolve() {
        let mut registry = LlmRegistry::new();
        registry.register("gemini", |name: &str| {
            Arc::new(MockLlm {
                model: name.to_string(),
            })
        });

        let llm = registry.resolve("gemini-2.5-flash").unwrap();
        assert_eq!(llm.model_id(), "gemini-2.5-flash");
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let registry = LlmRegistry::new();
        assert!(registry.resolve("gpt-4").is_none());
    }

    #[test]
    fn first_match_wins() {
        let mut registry = LlmRegistry::new();
        registry.register("gemini-2.5", |name: &str| {
            Arc::new(MockLlm {
                model: format!("v2.5:{name}"),
            })
        });
        registry.register("gemini", |name: &str| {
            Arc::new(MockLlm {
                model: format!("generic:{name}"),
            })
        });

        let llm = registry.resolve("gemini-2.5-flash").unwrap();
        assert_eq!(llm.model_id(), "v2.5:gemini-2.5-flash");

        let llm2 = registry.resolve("gemini-1.5-pro").unwrap();
        assert_eq!(llm2.model_id(), "generic:gemini-1.5-pro");
    }

    #[test]
    fn len_and_is_empty() {
        let mut registry = LlmRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);

        registry.register("test", |name: &str| {
            Arc::new(MockLlm {
                model: name.to_string(),
            })
        });
        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 1);
    }
}
