//! Trajectory evaluator — evaluates the tool call trajectory of agent invocations.
//!
//! Compares the sequence of tool calls made by the agent against expected trajectories.

use async_trait::async_trait;

use super::eval_case::Invocation;
use super::eval_result::{EvalMetric, EvalResult, PerInvocationResult};
use super::evaluator::{EvalError, Evaluator};

/// Evaluates the tool-call trajectory of agent invocations.
///
/// Compares actual tool calls (names and order) against expected tool calls
/// to assess whether the agent followed the correct reasoning path.
#[derive(Debug, Clone)]
pub struct TrajectoryEvaluator {
    /// Whether to enforce strict ordering of tool calls.
    pub strict_order: bool,
    metric_name: String,
}

impl TrajectoryEvaluator {
    /// Create a new trajectory evaluator.
    pub fn new(strict_order: bool) -> Self {
        Self {
            strict_order,
            metric_name: "trajectory_match".into(),
        }
    }

    /// Set a custom metric name.
    pub fn with_metric_name(mut self, name: impl Into<String>) -> Self {
        self.metric_name = name.into();
        self
    }

    /// Extract tool call names from an invocation's turns.
    fn extract_tool_names(inv: &Invocation) -> Vec<String> {
        inv.turns
            .iter()
            .flat_map(|turn| {
                turn.tool_calls
                    .iter()
                    .filter_map(|tc| tc.get("name").and_then(|n| n.as_str()).map(String::from))
            })
            .collect()
    }

    /// Score trajectory match between actual and expected tool call sequences.
    fn score_trajectory(&self, actual: &[String], expected: &[String]) -> (f64, String) {
        if expected.is_empty() && actual.is_empty() {
            return (1.0, "Both empty — trivially matching".into());
        }

        if expected.is_empty() {
            return (1.0, "No expected tools — any trajectory acceptable".into());
        }

        if self.strict_order {
            // Longest common subsequence ratio
            let lcs_len = lcs_length(actual, expected);
            let max_len = actual.len().max(expected.len());
            let score = if max_len == 0 {
                1.0
            } else {
                lcs_len as f64 / max_len as f64
            };
            (
                score,
                format!(
                    "Strict order: LCS {lcs_len}/{max_len} (actual={}, expected={})",
                    actual.len(),
                    expected.len()
                ),
            )
        } else {
            // Set-based: how many expected tools were called
            let expected_set: std::collections::HashSet<&str> =
                expected.iter().map(|s| s.as_str()).collect();
            let actual_set: std::collections::HashSet<&str> =
                actual.iter().map(|s| s.as_str()).collect();

            let intersection = expected_set.intersection(&actual_set).count();
            let union = expected_set.union(&actual_set).count();
            let score = if union == 0 {
                1.0
            } else {
                intersection as f64 / union as f64
            };
            (
                score,
                format!("Set match: {intersection}/{union} tools overlap"),
            )
        }
    }
}

impl Default for TrajectoryEvaluator {
    fn default() -> Self {
        Self::new(true)
    }
}

#[async_trait]
impl Evaluator for TrajectoryEvaluator {
    async fn evaluate(
        &self,
        actual: &[Invocation],
        expected: Option<&[Invocation]>,
    ) -> Result<EvalResult, EvalError> {
        let expected = expected.ok_or_else(|| {
            EvalError::InvalidInput("TrajectoryEvaluator requires expected invocations".into())
        })?;

        let mut per_invocation = Vec::new();
        let mut total_score = 0.0;

        for (i, actual_inv) in actual.iter().enumerate() {
            let actual_tools = Self::extract_tool_names(actual_inv);
            let expected_tools = expected
                .get(i)
                .map(Self::extract_tool_names)
                .unwrap_or_default();

            let (score, explanation) = self.score_trajectory(&actual_tools, &expected_tools);
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

/// Compute length of longest common subsequence.
fn lcs_length(a: &[String], b: &[String]) -> usize {
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::eval_case::InvocationTurn;
    use serde_json::json;

    fn make_invocation_with_tools(tool_names: &[&str]) -> Invocation {
        Invocation {
            id: String::new(),
            turns: vec![InvocationTurn {
                role: "model".into(),
                content: String::new(),
                tool_calls: tool_names
                    .iter()
                    .map(|name| json!({"name": name, "args": {}}))
                    .collect(),
                tool_results: vec![],
            }],
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn strict_order_perfect_match() {
        let eval = TrajectoryEvaluator::new(true);
        let actual = vec![make_invocation_with_tools(&["search", "lookup"])];
        let expected = vec![make_invocation_with_tools(&["search", "lookup"])];
        let result = eval.evaluate(&actual, Some(&expected)).await.unwrap();
        assert!((result.overall_score - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn set_match_unordered() {
        let eval = TrajectoryEvaluator::new(false);
        let actual = vec![make_invocation_with_tools(&["lookup", "search"])];
        let expected = vec![make_invocation_with_tools(&["search", "lookup"])];
        let result = eval.evaluate(&actual, Some(&expected)).await.unwrap();
        assert!((result.overall_score - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn partial_match() {
        let eval = TrajectoryEvaluator::new(false);
        let actual = vec![make_invocation_with_tools(&["search"])];
        let expected = vec![make_invocation_with_tools(&["search", "lookup"])];
        let result = eval.evaluate(&actual, Some(&expected)).await.unwrap();
        assert!(result.overall_score > 0.0);
        assert!(result.overall_score < 1.0);
    }

    #[tokio::test]
    async fn requires_expected() {
        let eval = TrajectoryEvaluator::default();
        let actual = vec![make_invocation_with_tools(&["search"])];
        assert!(eval.evaluate(&actual, None).await.is_err());
    }

    #[test]
    fn lcs_identical() {
        let a: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let b: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(lcs_length(&a, &b), 3);
    }

    #[test]
    fn lcs_different() {
        let a: Vec<String> = vec!["a".into(), "b".into()];
        let b: Vec<String> = vec!["c".into(), "d".into()];
        assert_eq!(lcs_length(&a, &b), 0);
    }
}
