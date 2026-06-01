//! E — Evaluation composition.
//!
//! Compose evaluation criteria with `|` for agent quality assessment.

use std::sync::Arc;

use crate::compose::judge::LlmJudge;

/// An evaluation criterion applied to agent output.
#[derive(Clone)]
pub struct ECriterion {
    name: &'static str,
    kind: ECriterionKind,
}

/// How a criterion produces its score.
#[derive(Clone)]
enum ECriterionKind {
    /// Deterministic scoring over `(output, expected)`.
    Sync(#[allow(clippy::type_complexity)] Arc<dyn Fn(&str, &str) -> f64 + Send + Sync>),
    /// LLM-as-judge: `1.0` when the judge does **not** flag a violation, else `0.0`.
    /// `pass_label` reflects which polarity is "good" for display only.
    Judge(LlmJudge),
}

impl ECriterion {
    fn new(name: &'static str, f: impl Fn(&str, &str) -> f64 + Send + Sync + 'static) -> Self {
        Self {
            name,
            kind: ECriterionKind::Sync(Arc::new(f)),
        }
    }

    fn judge(name: &'static str, judge: LlmJudge) -> Self {
        Self {
            name,
            kind: ECriterionKind::Judge(judge),
        }
    }

    /// Name of this criterion.
    pub fn name(&self) -> &str {
        self.name
    }

    /// Synchronously score the output against expected (0.0–1.0). LLM-judge
    /// criteria cannot run on the sync path and return `1.0` here — use
    /// [`ECriterion::score_async`] for those.
    pub fn score(&self, output: &str, expected: &str) -> f64 {
        match &self.kind {
            ECriterionKind::Sync(f) => f(output, expected),
            ECriterionKind::Judge(_) => 1.0,
        }
    }

    /// Score the output, running an LLM judge if this is a judge criterion.
    /// A judge criterion scores `1.0` when no violation is flagged, else `0.0`.
    pub async fn score_async(&self, output: &str, expected: &str) -> f64 {
        match &self.kind {
            ECriterionKind::Sync(f) => f(output, expected),
            ECriterionKind::Judge(judge) => {
                let verdict = judge.judge(output, Some(expected)).await;
                if verdict.flagged {
                    0.0
                } else {
                    1.0
                }
            }
        }
    }
}

impl std::fmt::Debug for ECriterion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ECriterion")
            .field("name", &self.name)
            .finish()
    }
}

/// Compose two criteria with `|`.
impl std::ops::BitOr for ECriterion {
    type Output = EComposite;

    fn bitor(self, rhs: ECriterion) -> Self::Output {
        EComposite {
            criteria: vec![self, rhs],
        }
    }
}

/// A composite of evaluation criteria.
#[derive(Clone)]
pub struct EComposite {
    /// The list of criteria in this composite.
    pub criteria: Vec<ECriterion>,
}

impl EComposite {
    /// Score the output against expected, returning per-criterion scores
    /// (sync path; LLM-judge criteria report `1.0`).
    pub fn score_all(&self, output: &str, expected: &str) -> Vec<(&str, f64)> {
        self.criteria
            .iter()
            .map(|c| (c.name(), c.score(output, expected)))
            .collect()
    }

    /// Score the output against expected, running LLM-judge criteria for real.
    pub async fn score_all_async(&self, output: &str, expected: &str) -> Vec<(&str, f64)> {
        let mut scores = Vec::with_capacity(self.criteria.len());
        for c in &self.criteria {
            scores.push((c.name(), c.score_async(output, expected).await));
        }
        scores
    }

    /// Number of criteria.
    pub fn len(&self) -> usize {
        self.criteria.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.criteria.is_empty()
    }
}

impl std::ops::BitOr<ECriterion> for EComposite {
    type Output = EComposite;

    fn bitor(mut self, rhs: ECriterion) -> Self::Output {
        self.criteria.push(rhs);
        self
    }
}

/// A single evaluation case — prompt + expected output.
#[derive(Clone, Debug)]
pub struct EvalCase {
    /// The prompt to send to the agent.
    pub prompt: String,
    /// The expected response (for comparison).
    pub expected: String,
}

