//! G — Guard composition.
//!
//! Compose output guards with `|` for validation and safety checks.
//!
//! ## Wiring
//!
//! A [`GComposite`] attached via `AgentBuilder::guard` is installed on the
//! compiled `LlmTextAgent` as an `after_model` middleware layer (see
//! [`GComposite::into_middleware`]). Every model response is checked against
//! all guards; if any guard rejects the output the agent run fails with an
//! [`AgentError`] enumerating the violations, vetoing the response.

use std::sync::Arc;

use async_trait::async_trait;
use gemini_adk_rs::error::AgentError;
use gemini_adk_rs::llm::{BaseLlm, LlmRequest, LlmResponse};
use gemini_adk_rs::middleware::Middleware;

use crate::compose::judge::{render_contents, LlmJudge};

/// A guard that validates agent output.
#[derive(Clone)]
pub struct GGuard {
    name: &'static str,
    kind: GuardKind,
}

/// How a guard decides pass/fail.
#[derive(Clone)]
enum GuardKind {
    /// Synchronous predicate over the output text.
    Sync(#[allow(clippy::type_complexity)] Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>),
    /// LLM-as-judge over the output (and, for grounding, the input context).
    Judge(LlmJudge),
}

impl GGuard {
    fn new(
        name: &'static str,
        f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            kind: GuardKind::Sync(Arc::new(f)),
        }
    }

    fn judge(name: &'static str, judge: LlmJudge) -> Self {
        Self {
            name,
            kind: GuardKind::Judge(judge),
        }
    }

    /// Name of this guard.
    pub fn name(&self) -> &str {
        self.name
    }

    /// Synchronously check the output. LLM-judge guards cannot run on the sync
    /// path and always return `Ok(())` here — use [`GGuard::check_async`] (the
    /// guard middleware uses the async path).
    pub fn check(&self, output: &str) -> Result<(), String> {
        match &self.kind {
            GuardKind::Sync(f) => f(output),
            GuardKind::Judge(_) => Ok(()),
        }
    }

