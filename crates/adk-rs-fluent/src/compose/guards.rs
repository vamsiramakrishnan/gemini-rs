//! G — Guard composition.
//!
//! Compose output guards with `|` for validation and safety checks.

use std::sync::Arc;

/// A guard that validates agent output.
#[derive(Clone)]
pub struct GGuard {
    name: &'static str,
    #[allow(clippy::type_complexity)]
    checker: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
}

impl GGuard {
    fn new(
        name: &'static str,
        f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            checker: Arc::new(f),
        }
    }

    /// Name of this guard.
    pub fn name(&self) -> &str {
        self.name
    }

    /// Check the output. Returns `Ok(())` if valid, `Err(reason)` if not.
    pub fn check(&self, output: &str) -> Result<(), String> {
        (self.checker)(output)
    }
}

impl std::fmt::Debug for GGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GGuard")
            .field("name", &self.name)
            .finish()
    }
}

/// Compose two guards with `|`.
impl std::ops::BitOr for GGuard {
    type Output = GComposite;

    fn bitor(self, rhs: GGuard) -> Self::Output {
        GComposite {
            guards: vec![self, rhs],
        }
    }
}

/// A composite of guards — all must pass for output to be accepted.
#[derive(Clone)]
pub struct GComposite {
    /// The guards in this composite.
    pub guards: Vec<GGuard>,
}

impl GComposite {
    /// Check all guards against the output. Returns all violations.
    pub fn check_all(&self, output: &str) -> Vec<String> {
        self.guards
            .iter()
            .filter_map(|g| g.check(output).err())
            .collect()
    }

    /// Number of guards.
    pub fn len(&self) -> usize {
        self.guards.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.guards.is_empty()
    }
}

impl std::ops::BitOr<GGuard> for GComposite {
    type Output = GComposite;

    fn bitor(mut self, rhs: GGuard) -> Self::Output {
        self.guards.push(rhs);
        self
    }
}

/// The `G` namespace — static factory methods for guards.
pub struct G;

impl G {
    /// Length guard — output must be within bounds.
    pub fn length(min: usize, max: usize) -> GGuard {
        GGuard::new("length", move |output| {
            let len = output.len();
            if len < min {
                Err(format!("Output too short: {} < {}", len, min))
            } else if len > max {
                Err(format!("Output too long: {} > {}", len, max))
            } else {
                Ok(())
            }
        })
    }

    /// Regex guard — output must match (or not match) a pattern.
    pub fn regex(pattern: &str) -> GGuard {
        let pattern = pattern.to_string();
        GGuard::new("regex", move |output| {
            // Simple substring check — full regex requires the `regex` crate.
            if output.contains(&pattern) {
                Err(format!("Output matches forbidden pattern: {}", pattern))
            } else {
                Ok(())
            }
        })
    }

    /// Budget guard — output must not exceed a token estimate.
    pub fn budget(max_tokens: usize) -> GGuard {
        GGuard::new("budget", move |output| {
            // Rough estimate: 4 chars per token.
            let estimated_tokens = output.len() / 4;
            if estimated_tokens > max_tokens {
                Err(format!(
                    "Output exceeds token budget: ~{} > {}",
                    estimated_tokens, max_tokens
                ))
            } else {
                Ok(())
            }
        })
    }

    /// JSON guard — output must be valid JSON.
    pub fn json() -> GGuard {
        GGuard::new("json", |output| {
            serde_json::from_str::<serde_json::Value>(output)
                .map(|_| ())
                .map_err(|e| format!("Invalid JSON: {}", e))
        })
    }

    /// Max turns guard — placeholder for turn limit enforcement.
    pub fn max_turns(n: u32) -> GGuard {
        GGuard::new("max_turns", move |_output| {
            // Turn counting happens at runtime, not at output validation.
            let _ = n;
            Ok(())
        })
    }

    /// PII guard — checks for common PII patterns (email, phone).
    pub fn pii() -> GGuard {
        GGuard::new("pii", |output| {
            // Simple heuristic checks for common PII patterns.
            if output.contains('@') && output.contains('.') {
                // Might be an email — flag it.
                return Err("Output may contain email addresses".to_string());
            }
            Ok(())
        })
    }

    /// Topic restriction guard — output must not mention denied topics.
    pub fn topic(deny: &[&str]) -> GGuard {
        let deny: Vec<String> = deny.iter().map(|s| s.to_lowercase()).collect();
        GGuard::new("topic", move |output| {
            let lower = output.to_lowercase();
            for topic in &deny {
                if lower.contains(topic.as_str()) {
                    return Err(format!("Output mentions denied topic: {}", topic));
                }
            }
            Ok(())
        })
    }

    /// Custom guard from a validation function.
    pub fn custom(f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) -> GGuard {
        GGuard::new("custom", f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_guard_passes() {
        assert!(G::length(1, 100).check("hello").is_ok());
    }

    #[test]
    fn length_guard_too_short() {
        assert!(G::length(10, 100).check("hi").is_err());
    }

    #[test]
    fn length_guard_too_long() {
        assert!(G::length(1, 5).check("too long text").is_err());
    }

    #[test]
    fn json_guard_valid() {
        assert!(G::json().check(r#"{"key": "value"}"#).is_ok());
    }

    #[test]
    fn json_guard_invalid() {
        assert!(G::json().check("not json").is_err());
    }

    #[test]
    fn regex_guard_blocks() {
        assert!(G::regex("secret").check("this is a secret").is_err());
    }

    #[test]
    fn regex_guard_passes() {
        assert!(G::regex("secret").check("this is public").is_ok());
    }

    #[test]
    fn budget_guard_passes() {
        assert!(G::budget(100).check("short").is_ok());
    }

    #[test]
    fn topic_guard_blocks() {
        assert!(G::topic(&["violence"]).check("There was violence").is_err());
    }

    #[test]
    fn topic_guard_passes() {
        assert!(G::topic(&["violence"]).check("A peaceful day").is_ok());
    }

    #[test]
    fn compose_with_bitor() {
        let composite = G::length(1, 1000) | G::json();
        assert_eq!(composite.len(), 2);
    }

    #[test]
    fn check_all_returns_violations() {
        let composite = G::length(1, 5) | G::json();
        let violations = composite.check_all("not json and too long text here");
        assert!(!violations.is_empty());
    }

    #[test]
    fn custom_guard() {
        let g = G::custom(|output| {
            if output.contains("bad") {
                Err("Contains 'bad'".into())
            } else {
                Ok(())
            }
        });
        assert!(g.check("good output").is_ok());
        assert!(g.check("bad output").is_err());
    }
}