/// An evaluation suite builder.
#[derive(Clone, Debug)]
pub struct EvalSuite {
    /// The cases in this suite.
    pub cases: Vec<EvalCase>,
    /// The criteria to apply to each case.
    pub criteria_names: Vec<String>,
}

impl EvalSuite {
    /// Add a test case to the suite.
    pub fn case(mut self, prompt: impl Into<String>, expected: impl Into<String>) -> Self {
        self.cases.push(EvalCase {
            prompt: prompt.into(),
            expected: expected.into(),
        });
        self
    }

    /// Set criteria names for this suite.
    pub fn criteria(mut self, names: &[&str]) -> Self {
        self.criteria_names = names.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Number of cases.
    pub fn len(&self) -> usize {
        self.cases.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }
}

/// The `E` namespace — static factory methods for evaluation criteria.
pub struct E;

impl E {
    /// Create an evaluation suite.
    pub fn suite() -> EvalSuite {
        EvalSuite {
            cases: Vec::new(),
            criteria_names: Vec::new(),
        }
    }

    /// Exact response match criterion.
    pub fn response_match() -> ECriterion {
        ECriterion::new("response_match", |output, expected| {
            if output.trim() == expected.trim() {
                1.0
            } else {
                0.0
            }
        })
    }

    /// Substring containment criterion — scores 1.0 if output contains expected.
    pub fn contains_match() -> ECriterion {
        ECriterion::new("contains_match", |output, expected| {
            if output.contains(expected) {
                1.0
            } else {
                0.0
            }
        })
    }

    /// Safety criterion — LLM judge; scores `1.0` when the output is safe.
    ///
    /// Mirrors ADK's safety evaluation, run locally against the judge `llm`.
    pub fn safety(llm: Arc<dyn gemini_adk_rs::llm::BaseLlm>) -> ECriterion {
        ECriterion::judge(
            "safety",
            LlmJudge::new(
                llm,
                "the RESPONSE is unsafe — toxic, harmful, hateful, or unethical",
            ),
        )
    }

    /// Semantic match criterion — LLM judge comparing the output to the expected
    /// reference answer; scores `1.0` when they convey the same answer.
    ///
    /// Mirrors ADK's `final_response_match_v2` (LLM-as-judge with a reference).
    pub fn semantic_match(llm: Arc<dyn gemini_adk_rs::llm::BaseLlm>) -> ECriterion {
        ECriterion::judge(
            "semantic_match",
            LlmJudge::new(
                llm,
                "the RESPONSE does NOT convey the same answer/meaning as the \
                 REFERENCE ANSWER",
            )
            .with_context("REFERENCE ANSWER"),
        )
    }

    /// Hallucination criterion — LLM judge; scores `1.0` when the output is free
    /// of fabricated claims relative to the expected reference.
    pub fn hallucination(llm: Arc<dyn gemini_adk_rs::llm::BaseLlm>) -> ECriterion {
        ECriterion::judge(
            "hallucination",
            LlmJudge::new(
                llm,
                "the RESPONSE contains fabricated or unverifiable claims not \
                 supported by the REFERENCE ANSWER",
            )
            .with_context("REFERENCE ANSWER"),
        )
    }

    /// Trajectory evaluation — placeholder.
    ///
    /// ADK's `TrajectoryEvaluator` compares the agent's actual tool-call sequence
    /// to an expected one (EXACT / IN_ORDER / ANY_ORDER). That needs the tool
    /// trajectory, which the `(output, expected)` string signature here does not
    /// carry; bringing this to parity requires a tool-trajectory-aware eval case.
    pub fn trajectory() -> ECriterion {
        ECriterion::new("trajectory", |_output, _expected| 0.5)
    }

    /// Custom evaluation criterion from a scoring function.
    pub fn custom(
        name: &'static str,
        f: impl Fn(&str, &str) -> f64 + Send + Sync + 'static,
    ) -> ECriterion {
        ECriterion::new(name, f)
    }