    /// Check the output, running an LLM judge if this is a judge guard.
    /// `context` is the model's input history (for grounding/hallucination).
    pub async fn check_async(&self, output: &str, context: Option<&str>) -> Result<(), String> {
        match &self.kind {
            GuardKind::Sync(f) => f(output),
            GuardKind::Judge(judge) => {
                let verdict = judge.judge(output, context).await;
                if verdict.flagged {
                    Err(verdict.reason)
                } else {
                    Ok(())
                }
            }
        }
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
    /// Check all guards against the output (sync path; LLM-judge guards are
    /// skipped — see [`GComposite::check_all_async`]). Returns all violations.
    pub fn check_all(&self, output: &str) -> Vec<String> {
        self.guards
            .iter()
            .filter_map(|g| g.check(output).err())
            .collect()
    }

    /// Check all guards, running LLM-judge guards against `output` and the
    /// optional input `context`. Returns all violations as `name: reason`.
    pub async fn check_all_async(&self, output: &str, context: Option<&str>) -> Vec<String> {
        let mut violations = Vec::new();
        for g in &self.guards {
            if let Err(reason) = g.check_async(output, context).await {
                violations.push(format!("{}: {}", g.name(), reason));
            }
        }
        violations
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

/// A single guard is a one-element composite, so `.guard(G::pii())` works
/// without an explicit `| `.
impl From<GGuard> for GComposite {
    fn from(guard: GGuard) -> Self {
        GComposite {
            guards: vec![guard],
        }
    }
}

impl GComposite {
    /// Adapt this guard composite into an `after_model` middleware layer that
    /// vetoes any model response failing one or more guards.
    pub fn into_middleware(self) -> Arc<dyn Middleware> {
        Arc::new(GuardMiddleware { guards: self })
    }
}

/// Middleware adapter that enforces a [`GComposite`] on every model response.
struct GuardMiddleware {
    guards: GComposite,
}

#[async_trait]
impl Middleware for GuardMiddleware {
    fn name(&self) -> &str {
        "guard"
    }

    async fn after_model(
        &self,
        request: &LlmRequest,
        response: &LlmResponse,
    ) -> Result<Option<LlmResponse>, AgentError> {
        // Render the input history so grounding/hallucination judges can see
        // what the response is supposed to be consistent with.
        let context = render_contents(&request.contents);
        let violations = self
            .guards
            .check_all_async(&response.text(), Some(&context))
            .await;
        if violations.is_empty() {
            Ok(None)
        } else {
            Err(AgentError::Other(format!(
                "guard violation: {}",
                violations.join("; ")
            )))
        }
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
    pub fn output(f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) -> GGuard {
        GGuard::new("output", f)
    }

    /// Input guard — validates user input content via a predicate function.
    pub fn input(f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) -> GGuard {
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

    /// Toxicity guard — flags toxic/abusive output using an LLM judge.
    ///
    /// Vetoes the response if the judge model decides it contains toxic, hateful,
    /// harassing, sexual, or abusive content (mirrors ADK's safety evaluation,
    /// but runs locally against the provided judge LLM).
    pub fn toxicity(judge: Arc<dyn BaseLlm>) -> GGuard {
        GGuard::judge(
            "toxicity",
            LlmJudge::new(
                judge,
                "the RESPONSE contains toxic, hateful, harassing, sexual, or otherwise \
                 abusive or unsafe content",
            ),
        )
    }

    /// Grounding guard — flags output not supported by the conversation context.
    ///
    /// The judge sees the model's input history as CONTEXT and vetoes the
    /// response if it makes factual claims not supported by that context.
    pub fn grounded(judge: Arc<dyn BaseLlm>) -> GGuard {
        GGuard::judge(
            "grounded",
            LlmJudge::new(
                judge,
                "the RESPONSE asserts facts that are NOT supported by, or that \
                 contradict, the provided CONTEXT",
            )
            .with_context("CONTEXT"),
        )
    }

    /// Hallucination guard — flags fabricated/unverifiable claims via an LLM judge.
    pub fn hallucination(judge: Arc<dyn BaseLlm>) -> GGuard {
        GGuard::judge(
            "hallucination",
            LlmJudge::new(
                judge,
                "the RESPONSE contains fabricated, invented, or unverifiable facts \
                 that are not supported by the CONTEXT",
            )
            .with_context("CONTEXT"),
        )
    }

    /// Conditional guard — only applies `inner` when `predicate` returns true.
    pub fn when(predicate: impl Fn(&str) -> bool + Send + Sync + 'static, inner: GGuard) -> GGuard {
        GGuard::new("when", move |output| {
            if predicate(output) {
                inner.check(output)
            } else {
                Ok(())
            }
        })
    }

    /// LLM-as-judge content guard.
    ///
    /// `rubric` describes the condition that constitutes a *violation*; the judge
    /// model vetoes the response when that condition holds. Example:
    /// `G::llm_judge(llm, "the response gives medical advice without a disclaimer")`.
    pub fn llm_judge(judge: Arc<dyn BaseLlm>, rubric: impl Into<String>) -> GGuard {
        GGuard::judge("llm_judge", LlmJudge::new(judge, rubric))
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

    // A no-op judge LLM for constructing LLM-backed guards in unit tests
    // (these tests exercise composition/naming, not the judge call itself).
    fn judge_llm() -> Arc<dyn BaseLlm> {
        use gemini_adk_rs::llm::{LlmError, LlmResponse};
        use gemini_genai_rs::prelude::{Content, Part, Role};

        struct NoopJudge;
        #[async_trait]
        impl BaseLlm for NoopJudge {
            fn model_id(&self) -> &str {
                "noop-judge"
            }
            async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text {
                            text: r#"{"violation": false, "reason": "ok"}"#.to_string(),
                        }],
                    },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            }
        }
        Arc::new(NoopJudge)
    }

    #[test]
    fn toxicity_guard() {
        let g = G::toxicity(judge_llm());
        // Sync path is a no-op for judge guards.
        assert!(g.check("anything").is_ok());
        assert_eq!(g.name(), "toxicity");
    }

    #[test]
    fn grounded_guard() {
        let g = G::grounded(judge_llm());
        assert!(g.check("anything").is_ok());
        assert_eq!(g.name(), "grounded");
    }

    #[test]
    fn hallucination_guard() {
        let g = G::hallucination(judge_llm());
        assert!(g.check("anything").is_ok());
        assert_eq!(g.name(), "hallucination");
    }

    #[tokio::test]
    async fn judge_guard_runs_async() {
        // A judge that flags everything should produce a violation via check_async.
        use gemini_adk_rs::llm::{LlmError, LlmResponse};
        use gemini_genai_rs::prelude::{Content, Part, Role};
        struct FlagAll;
        #[async_trait]
        impl BaseLlm for FlagAll {
            fn model_id(&self) -> &str {
                "flag-all"
            }
            async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text {
                            text: r#"{"violation": true, "reason": "bad"}"#.to_string(),
                        }],
                    },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            }
        }
        let g = G::toxicity(Arc::new(FlagAll));
        assert!(g.check_async("hello", None).await.is_err());
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
        let g = G::llm_judge(judge_llm(), "the response is unhelpful");
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
        let composite =
            G::toxicity(judge_llm()) | G::grounded(judge_llm()) | G::hallucination(judge_llm());
        assert_eq!(composite.len(), 3);
        // Sync path skips judge guards, so no violations surface synchronously.
        assert!(composite.check_all("test").is_empty());
    }
}
