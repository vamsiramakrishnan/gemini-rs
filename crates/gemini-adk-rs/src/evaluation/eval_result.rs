//! Evaluation result types — metric scores and per-invocation breakdowns.

use serde::{Deserialize, Serialize};

/// A single metric evaluation score for one invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerInvocationResult {
    /// The invocation ID.
    pub invocation_id: String,
    /// Score for this invocation (0.0–1.0 typically).
    pub score: f64,
    /// Optional explanation of the score.
    #[serde(default)]
    pub explanation: Option<String>,
}

/// A named metric with its aggregated and per-invocation results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalMetric {
    /// Name of this metric (e.g., "response_match", "tool_use_quality").
    pub name: String,
    /// Aggregated score across all invocations.
    pub score: f64,
    /// Per-invocation breakdown.
    pub per_invocation: Vec<PerInvocationResult>,
}

/// The result of evaluating an evaluation set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    /// Overall aggregated score.
    pub overall_score: f64,
    /// Per-metric results.
    pub metrics: Vec<EvalMetric>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_result_construction() {
        let result = EvalResult {
            overall_score: 0.85,
            metrics: vec![EvalMetric {
                name: "response_match".into(),
                score: 0.85,
                per_invocation: vec![PerInvocationResult {
                    invocation_id: "inv-1".into(),
                    score: 0.9,
                    explanation: Some("Good match".into()),
                }],
            }],
        };
        assert!((result.overall_score - 0.85).abs() < f64::EPSILON);
        assert_eq!(result.metrics.len(), 1);
    }

    #[test]
    fn eval_result_serde_roundtrip() {
        let result = EvalResult {
            overall_score: 0.75,
            metrics: vec![],
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: EvalResult = serde_json::from_str(&json).unwrap();
        assert!((deserialized.overall_score - 0.75).abs() < f64::EPSILON);
    }
}
