//! LLM-as-judge evaluator — uses an LLM to grade agent responses.
//!
//! Mirrors ADK-Python's `llm_as_judge` evaluator.

use std::sync::Arc;

use async_trait::async_trait;

use super::eval_case::Invocation;
use super::eval_result::{EvalMetric, EvalResult, PerInvocationResult};
use super::evaluator::{EvalError, Evaluator};
use crate::llm::BaseLlm;

/// Configuration for the LLM-as-judge evaluator.
#[derive(Debug, Clone)]
pub struct LlmAsJudgeConfig {
    /// The rubric/criteria to evaluate against.
    pub rubric: String,
    /// The metric name for this evaluation.
    pub metric_name: String,
}

impl Default for LlmAsJudgeConfig {
    fn default() -> Self {
        Self {
            rubric: "Evaluate the quality and correctness of the agent's response.".into(),
            metric_name: "llm_judge_score".into(),
        }
    }
}

/// Evaluator that uses an LLM to judge agent responses.
///
/// Sends the actual and expected invocations to an LLM along with
/// a rubric, and parses the score from the LLM's response.
pub struct LlmAsJudge {
    llm: Arc<dyn BaseLlm>,
    config: LlmAsJudgeConfig,
}

impl LlmAsJudge {
    /// Create a new LLM-as-judge evaluator.
    pub fn new(llm: Arc<dyn BaseLlm>, config: LlmAsJudgeConfig) -> Self {
        Self { llm, config }
    }

    /// Build the evaluation prompt for a single invocation.
    fn build_prompt(&self, actual: &Invocation, expected: Option<&Invocation>) -> String {
        let mut prompt = format!(
            "You are an expert evaluator. Score the agent's response on a scale of 0.0 to 1.0.\n\n\
             Rubric: {}\n\n\
             Actual conversation:\n",
            self.config.rubric
        );

        for turn in &actual.turns {
            prompt.push_str(&format!("[{}]: {}\n", turn.role, turn.content));
        }

        if let Some(expected) = expected {
            prompt.push_str("\nExpected conversation:\n");
            for turn in &expected.turns {
                prompt.push_str(&format!("[{}]: {}\n", turn.role, turn.content));
            }
        }

        prompt.push_str(
            "\nRespond with ONLY a JSON object: {\"score\": <float>, \"explanation\": \"<text>\"}",
        );

        prompt
    }
}

#[async_trait]
impl Evaluator for LlmAsJudge {
    async fn evaluate(
        &self,
        actual: &[Invocation],
        expected: Option<&[Invocation]>,
    ) -> Result<EvalResult, EvalError> {
        let mut per_invocation = Vec::new();
        let mut total_score = 0.0;

        for (i, actual_inv) in actual.iter().enumerate() {
            let expected_inv = expected.and_then(|e| e.get(i));
            let prompt = self.build_prompt(actual_inv, expected_inv);

            let request = crate::llm::LlmRequest::from_text(&prompt);
            let response = self
                .llm
                .generate(request)
                .await
                .map_err(|e| EvalError::Llm(e.to_string()))?;

            // Try to parse score from response
            let (score, explanation) = parse_judge_response(&response.text());
            total_score += score;

            per_invocation.push(PerInvocationResult {
                invocation_id: if actual_inv.id.is_empty() {
                    format!("inv-{}", i)
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
                name: self.config.metric_name.clone(),
                score: overall_score,
                per_invocation,
            }],
        })
    }
}

/// Parse the LLM judge's response to extract score and explanation.
fn parse_judge_response(text: &str) -> (f64, String) {
    // Try to parse JSON response
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        let score = v["score"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
        let explanation = v["explanation"]
            .as_str()
            .unwrap_or("No explanation")
            .to_string();
        return (score, explanation);
    }

    // Try to find JSON in the response text
    if let Some(start) = text.find('{') {
        if let Some(end) = text[start..].rfind('}') {
            let json_str = &text[start..=start + end];
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                let score = v["score"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
                let explanation = v["explanation"]
                    .as_str()
                    .unwrap_or("No explanation")
                    .to_string();
                return (score, explanation);
            }
        }
    }

    (0.0, format!("Failed to parse judge response: {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_json_response() {
        let (score, explanation) =
            parse_judge_response(r#"{"score": 0.85, "explanation": "Good response"}"#);
        assert!((score - 0.85).abs() < f64::EPSILON);
        assert_eq!(explanation, "Good response");
    }

    #[test]
    fn parse_json_in_text() {
        let (score, _) = parse_judge_response(
            r#"Here is my evaluation: {"score": 0.7, "explanation": "Decent"}"#,
        );
        assert!((score - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_invalid_response() {
        let (score, explanation) = parse_judge_response("This is just text");
        assert!((score - 0.0).abs() < f64::EPSILON);
        assert!(explanation.contains("Failed to parse"));
    }

    #[test]
    fn score_clamped_to_valid_range() {
        let (score, _) = parse_judge_response(r#"{"score": 1.5, "explanation": "Over"}"#);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn default_config() {
        let config = LlmAsJudgeConfig::default();
        assert_eq!(config.metric_name, "llm_judge_score");
        assert!(!config.rubric.is_empty());
    }

    #[test]
    fn build_prompt_includes_rubric() {
        use crate::evaluation::eval_case::InvocationTurn;

        struct DummyLlm;
        #[async_trait]
        impl BaseLlm for DummyLlm {
            fn model_id(&self) -> &str {
                "dummy"
            }
            async fn generate(
                &self,
                _req: crate::llm::LlmRequest,
            ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
                unreachable!()
            }
        }

        let judge = LlmAsJudge::new(Arc::new(DummyLlm), LlmAsJudgeConfig::default());
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
        let prompt = judge.build_prompt(&inv, None);
        assert!(prompt.contains("Rubric:"));
        assert!(prompt.contains("[user]: Hello"));
    }
}
