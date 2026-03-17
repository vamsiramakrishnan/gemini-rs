//! `.evalset.json` parser — load golden evaluation datasets.
//!
//! Parses the upstream ADK golden dataset format into typed Rust structures
//! that can be fed into evaluators.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::evaluator::EvalError;

// ---------------------------------------------------------------------------
// Wire types — match the upstream `.evalset.json` schema
// ---------------------------------------------------------------------------

/// Top-level structure of a `.evalset.json` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSetFile {
    /// Name of the evaluation set.
    pub name: String,
    /// The evaluation cases.
    pub eval_cases: Vec<EvalCaseFile>,
}

/// A single evaluation case within an eval set file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCaseFile {
    /// Unique identifier for this eval case.
    pub eval_id: String,
    /// The multi-turn conversation to evaluate.
    pub conversation: Vec<InvocationFile>,
}

/// A single invocation (turn pair) in the eval case conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationFile {
    /// Unique identifier for this invocation.
    pub invocation_id: String,
    /// The user's input content.
    pub user_content: String,
    /// Expected tool uses for this invocation.
    #[serde(default)]
    pub expected_tool_use: Vec<ExpectedToolUse>,
    /// Expected final response text (if any).
    #[serde(default)]
    pub expected_response: Option<String>,
    /// Intermediate data recorded during this invocation.
    #[serde(default)]
    pub intermediate_data: Option<IntermediateData>,
}

/// An expected tool call within an invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedToolUse {
    /// Name of the tool expected to be called.
    pub tool_name: String,
    /// Expected input arguments to the tool.
    pub tool_input: serde_json::Value,
}

/// Intermediate data captured during agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntermediateData {
    /// Tool uses that actually occurred.
    #[serde(default)]
    pub tool_uses: Vec<ToolUseRecord>,
    /// Intermediate text responses from the model.
    #[serde(default)]
    pub intermediate_responses: Vec<String>,
}

/// Record of a tool use that occurred during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseRecord {
    /// Name of the tool that was called.
    pub tool_name: String,
    /// Input arguments passed to the tool.
    pub tool_input: serde_json::Value,
    /// Output returned by the tool.
    pub tool_output: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Parsing functions
// ---------------------------------------------------------------------------

/// Parse an `.evalset.json` file from disk.
///
/// # Errors
///
/// Returns `EvalError::Io` if the file cannot be read, or
/// `EvalError::Parse` if the JSON is invalid.
pub fn parse_evalset(path: &Path) -> Result<EvalSetFile, EvalError> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        EvalError::Io(format!(
            "Failed to read evalset file {}: {e}",
            path.display()
        ))
    })?;
    parse_evalset_str(&contents)
}

/// Parse an `.evalset.json` from a raw JSON string.
///
/// # Errors
///
/// Returns `EvalError::Parse` if the JSON is invalid.
pub fn parse_evalset_str(json: &str) -> Result<EvalSetFile, EvalError> {
    serde_json::from_str(json).map_err(|e| EvalError::Parse(format!("Invalid evalset JSON: {e}")))
}

// ---------------------------------------------------------------------------
// Conversion helpers — turn file types into evaluator types
// ---------------------------------------------------------------------------

