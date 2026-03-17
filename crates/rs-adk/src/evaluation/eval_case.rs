//! Evaluation case and set types — define test scenarios for agents.

use serde::{Deserialize, Serialize};

/// A single turn in a conversation for evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationTurn {
    /// The role of this turn (e.g., "user", "model").
    pub role: String,
    /// The text content of this turn.
    pub content: String,
    /// Tool calls made during this turn (if any).
    #[serde(default)]
    pub tool_calls: Vec<serde_json::Value>,
    /// Tool results returned during this turn (if any).
    #[serde(default)]
    pub tool_results: Vec<serde_json::Value>,
}

/// A single invocation (conversation) for evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invocation {
    /// Unique identifier for this invocation.
    #[serde(default)]
    pub id: String,
    /// The turns of conversation in this invocation.
    pub turns: Vec<InvocationTurn>,
    /// Optional metadata about this invocation.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// A single evaluation case — pairs actual invocations with optional expected results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    /// Name of the eval case.
    pub name: String,
    /// The actual agent invocations to evaluate.
    pub actual: Vec<Invocation>,
    /// The expected (golden) invocations for comparison.
    #[serde(default)]
    pub expected: Vec<Invocation>,
    /// Optional conversation scenario description.
    #[serde(default)]
    pub scenario: Option<String>,
}

/// A collection of evaluation cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSet {
    /// Name of this evaluation set.
    pub name: String,
    /// The evaluation cases in this set.
    pub cases: Vec<EvalCase>,
    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_case_serde_roundtrip() {
        let case = EvalCase {
            name: "test-case".into(),
            actual: vec![Invocation {
                id: "inv-1".into(),
                turns: vec![InvocationTurn {
                    role: "user".into(),
                    content: "What is the weather?".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                }],
                metadata: serde_json::Value::Null,
            }],
            expected: vec![],
            scenario: Some("Weather query".into()),
        };

        let json = serde_json::to_string(&case).unwrap();
        let deserialized: EvalCase = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "test-case");
        assert_eq!(deserialized.actual.len(), 1);
    }

    #[test]
    fn eval_set_construction() {
        let set = EvalSet {
            name: "suite-1".into(),
            cases: vec![],
            description: Some("Test suite".into()),
        };
        assert_eq!(set.name, "suite-1");
        assert!(set.cases.is_empty());
    }
}
