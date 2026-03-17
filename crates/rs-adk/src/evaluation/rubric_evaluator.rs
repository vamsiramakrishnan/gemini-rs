//! Rubric-based evaluator — evaluate agent responses against rubric criteria.
//!
//! Uses an LLM-as-judge to score agent outputs against one or more free-text
//! rubric criteria. Supports both final-response quality and tool-use quality
//! evaluation modes.

use std::sync::Arc;

use async_trait::async_trait;

use super::eval_case::Invocation;
use super::eval_result::{EvalMetric, EvalResult, PerInvocationResult};
use super::evaluator::{EvalError, Evaluator};
use crate::llm::BaseLlm;

/// Evaluation mode for rubric evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RubricMode {
    /// Evaluate the final response quality.
    FinalResponse,
    /// Evaluate tool use quality (selection, arguments, sequencing).
    ToolUse,
}

/// Evaluator that scores agent outputs against free-text rubric criteria
/// using an LLM as judge.
pub struct RubricEvaluator {
    /// The rubric criteria to evaluate against.
    rubrics: Vec<String>,
    /// Optional override for the judge model.
    judge_model: Option<String>,
    /// The evaluation mode (response vs tool use).
    mode: RubricMode,
    /// Optional LLM for performing evaluations.
    llm: Option<Arc<dyn BaseLlm>>,
}

impl RubricEvaluator {
    /// Create a new rubric evaluator with the given rubric criteria.
    pub fn new(rubrics: Vec<String>) -> Self {
        Self {
            rubrics,
            judge_model: None,
            mode: RubricMode::FinalResponse,
            llm: None,
        }
    }

    /// Create a rubric evaluator for final response quality.
    ///
    /// Uses the `rubric_based_final_response_quality_v1` evaluation strategy.
    pub fn for_response(rubrics: Vec<String>) -> Self {
        Self {
            rubrics,
            judge_model: None,
            mode: RubricMode::FinalResponse,
            llm: None,
        }
    }

    /// Create a rubric evaluator for tool use quality.
    ///
    /// Uses the `rubric_based_tool_use_quality_v1` evaluation strategy.
    pub fn for_tool_use(rubrics: Vec<String>) -> Self {
        Self {
            rubrics,
            judge_model: None,
            mode: RubricMode::ToolUse,
            llm: None,
        }
    }

    /// Set an override judge model name.
    pub fn with_judge_model(mut self, model: impl Into<String>) -> Self {
        self.judge_model = Some(model.into());
        self
    }

    /// Provide an LLM instance for performing evaluations.
    pub fn with_llm(mut self, llm: Arc<dyn BaseLlm>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Build the evaluation prompt for a single invocation.
    fn build_prompt(&self, actual: &Invocation, expected: Option<&Invocation>) -> String {
        let mode_label = match self.mode {
            RubricMode::FinalResponse => "FINAL RESPONSE QUALITY",
            RubricMode::ToolUse => "TOOL USE QUALITY",
        };

        let mut prompt = format!(
            "You are an expert evaluator assessing {mode_label}.\n\n\
             Score the agent's performance on a scale of 0.0 to 1.0 for EACH rubric criterion.\n\n"
        );

        // Add rubrics
        prompt.push_str("RUBRIC CRITERIA:\n");
        for (i, rubric) in self.rubrics.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, rubric));
        }
        prompt.push('\n');

        // Add actual conversation
        prompt.push_str("ACTUAL AGENT CONVERSATION:\n");
        for turn in &actual.turns {
            prompt.push_str(&format!("[{}]: {}\n", turn.role, turn.content));
            if !turn.tool_calls.is_empty() {
                prompt.push_str(&format!(
                    "  Tool calls: {}\n",
                    serde_json::json!(turn.tool_calls)
                ));
            }
            if !turn.tool_results.is_empty() {
                prompt.push_str(&format!(
                    "  Tool results: {}\n",
                    serde_json::json!(turn.tool_results)
                ));
            }
        }

        // Add expected conversation if available
        if let Some(expected) = expected {
            prompt.push_str("\nEXPECTED CONVERSATION:\n");
            for turn in &expected.turns {
                prompt.push_str(&format!("[{}]: {}\n", turn.role, turn.content));
                if !turn.tool_calls.is_empty() {
                    prompt.push_str(&format!(
                        "  Tool calls: {}\n",
                        serde_json::json!(turn.tool_calls)
                    ));
                }
            }
        }

        prompt.push_str(
            "\nRespond with ONLY a JSON object:\n\
             {\"scores\": [<float per rubric criterion>], \
             \"overall_score\": <float average>, \
             \"explanation\": \"<text>\"}\n",
        );

        prompt
    }

    /// Parse the LLM judge response to extract rubric scores.
    fn parse_response(text: &str, num_rubrics: usize) -> (f64, String) {
        // Try full JSON parse first
        if let Some((score, explanation)) = try_parse_json(text) {
            return (score, explanation);
        }

        // Try to find JSON embedded in text
        if let Some(start) = text.find('{') {
            if let Some(end) = text[start..].rfind('}') {
                let json_str = &text[start..=start + end];
                if let Some((score, explanation)) = try_parse_json(json_str) {
                    return (score, explanation);
                }
            }
        }

        // Fallback: try to find individual scores
        let _ = num_rubrics; // Used in full implementation
        (
            0.0,
            format!("Failed to parse rubric judge response: {text}"),
        )
    }
}

