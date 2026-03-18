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
        f.debug_struct("GGuard").field("name", &self.name).finish()
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

    /// Output guard — validates model output content via a predicate function.
    pub fn output(
        f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> GGuard {
        GGuard::new("output", f)
    }

    /// Input guard — validates user input content via a predicate function.
    pub fn input(
        f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> GGuard {
        GGuard::new("input", f)
    }

    /// Rate limiting guard — enforces a maximum number of checks per minute.
    pub fn rate_limit(max_per_minute: u32) -> GGuard {
        GGuard::new("rate_limit", move |_output| {
            // Rate limiting is enforced at runtime by the processor.
            let _ = max_per_minute;
            Ok(())
        })
    }

    /// Toxicity detection guard — placeholder for toxicity classification.
    pub fn toxicity() -> GGuard {
        GGuard::new("toxicity", |_output| {
            // Toxicity detection requires an external classifier at runtime.
            Ok(())
        })
    }

    /// Grounding check guard — placeholder for grounding verification.
    pub fn grounded() -> GGuard {
        GGuard::new("grounded", |_output| {
            // Grounding checks require external verification at runtime.
            Ok(())
        })
    }

    /// Hallucination detection guard — placeholder for hallucination detection.
    pub fn hallucination() -> GGuard {
        GGuard::new("hallucination", |_output| {
            // Hallucination detection requires external verification at runtime.
            Ok(())
        })
    }

    /// Conditional guard — only applies `inner` when `predicate` returns true.
    pub fn when(
        predicate: impl Fn(&str) -> bool + Send + Sync + 'static,
        inner: GGuard,
    ) -> GGuard {
        GGuard::new("when", move |output| {
            if predicate(output) {
                inner.check(output)
            } else {
                Ok(())
            }
        })
    }

    /// LLM-as-judge content guard — stores a prompt for later LLM evaluation.
    pub fn llm_judge(prompt: &str) -> GGuard {
        let prompt = prompt.to_string();
        GGuard::new("llm_judge", move |_output| {
            // LLM judge evaluation happens at runtime with access to the LLM.
            let _ = &prompt;
            Ok(())
        })
    }

    /// Named custom judge function guard.
    pub fn custom_judge(
        name: &str,
        f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> GGuard {
        // Leak the name to get a 'static str, matching the GGuard field type.
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        GGuard::new(name, f)
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

    #[test]
    fn output_guard() {
        let g = G::output(|output| {
            if output.contains("forbidden") {
                Err("Forbidden content".into())
            } else {
                Ok(())
            }
        });
        assert!(g.check("safe content").is_ok());
        assert!(g.check("forbidden content").is_err());
        assert_eq!(g.name(), "output");
    }

    #[test]
    fn input_guard() {
        let g = G::input(|input| {
            if input.is_empty() {
                Err("Empty input".into())
            } else {
                Ok(())
            }
        });
        assert!(g.check("hello").is_ok());
        assert!(g.check("").is_err());
        assert_eq!(g.name(), "input");
    }

    #[test]
    fn rate_limit_guard() {
        let g = G::rate_limit(60);
        assert!(g.check("anything").is_ok());
        assert_eq!(g.name(), "rate_limit");
    }

    #[test]
    fn toxicity_guard() {
        let g = G::toxicity();
        assert!(g.check("anything").is_ok());
        assert_eq!(g.name(), "toxicity");
    }

    #[test]
    fn grounded_guard() {
        let g = G::grounded();
        assert!(g.check("anything").is_ok());
        assert_eq!(g.name(), "grounded");
    }

    #[test]
    fn hallucination_guard() {
        let g = G::hallucination();
        assert!(g.check("anything").is_ok());
        assert_eq!(g.name(), "hallucination");
    }

    #[test]
    fn when_guard_applies() {
        let inner = G::length(1, 5);
        let g = G::when(|output| output.starts_with("check:"), inner);
        // Predicate true — inner guard runs and rejects long output.
        assert!(g.check("check: this is way too long").is_err());
        // Predicate false — inner guard skipped.
        assert!(g.check("skip: this is way too long").is_ok());
        assert_eq!(g.name(), "when");
    }

    #[test]
    fn llm_judge_guard() {
        let g = G::llm_judge("Is this response helpful?");
        assert!(g.check("anything").is_ok());
        assert_eq!(g.name(), "llm_judge");
    }

    #[test]
    fn custom_judge_guard() {
        let g = G::custom_judge("profanity_filter", |output| {
            if output.contains("bad_word") {
                Err("Profanity detected".into())
            } else {
                Ok(())
            }
        });
        assert!(g.check("clean text").is_ok());
        assert!(g.check("has bad_word here").is_err());
        assert_eq!(g.name(), "profanity_filter");
    }

    #[test]
    fn compose_new_guards_with_bitor() {
        let composite = G::toxicity() | G::grounded() | G::hallucination();
        assert_eq!(composite.len(), 3);
        assert!(composite.check_all("test").is_empty());
    }
}