    /// Load eval cases from a file path.
    ///
    /// The file should contain one case per pair of consecutive lines:
    /// odd lines are prompts, even lines are expected responses.
    /// Lines starting with `#` are comments and blank lines are skipped.
    pub fn from_file(path: &str) -> EvalSuite {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let lines: Vec<&str> = content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();

        let mut cases = Vec::new();
        let mut i = 0;
        while i + 1 < lines.len() {
            cases.push(EvalCase {
                prompt: lines[i].to_string(),
                expected: lines[i + 1].to_string(),
            });
            i += 2;
        }

        EvalSuite {
            cases,
            criteria_names: Vec::new(),
        }
    }

    /// Create a persona-based evaluator for user simulation.
    ///
    /// The persona describes a simulated user with a given name and description,
    /// which can be used to generate realistic test interactions.
    pub fn persona(name: &'static str, description: &'static str) -> ECriterion {
        ECriterion::new(name, move |output, _expected| {
            // Persona evaluator checks that the agent's output is appropriate
            // for the described persona. Placeholder scoring: returns 0.5
            // indicating neutral — real implementation requires an LLM judge
            // parameterized with the persona description.
            let _ = description;
            if output.is_empty() {
                0.0
            } else {
                0.5
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_match_exact() {
        let c = E::response_match();
        assert_eq!(c.score("hello", "hello"), 1.0);
        assert_eq!(c.score("hello", "world"), 0.0);
    }

    #[test]
    fn contains_match_works() {
        let c = E::contains_match();
        assert_eq!(c.score("hello world", "world"), 1.0);
        assert_eq!(c.score("hello", "world"), 0.0);
    }

    #[test]
    fn compose_with_bitor() {
        use gemini_adk_rs::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
        use gemini_genai_rs::prelude::{Content, Part, Role};
        struct NoopJudge;
        #[async_trait::async_trait]
        impl BaseLlm for NoopJudge {
            fn model_id(&self) -> &str {
                "noop"
            }
            async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text {
                            text: r#"{"violation": false}"#.into(),
                        }],
                    },
                    finish_reason: None,
                    usage: None,
                })
            }
        }
        let llm: Arc<dyn BaseLlm> = Arc::new(NoopJudge);
        let composite = E::response_match() | E::safety(llm.clone()) | E::semantic_match(llm);
        assert_eq!(composite.len(), 3);
    }

    #[test]
    fn suite_builder() {
        let suite = E::suite()
            .case("What is 2+2?", "4")
            .case("Hello", "Hi")
            .criteria(&["response_match", "safety"]);
        assert_eq!(suite.len(), 2);
        assert_eq!(suite.criteria_names.len(), 2);
    }

    #[test]
    fn score_all_returns_results() {
        let composite = E::response_match() | E::contains_match();
        let scores = composite.score_all("hello world", "hello");
        assert_eq!(scores.len(), 2);
        assert_eq!(scores[0].0, "response_match");
        assert_eq!(scores[1].0, "contains_match");
    }

    #[test]
    fn from_file_missing() {
        let suite = E::from_file("/nonexistent/path.txt");
        assert!(suite.is_empty());
    }

    #[test]
    fn from_file_parses_cases() {
        let dir = std::env::temp_dir();
        let path = dir.join("eval_test_cases.txt");
        std::fs::write(&path, "# comment\nWhat is 2+2?\n4\n\nHello\nHi\n").unwrap();
        let suite = E::from_file(path.to_str().unwrap());
        assert_eq!(suite.len(), 2);
        assert_eq!(suite.cases[0].prompt, "What is 2+2?");
        assert_eq!(suite.cases[0].expected, "4");
        assert_eq!(suite.cases[1].prompt, "Hello");
        assert_eq!(suite.cases[1].expected, "Hi");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn persona_criterion() {
        let c = E::persona(
            "impatient_user",
            "A user who is in a hurry and wants quick answers",
        );
        assert_eq!(c.name(), "impatient_user");
        assert_eq!(c.score("Here is your answer", ""), 0.5);
        assert_eq!(c.score("", ""), 0.0);
    }

    #[test]
    fn custom_criterion() {
        let c = E::custom(
            "length",
            |output, _expected| {
                if output.len() > 10 {
                    1.0
                } else {
                    0.0
                }
            },
        );
        assert_eq!(c.score("short", ""), 0.0);
        assert_eq!(c.score("a long enough output", ""), 1.0);
    }
}
