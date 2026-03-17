//! Response evaluator — evaluates final response quality.
//!
//! Compares the agent's final response text against expected output
//! using configurable matching strategies.

use async_trait::async_trait;

use super::eval_case::Invocation;
use super::eval_result::{EvalMetric, EvalResult, PerInvocationResult};
use super::evaluator::{EvalError, Evaluator};

/// Strategy for comparing actual vs. expected responses.
#[derive(Debug, Clone, Copy, Default)]
pub enum MatchStrategy {
    /// Exact string match.
    Exact,
    /// Case-insensitive containment.
    #[default]
    Contains,
    /// Fuzzy match using Levenshtein-like distance.
    Fuzzy {
        /// Minimum similarity threshold (0.0–1.0).
        threshold: f64,
    },
}

/// Evaluates the agent's final response against expected output.
#[derive(Debug, Clone)]
pub struct ResponseEvaluator {
    strategy: MatchStrategy,
    metric_name: String,
}

impl ResponseEvaluator {
    /// Create a new response evaluator with the given matching strategy.
    pub fn new(strategy: MatchStrategy) -> Self {
        Self {
            strategy,
            metric_name: "response_match".into(),
        }
    }

    /// Set a custom metric name.
    pub fn with_metric_name(mut self, name: impl Into<String>) -> Self {
        self.metric_name = name.into();
        self
    }

    /// Get the final model response from an invocation.
    fn last_model_response(inv: &Invocation) -> Option<&str> {
        inv.turns
            .iter()
            .rev()
            .find(|t| t.role == "model")
            .map(|t| t.content.as_str())
    }

    /// Score a single pair of actual/expected responses.
    fn score_pair(&self, actual: &str, expected: &str) -> (f64, String) {
        match self.strategy {
            MatchStrategy::Exact => {
                if actual == expected {
                    (1.0, "Exact match".into())
                } else {
                    (0.0, "No exact match".into())
                }
            }
            MatchStrategy::Contains => {
                let actual_lower = actual.to_lowercase();
                let expected_lower = expected.to_lowercase();
                if actual_lower.contains(&expected_lower)
                    || expected_lower.contains(&actual_lower)
                {
                    (1.0, "Contains match".into())
                } else {
                    (0.0, "No containment match".into())
                }
            }
            MatchStrategy::Fuzzy { threshold } => {
                let similarity = string_similarity(actual, expected);
                if similarity >= threshold {
                    (similarity, format!("Fuzzy match: {similarity:.2}"))
                } else {
                    (
                        similarity,
                        format!("Below threshold {threshold:.2}: {similarity:.2}"),
                    )
                }
            }
        }
    }
}

impl Default for ResponseEvaluator {
    fn default() -> Self {
        Self::new(MatchStrategy::default())
    }
}

#[async_trait]
impl Evaluator for ResponseEvaluator {
    async fn evaluate(
        &self,
        actual: &[Invocation],
        expected: Option<&[Invocation]>,
    ) -> Result<EvalResult, EvalError> {
        let expected = expected.ok_or_else(|| {
            EvalError::InvalidInput("ResponseEvaluator requires expected invocations".into())
        })?;

        let mut per_invocation = Vec::new();
        let mut total_score = 0.0;

        for (i, actual_inv) in actual.iter().enumerate() {
            let actual_resp = Self::last_model_response(actual_inv).unwrap_or("");
            let expected_resp = expected
                .get(i)
                .and_then(|e| Self::last_model_response(e))
                .unwrap_or("");

            let (score, explanation) = self.score_pair(actual_resp, expected_resp);
            total_score += score;

            per_invocation.push(PerInvocationResult {
                invocation_id: if actual_inv.id.is_empty() {
                    format!("inv-{i}")
                } else {
                    actual_inv.id.clone()
                },
                score,
                explanation: Some(explanation),
            });
        }

        let overall_score = if actual.is_empty() {
            0.0
        } else {
            total_score / actual.len() as f64
        };

        Ok(EvalResult {
            overall_score,
            metrics: vec![EvalMetric {
                name: self.metric_name.clone(),
                score: overall_score,
                per_invocation,
            }],
        })
    }
}

/// Simple character-based similarity (normalized Levenshtein-like).
fn string_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let max_len = a.len().max(b.len()) as f64;
    if max_len == 0.0 {
        return 1.0;
    }

    let distance = levenshtein_distance(a, b) as f64;
    1.0 - (distance / max_len)
}

/// Compute Levenshtein edit distance.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }

    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::eval_case::InvocationTurn;

    fn make_invocation(model_response: &str) -> Invocation {
        Invocation {
            id: String::new(),
            turns: vec![
                InvocationTurn {
                    role: "user".into(),
                    content: "What is 2+2?".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                InvocationTurn {
                    role: "model".into(),
                    content: model_response.into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn exact_match() {
        let evaluator = ResponseEvaluator::new(MatchStrategy::Exact);
        let actual = vec![make_invocation("4")];
        let expected = vec![make_invocation("4")];
        let result = evaluator.evaluate(&actual, Some(&expected)).await.unwrap();
        assert!((result.overall_score - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn exact_mismatch() {
        let evaluator = ResponseEvaluator::new(MatchStrategy::Exact);
        let actual = vec![make_invocation("four")];
        let expected = vec![make_invocation("4")];
        let result = evaluator.evaluate(&actual, Some(&expected)).await.unwrap();
        assert!((result.overall_score - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn contains_match() {
        let evaluator = ResponseEvaluator::new(MatchStrategy::Contains);
        let actual = vec![make_invocation("The answer is 4")];
        let expected = vec![make_invocation("4")];
        let result = evaluator.evaluate(&actual, Some(&expected)).await.unwrap();
        assert!((result.overall_score - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn fuzzy_match() {
        let evaluator = ResponseEvaluator::new(MatchStrategy::Fuzzy { threshold: 0.5 });
        let actual = vec![make_invocation("hello world")];
        let expected = vec![make_invocation("hello worl")];
        let result = evaluator.evaluate(&actual, Some(&expected)).await.unwrap();
        assert!(result.overall_score > 0.5);
    }

    #[tokio::test]
    async fn requires_expected() {
        let evaluator = ResponseEvaluator::default();
        let actual = vec![make_invocation("test")];
        let result = evaluator.evaluate(&actual, None).await;
        assert!(result.is_err());
    }

    #[test]
    fn levenshtein_identical() {
        assert_eq!(levenshtein_distance("abc", "abc"), 0);
    }

    #[test]
    fn levenshtein_one_edit() {
        assert_eq!(levenshtein_distance("abc", "ab"), 1);
    }

    #[test]
    fn similarity_identical() {
        assert!((string_similarity("hello", "hello") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn similarity_empty() {
        assert!((string_similarity("", "") - 1.0).abs() < f64::EPSILON);
    }
}
