//! Test configuration parser — load `test_config.json` for evaluation thresholds.
//!
//! Defines per-criterion pass/fail thresholds and optional LLM-judge configuration.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::evaluator::EvalError;

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Top-level test configuration loaded from `test_config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    /// Per-criterion evaluation configuration, keyed by criterion name.
    pub criteria: HashMap<String, CriterionConfig>,
}

/// Configuration for a single evaluation criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CriterionConfig {
    /// Simple threshold: the metric score must be >= this value to pass.
    Threshold(f64),
    /// LLM-judge criterion with optional model and sampling configuration.
    LlmJudge {
        /// Minimum score threshold to pass.
        threshold: f64,
        /// Override the judge model (e.g., "gemini-2.0-flash").
        #[serde(default)]
        judge_model: Option<String>,
        /// Number of LLM samples to average over for more stable scores.
        #[serde(default)]
        num_samples: Option<u32>,
    },
}

impl CriterionConfig {
    /// Get the threshold value regardless of variant.
    pub fn threshold(&self) -> f64 {
        match self {
            Self::Threshold(t) => *t,
            Self::LlmJudge { threshold, .. } => *threshold,
        }
    }

    /// Check whether a score passes this criterion.
    pub fn passes(&self, score: f64) -> bool {
        score >= self.threshold()
    }
}

impl TestConfig {
    /// Check whether all criteria pass for a set of metric scores.
    ///
    /// Returns a map of criterion name -> (passed, score, threshold).
    pub fn check_all(
        &self,
        scores: &HashMap<String, f64>,
    ) -> HashMap<String, (bool, f64, f64)> {
        self.criteria
            .iter()
            .map(|(name, config)| {
                let score = scores.get(name).copied().unwrap_or(0.0);
                let threshold = config.threshold();
                let passed = config.passes(score);
                (name.clone(), (passed, score, threshold))
            })
            .collect()
    }

    /// Returns `true` if all criteria pass.
    pub fn all_pass(&self, scores: &HashMap<String, f64>) -> bool {
        self.check_all(scores).values().all(|(passed, _, _)| *passed)
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a `test_config.json` file from disk.
///
/// # Errors
///
/// Returns `EvalError::Io` if the file cannot be read, or
/// `EvalError::Parse` if the JSON is invalid.
pub fn parse_test_config(path: &Path) -> Result<TestConfig, EvalError> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        EvalError::Io(format!(
            "Failed to read test config {}: {e}",
            path.display()
        ))
    })?;
    parse_test_config_str(&contents)
}

/// Parse a `test_config.json` from a raw JSON string.
pub fn parse_test_config_str(json: &str) -> Result<TestConfig, EvalError> {
    serde_json::from_str(json)
        .map_err(|e| EvalError::Parse(format!("Invalid test config JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_thresholds() {
        let json = r#"{
            "criteria": {
                "response_quality": 0.8,
                "tool_accuracy": 0.9
            }
        }"#;
        let config = parse_test_config_str(json).unwrap();
        assert_eq!(config.criteria.len(), 2);

        let rq = &config.criteria["response_quality"];
        assert!((rq.threshold() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_llm_judge_config() {
        let json = r#"{
            "criteria": {
                "coherence": {
                    "threshold": 0.7,
                    "judge_model": "gemini-2.0-flash",
                    "num_samples": 3
                }
            }
        }"#;
        let config = parse_test_config_str(json).unwrap();
        match &config.criteria["coherence"] {
            CriterionConfig::LlmJudge {
                threshold,
                judge_model,
                num_samples,
            } => {
                assert!((threshold - 0.7).abs() < f64::EPSILON);
                assert_eq!(judge_model.as_deref(), Some("gemini-2.0-flash"));
                assert_eq!(*num_samples, Some(3));
            }
            _ => panic!("Expected LlmJudge variant"),
        }
    }

    #[test]
    fn check_all_passing() {
        let json = r#"{"criteria": {"a": 0.5, "b": 0.8}}"#;
        let config = parse_test_config_str(json).unwrap();
        let scores: HashMap<String, f64> =
            [("a".into(), 0.6), ("b".into(), 0.9)].into_iter().collect();
        assert!(config.all_pass(&scores));
    }

    #[test]
    fn check_all_failing() {
        let json = r#"{"criteria": {"a": 0.5, "b": 0.8}}"#;
        let config = parse_test_config_str(json).unwrap();
        let scores: HashMap<String, f64> =
            [("a".into(), 0.6), ("b".into(), 0.7)].into_iter().collect();
        assert!(!config.all_pass(&scores));
    }

    #[test]
    fn missing_score_defaults_to_zero() {
        let json = r#"{"criteria": {"a": 0.5}}"#;
        let config = parse_test_config_str(json).unwrap();
        let scores: HashMap<String, f64> = HashMap::new();
        assert!(!config.all_pass(&scores));
    }

    #[test]
    fn parse_invalid_json() {
        let result = parse_test_config_str("bad");
        assert!(result.is_err());
    }
}