impl EvalSetFile {
    /// Convert this file representation into evaluator-compatible [`super::Invocation`] pairs.
    ///
    /// Returns `(actual_invocations, expected_invocations)` for each eval case.
    /// Actual invocations are built from `intermediate_data` when present,
    /// falling back to user content only. Expected invocations are built from
    /// `expected_tool_use` and `expected_response`.
    pub fn to_eval_pairs(&self) -> Vec<(Vec<super::Invocation>, Vec<super::Invocation>)> {
        self.eval_cases
            .iter()
            .map(|case| {
                let mut actual_invocations = Vec::new();
                let mut expected_invocations = Vec::new();

                for inv in &case.conversation {
                    // Build the actual invocation from intermediate data
                    let mut actual_turns = vec![super::InvocationTurn {
                        role: "user".into(),
                        content: inv.user_content.clone(),
                        tool_calls: vec![],
                        tool_results: vec![],
                    }];

                    if let Some(ref data) = inv.intermediate_data {
                        // Add tool call turns from intermediate data
                        for tu in &data.tool_uses {
                            actual_turns.push(super::InvocationTurn {
                                role: "model".into(),
                                content: String::new(),
                                tool_calls: vec![serde_json::json!({
                                    "name": tu.tool_name,
                                    "args": tu.tool_input,
                                })],
                                tool_results: vec![tu.tool_output.clone()],
                            });
                        }
                        // Add intermediate responses
                        for resp in &data.intermediate_responses {
                            actual_turns.push(super::InvocationTurn {
                                role: "model".into(),
                                content: resp.clone(),
                                tool_calls: vec![],
                                tool_results: vec![],
                            });
                        }
                    }

                    actual_invocations.push(super::Invocation {
                        id: inv.invocation_id.clone(),
                        turns: actual_turns,
                        metadata: serde_json::Value::Null,
                    });

                    // Build the expected invocation
                    let mut expected_turns = vec![super::InvocationTurn {
                        role: "user".into(),
                        content: inv.user_content.clone(),
                        tool_calls: vec![],
                        tool_results: vec![],
                    }];

                    // Add expected tool calls
                    if !inv.expected_tool_use.is_empty() {
                        expected_turns.push(super::InvocationTurn {
                            role: "model".into(),
                            content: String::new(),
                            tool_calls: inv
                                .expected_tool_use
                                .iter()
                                .map(|tu| {
                                    serde_json::json!({
                                        "name": tu.tool_name,
                                        "args": tu.tool_input,
                                    })
                                })
                                .collect(),
                            tool_results: vec![],
                        });
                    }

                    // Add expected response
                    if let Some(ref resp) = inv.expected_response {
                        expected_turns.push(super::InvocationTurn {
                            role: "model".into(),
                            content: resp.clone(),
                            tool_calls: vec![],
                            tool_results: vec![],
                        });
                    }

                    expected_invocations.push(super::Invocation {
                        id: inv.invocation_id.clone(),
                        turns: expected_turns,
                        metadata: serde_json::Value::Null,
                    });
                }

                (actual_invocations, expected_invocations)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_EVALSET: &str = r#"{
        "name": "weather-agent-eval",
        "eval_cases": [
            {
                "eval_id": "case-1",
                "conversation": [
                    {
                        "invocation_id": "inv-1",
                        "user_content": "What's the weather in London?",
                        "expected_tool_use": [
                            {
                                "tool_name": "get_weather",
                                "tool_input": {"city": "London"}
                            }
                        ],
                        "expected_response": "The weather in London is 15°C and cloudy.",
                        "intermediate_data": {
                            "tool_uses": [
                                {
                                    "tool_name": "get_weather",
                                    "tool_input": {"city": "London"},
                                    "tool_output": {"temp": 15, "condition": "cloudy"}
                                }
                            ],
                            "intermediate_responses": ["Let me check the weather for London."]
                        }
                    },
                    {
                        "invocation_id": "inv-2",
                        "user_content": "And in Paris?",
                        "expected_tool_use": [
                            {
                                "tool_name": "get_weather",
                                "tool_input": {"city": "Paris"}
                            }
                        ],
                        "expected_response": null
                    }
                ]
            }
        ]
    }"#;

    #[test]
    fn parse_valid_evalset() {
        let evalset = parse_evalset_str(SAMPLE_EVALSET).unwrap();
        assert_eq!(evalset.name, "weather-agent-eval");
        assert_eq!(evalset.eval_cases.len(), 1);

        let case = &evalset.eval_cases[0];
        assert_eq!(case.eval_id, "case-1");
        assert_eq!(case.conversation.len(), 2);

        let inv1 = &case.conversation[0];
        assert_eq!(inv1.invocation_id, "inv-1");
        assert_eq!(inv1.user_content, "What's the weather in London?");
        assert_eq!(inv1.expected_tool_use.len(), 1);
        assert_eq!(inv1.expected_tool_use[0].tool_name, "get_weather");
        assert!(inv1.expected_response.is_some());
        assert!(inv1.intermediate_data.is_some());

        let data = inv1.intermediate_data.as_ref().unwrap();
        assert_eq!(data.tool_uses.len(), 1);
        assert_eq!(data.intermediate_responses.len(), 1);
    }

    #[test]
    fn parse_invalid_json() {
        let result = parse_evalset_str("not json");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Invalid evalset JSON"));
    }

    #[test]
    fn to_eval_pairs_converts_correctly() {
        let evalset = parse_evalset_str(SAMPLE_EVALSET).unwrap();
        let pairs = evalset.to_eval_pairs();
        assert_eq!(pairs.len(), 1);

        let (actual, expected) = &pairs[0];
        assert_eq!(actual.len(), 2);
        assert_eq!(expected.len(), 2);

        // First invocation should have user turn + tool call turn + intermediate response
        assert_eq!(actual[0].id, "inv-1");
        assert_eq!(actual[0].turns.len(), 3); // user + tool call + intermediate response
        assert_eq!(actual[0].turns[0].role, "user");
        assert_eq!(actual[0].turns[1].role, "model");
        assert!(!actual[0].turns[1].tool_calls.is_empty());

        // Expected should have user turn + tool call turn + response turn
        assert_eq!(expected[0].turns.len(), 3); // user + expected tool + expected response
    }

    #[test]
    fn minimal_evalset() {
        let json = r#"{
            "name": "minimal",
            "eval_cases": [{
                "eval_id": "c1",
                "conversation": [{
                    "invocation_id": "i1",
                    "user_content": "hello"
                }]
            }]
        }"#;
        let evalset = parse_evalset_str(json).unwrap();
        assert_eq!(
            evalset.eval_cases[0].conversation[0]
                .expected_tool_use
                .len(),
            0
        );
        assert!(evalset.eval_cases[0].conversation[0]
            .expected_response
            .is_none());
        assert!(evalset.eval_cases[0].conversation[0]
            .intermediate_data
            .is_none());
    }
}
