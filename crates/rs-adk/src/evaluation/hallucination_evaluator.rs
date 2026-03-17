//! Hallucination evaluator — check groundedness of agent responses.
//!
//! Evaluates whether agent responses are grounded in the provided context
//! (tool results, user input, conversation history) or contain fabricated
//! information.

use std::sync::Arc;

use async_trait::async_trait;

use super::eval_case::Invocation;
use super::eval_result::{EvalMetric, EvalResult, PerInvocationResult};
use super::evaluator::{EvalError, Evaluator};
use crate::llm::BaseLlm;

/// Evaluates whether agent responses are grounded (not hallucinated).
///
/// Uses an LLM-as-judge to assess whether the model's claims are supported
/// by the conversation context, tool outputs, and provided information.
pub struct HallucinationEvaluator {
    /// Optional override for the judge model.
    judge_model: Option<String>,
    /// Whether to also evaluate intermediate responses (not just the final one).
    evaluate_intermediate: bool,
    /// Optional LLM for performing evaluations.
    llm: Option<Arc<dyn BaseLlm>>,
}

impl HallucinationEvaluator {
    /// Create a new hallucination evaluator.
    pub fn new() -> Self {
        Self {
            judge_model: None,
            evaluate_intermediate: false,
            llm: None,
        }
    }

    /// Set whether to evaluate intermediate responses in addition to the final response.
    pub fn with_intermediate(mut self, eval: bool) -> Self {
        self.evaluate_intermediate = eval;
        self
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

    /// Extract grounding context from an invocation.
    ///
    /// Collects user inputs and tool results as the "source of truth" that
    /// model responses should be grounded in.
    fn extract_context(inv: &Invocation) -> String {
        let mut context = String::new();

        for turn in &inv.turns {
            match turn.role.as_str() {
                "user" => {
                    context.push_str(&format!("USER INPUT: {}\n", turn.content));
                }
                "model" if !turn.tool_results.is_empty() => {
                    for result in &turn.tool_results {
                        context.push_str(&format!("TOOL RESULT: {}\n", result));
                    }
                }
                _ => {}
            }
        }

        context
    }

    /// Extract model responses to evaluate for groundedness.
    fn extract_responses(inv: &Invocation, include_intermediate: bool) -> Vec<String> {
        let model_turns: Vec<&str> = inv
            .turns
            .iter()
            .filter(|t| t.role == "model" && !t.content.is_empty())
            .map(|t| t.content.as_str())
            .collect();

        if include_intermediate {
            model_turns.into_iter().map(String::from).collect()
        } else {
            // Only the last model response
            model_turns.last().map(|s| vec![s.to_string()]).unwrap_or_default()
        }
    }

    /// Build the groundedness evaluation prompt.
    fn build_prompt(context: &str, response: &str) -> String {
        format!(
            "You are an expert evaluator assessing GROUNDEDNESS (absence of hallucination).\n\n\
             Your task: determine whether the agent's response is fully supported by the \
             provided context. A response is grounded if every factual claim it makes can \
             be traced back to information in the context.\n\n\
             GROUNDING CONTEXT (source of truth):\n\
             {context}\n\n\
             AGENT RESPONSE TO EVALUATE:\n\
             {response}\n\n\
             Scoring guide:\n\
             - 1.0: Every claim is directly supported by the context\n\
             - 0.75: Most claims are supported, minor unsupported details\n\
             - 0.5: Mix of supported and unsupported claims\n\
             - 0.25: Mostly unsupported claims with some grounded elements\n\
             - 0.0: Entirely fabricated or contradicts the context\n\n\
             Respond with ONLY a JSON object:\n\
             {{\"score\": <float>, \"hallucinated_claims\": [\"<claim1>\", ...], \"explanation\": \"<text>\"}}"
        )
    }

    /// Parse the judge response for a groundedness score.
    fn parse_response(text: &str) -> (f64, String) {
        // Try direct JSON parse
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            return extract_score_and_explanation(&v);
        }

        // Try finding embedded JSON
        if let Some(start) = text.find('{') {
            if let Some(end) = text[start..].rfind('}') {
                let json_str = &text[start..=start + end];
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                    return extract_score_and_explanation(&v);
                }
            }
        }

        (0.0, format!("Failed to parse hallucination judge response: {text}"))
    }
}

impl Default for HallucinationEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract score and explanation from a parsed JSON value.
fn extract_score_and_explanation(v: &serde_json::Value) -> (f64, String) {
    let score = v["score"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);

    let mut explanation = v["explanation"]
        .as_str()
        .unwrap_or("No explanation")
        .to_string();

    // Append hallucinated claims if present
    if let Some(claims) = v["hallucinated_claims"].as_array() {
        let claim_strs: Vec<&str> = claims.iter().filter_map(|c| c.as_str()).collect();
        if !claim_strs.is_empty() {
            explanation.push_str(&format!(
                " | Hallucinated claims: {}",
                claim_strs.join("; ")
            ));
        }
    }

    (score, explanation)
}

