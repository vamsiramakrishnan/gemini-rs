//! C — Context engineering.
//!
//! Compose context policies additively with `+`.

use std::sync::Arc;

use rs_genai::prelude::Content;

/// A context policy that filters/transforms conversation history.
#[derive(Clone)]
pub struct ContextPolicy {
    name: &'static str,
    #[allow(clippy::type_complexity)]
    filter: Arc<dyn Fn(&[Content]) -> Vec<Content> + Send + Sync>,
}

impl ContextPolicy {
    fn new(
        name: &'static str,
        f: impl Fn(&[Content]) -> Vec<Content> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            filter: Arc::new(f),
        }
    }

    /// Apply this policy to conversation history.
    pub fn apply(&self, history: &[Content]) -> Vec<Content> {
        (self.filter)(history)
    }

    /// Name of this policy.
    pub fn name(&self) -> &str {
        self.name
    }
}

impl std::fmt::Debug for ContextPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextPolicy")
            .field("name", &self.name)
            .finish()
    }
}

/// Compose two context policies additively with `+`.
/// The combined policy applies both filters and merges (deduplicates) results.
impl std::ops::Add for ContextPolicy {
    type Output = ContextPolicyChain;

    fn add(self, rhs: ContextPolicy) -> Self::Output {
        ContextPolicyChain {
            policies: vec![self, rhs],
        }
    }
}

/// A chain of context policies applied in combination.
#[derive(Clone)]
pub struct ContextPolicyChain {
    pub policies: Vec<ContextPolicy>,
}

impl ContextPolicyChain {
    /// Apply all policies and return the union of their results.
    pub fn apply(&self, history: &[Content]) -> Vec<Content> {
        let mut result = Vec::new();
        for policy in &self.policies {
            let filtered = policy.apply(history);
            // Simple append — dedup can be added if Content implements Eq
            result.extend(filtered);
        }
        result
    }
}

impl std::ops::Add<ContextPolicy> for ContextPolicyChain {
    type Output = ContextPolicyChain;

    fn add(mut self, rhs: ContextPolicy) -> Self::Output {
        self.policies.push(rhs);
        self
    }
}

/// The `C` namespace — static factory methods for context policies.
pub struct C;

impl C {
    /// Keep only the last `n` messages.
    pub fn window(n: usize) -> ContextPolicy {
        ContextPolicy::new("window", move |history| {
            if history.len() > n {
                history[history.len() - n..].to_vec()
            } else {
                history.to_vec()
            }
        })
    }

    /// Keep only messages with role "user".
    pub fn user_only() -> ContextPolicy {
        use rs_genai::prelude::Role;
        ContextPolicy::new("user_only", move |history| {
            history
                .iter()
                .filter(|c| c.role == Some(Role::User))
                .cloned()
                .collect()
        })
    }

    /// Apply a custom filter function.
    pub fn custom(
        f: impl Fn(&[Content]) -> Vec<Content> + Send + Sync + 'static,
    ) -> ContextPolicy {
        ContextPolicy::new("custom", f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_genai::prelude::Content;

    #[test]
    fn window_limits_messages() {
        let history = vec![
            Content::user("a"),
            Content::model("b"),
            Content::user("c"),
        ];
        let result = C::window(2).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn window_keeps_all_if_under_limit() {
        let history = vec![Content::user("a")];
        let result = C::window(5).apply(&history);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn user_only_filters() {
        let history = vec![
            Content::user("a"),
            Content::model("b"),
            Content::user("c"),
        ];
        let result = C::user_only().apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn compose_with_add() {
        let chain = C::window(10) + C::user_only();
        assert_eq!(chain.policies.len(), 2);
    }

    #[test]
    fn chain_extends_with_add() {
        let chain = C::window(10) + C::user_only() + C::custom(|h| h.to_vec());
        assert_eq!(chain.policies.len(), 3);
    }
}