/// Try to parse a JSON string into a score and explanation.
fn try_parse_json(text: &str) -> Option<(f64, String)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    let score = if let Some(overall) = v["overall_score"].as_f64() {
        overall.clamp(0.0, 1.0)
    } else if let Some(scores) = v["scores"].as_array() {
        let sum: f64 = scores
            .iter()
            .filter_map(|s| s.as_f64())
            .map(|s| s.clamp(0.0, 1.0))
            .sum();
        let count = scores.len().max(1) as f64;
        sum / count
    } else {
        return None;
    };

    let explanation = v["explanation"]
        .as_str()
        .unwrap_or("No explanation")
        .to_string();

    Some((score, explanation))
}

#[async_trait]
impl Evaluator for RubricEvaluator {
    async fn evaluate(
        &self,
        actual: &[Invocation],
        expected: Option<&[Invocation]>,
    ) -> Result<EvalResult, EvalError> {
        let llm = self.llm.as_ref().ok_or_else(|| {
            EvalError::Llm(
                "RubricEvaluator requires an LLM instance — call .with_llm() before evaluating"
                    .into(),
            )
        })?;

        let mut per_invocation = Vec::new();
        let mut total_score = 0.0;

        for (i, actual_inv) in actual.iter().enumerate() {
            let expected_inv = expected.and_then(|e| e.get(i));
            let prompt = self.build_prompt(actual_inv, expected_inv);

            let request = crate::llm::LlmRequest::from_text(&prompt);
            let response = llm
                .generate(request)
                .await
                .map_err(|e| EvalError::Llm(e.to_string()))?;

            let (score, explanation) = Self::parse_response(&response.text(), self.rubrics.len());
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

        let metric_name = match self.mode {
            RubricMode::FinalResponse => "rubric_based_final_response_quality_v1",
            RubricMode::ToolUse => "rubric_based_tool_use_quality_v1",
        };

        Ok(EvalResult {
            overall_score,
            metrics: vec![EvalMetric {
                name: metric_name.into(),
                score: overall_score,
                per_invocation,
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_response() {
        let json = r#"{"scores": [0.8, 0.9], "overall_score": 0.85, "explanation": "Good"}"#;
        let (score, explanation) = RubricEvaluator::parse_response(json, 2);
        assert!((score - 0.85).abs() < f64::EPSILON);
        assert_eq!(explanation, "Good");
    }

    #[test]
    fn parse_scores_only() {
        let json = r#"{"scores": [0.8, 0.6]}"#;
        let (score, _) = RubricEvaluator::parse_response(json, 2);
        assert!((score - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_embedded_json() {
        let text = r#"Here is my evaluation: {"overall_score": 0.9, "explanation": "Great"}"#;
        let (score, _) = RubricEvaluator::parse_response(text, 1);
        assert!((score - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_invalid() {
        let (score, explanation) = RubricEvaluator::parse_response("no json here", 1);
        assert!((score - 0.0).abs() < f64::EPSILON);
        assert!(explanation.contains("Failed to parse"));
    }

    #[test]
    fn for_response_mode() {
        let eval = RubricEvaluator::for_response(vec!["Accuracy".into()]);
        assert_eq!(eval.mode, RubricMode::FinalResponse);
    }

    #[test]
    fn for_tool_use_mode() {
        let eval = RubricEvaluator::for_tool_use(vec!["Tool selection".into()]);
        assert_eq!(eval.mode, RubricMode::ToolUse);
    }

    #[test]
    fn build_prompt_includes_rubrics() {
        use crate::evaluation::eval_case::InvocationTurn;

        let eval = RubricEvaluator::new(vec![
            "Is the response accurate?".into(),
            "Is it well-formatted?".into(),
        ]);
        let inv = Invocation {
            id: "test".into(),
            turns: vec![InvocationTurn {
                role: "user".into(),
                content: "Hello".into(),
                tool_calls: vec![],
                tool_results: vec![],
            }],
            metadata: serde_json::Value::Null,
        };
        let prompt = eval.build_prompt(&inv, None);
        assert!(prompt.contains("Is the response accurate?"));
        assert!(prompt.contains("Is it well-formatted?"));
        assert!(prompt.contains("FINAL RESPONSE QUALITY"));
    }

    #[test]
    fn with_judge_model() {
        let eval = RubricEvaluator::new(vec!["test".into()]).with_judge_model("gemini-2.0-flash");
        assert_eq!(eval.judge_model.as_deref(), Some("gemini-2.0-flash"));
    }

    #[test]
    fn score_clamped() {
        let json = r#"{"overall_score": 1.5, "explanation": "Over"}"#;
        let (score, _) = RubricEvaluator::parse_response(json, 1);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }
}