#[async_trait]
impl Evaluator for HallucinationEvaluator {
    async fn evaluate(
        &self,
        actual: &[Invocation],
        _expected: Option<&[Invocation]>,
    ) -> Result<EvalResult, EvalError> {
        let llm = self
            .llm
            .as_ref()
            .ok_or_else(|| EvalError::Llm("HallucinationEvaluator requires an LLM instance — call .with_llm() before evaluating".into()))?;

        let mut per_invocation = Vec::new();
        let mut total_score = 0.0;

        for (i, actual_inv) in actual.iter().enumerate() {
            let context = Self::extract_context(actual_inv);
            let responses = Self::extract_responses(actual_inv, self.evaluate_intermediate);

            if responses.is_empty() {
                // No model responses to evaluate — trivially grounded
                per_invocation.push(PerInvocationResult {
                    invocation_id: inv_id(actual_inv, i),
                    score: 1.0,
                    explanation: Some("No model responses to evaluate".into()),
                });
                total_score += 1.0;
                continue;
            }

            // Evaluate each response and average
            let mut resp_total = 0.0;
            let mut explanations = Vec::new();

            for response in &responses {
                let prompt = Self::build_prompt(&context, response);
                let request = crate::llm::LlmRequest::from_text(&prompt);
                let llm_response = llm
                    .generate(request)
                    .await
                    .map_err(|e| EvalError::Llm(e.to_string()))?;

                let (score, explanation) = Self::parse_response(&llm_response.text());
                resp_total += score;
                explanations.push(explanation);
            }

            let avg_score = resp_total / responses.len() as f64;
            total_score += avg_score;

            per_invocation.push(PerInvocationResult {
                invocation_id: inv_id(actual_inv, i),
                score: avg_score,
                explanation: Some(explanations.join(" | ")),
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
                name: "groundedness".into(),
                score: overall_score,
                per_invocation,
            }],
        })
    }
}

/// Helper to get a meaningful invocation ID.
fn inv_id(inv: &Invocation, index: usize) -> String {
    if inv.id.is_empty() {
        format!("inv-{index}")
    } else {
        inv.id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::eval_case::InvocationTurn;

    #[test]
    fn extract_context_includes_user_and_tools() {
        let inv = Invocation {
            id: "test".into(),
            turns: vec![
                InvocationTurn {
                    role: "user".into(),
                    content: "What is the weather?".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                InvocationTurn {
                    role: "model".into(),
                    content: String::new(),
                    tool_calls: vec![serde_json::json!({"name": "get_weather"})],
                    tool_results: vec![serde_json::json!({"temp": 22})],
                },
                InvocationTurn {
                    role: "model".into(),
                    content: "It's 22 degrees.".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            metadata: serde_json::Value::Null,
        };

        let context = HallucinationEvaluator::extract_context(&inv);
        assert!(context.contains("What is the weather?"));
        assert!(context.contains("22"));
    }

    #[test]
    fn extract_responses_final_only() {
        let inv = Invocation {
            id: "test".into(),
            turns: vec![
                InvocationTurn {
                    role: "model".into(),
                    content: "first".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                InvocationTurn {
                    role: "model".into(),
                    content: "second".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            metadata: serde_json::Value::Null,
        };

        let responses = HallucinationEvaluator::extract_responses(&inv, false);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], "second");
    }

    #[test]
    fn extract_responses_all() {
        let inv = Invocation {
            id: "test".into(),
            turns: vec![
                InvocationTurn {
                    role: "model".into(),
                    content: "first".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                InvocationTurn {
                    role: "model".into(),
                    content: "second".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            metadata: serde_json::Value::Null,
        };

        let responses = HallucinationEvaluator::extract_responses(&inv, true);
        assert_eq!(responses.len(), 2);
    }

    #[test]
    fn parse_valid_response() {
        let json = r#"{"score": 0.9, "hallucinated_claims": [], "explanation": "Well grounded"}"#;
        let (score, explanation) = HallucinationEvaluator::parse_response(json);
        assert!((score - 0.9).abs() < f64::EPSILON);
        assert!(explanation.contains("Well grounded"));
    }

    #[test]
    fn parse_response_with_claims() {
        let json = r#"{"score": 0.5, "hallucinated_claims": ["temp was 25 not 22"], "explanation": "Partial"}"#;
        let (score, explanation) = HallucinationEvaluator::parse_response(json);
        assert!((score - 0.5).abs() < f64::EPSILON);
        assert!(explanation.contains("temp was 25 not 22"));
    }

    #[test]
    fn parse_invalid() {
        let (score, explanation) = HallucinationEvaluator::parse_response("garbage");
        assert!((score - 0.0).abs() < f64::EPSILON);
        assert!(explanation.contains("Failed to parse"));
    }

    #[test]
    fn default_impl() {
        let eval = HallucinationEvaluator::default();
        assert!(!eval.evaluate_intermediate);
        assert!(eval.judge_model.is_none());
    }

    #[test]
    fn builder_methods() {
        let eval = HallucinationEvaluator::new()
            .with_intermediate(true)
            .with_judge_model("gemini-2.0-flash");
        assert!(eval.evaluate_intermediate);
        assert_eq!(eval.judge_model.as_deref(), Some("gemini-2.0-flash"));
    }

    #[test]
    fn build_prompt_structure() {
        let prompt = HallucinationEvaluator::build_prompt(
            "USER INPUT: What is 2+2?\nTOOL RESULT: {\"answer\": 4}",
            "The answer is 4.",
        );
        assert!(prompt.contains("GROUNDEDNESS"));
        assert!(prompt.contains("GROUNDING CONTEXT"));
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.contains("The answer is 4."));
    }
}
