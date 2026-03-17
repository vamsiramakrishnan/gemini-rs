//! User simulator evaluator — assess multi-turn simulation fidelity.
//!
//! Evaluates how well a user simulator (used in automated multi-turn testing)
//! follows its assigned persona, stays on topic, and produces realistic
//! user messages that effectively exercise the agent under test.

use std::sync::Arc;

use async_trait::async_trait;

use super::eval_case::Invocation;
use super::eval_result::{EvalMetric, EvalResult, PerInvocationResult};
use super::evaluator::{EvalError, Evaluator};
use crate::llm::BaseLlm;

/// Evaluates the fidelity of a user simulator in multi-turn conversations.
///
/// Assesses whether simulated user messages are:
/// - Realistic and coherent
/// - Following the assigned persona/scenario
/// - Providing adequate coverage of the test scenario
/// - Properly using the stop signal when the conversation should end
pub struct UserSimulatorEvaluator {
    /// Optional override for the judge model.
    judge_model: Option<String>,
    /// The stop signal token that ends simulation (e.g., "[DONE]").
    stop_signal: Option<String>,
    /// Optional LLM for performing evaluations.
    llm: Option<Arc<dyn BaseLlm>>,
}

impl UserSimulatorEvaluator {
    /// Create a new user simulator evaluator.
    pub fn new() -> Self {
        Self {
            judge_model: None,
            stop_signal: None,
            llm: None,
        }
    }

    /// Set the stop signal that the simulator uses to end conversations.
    pub fn with_stop_signal(mut self, signal: impl Into<String>) -> Self {
        self.stop_signal = Some(signal.into());
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

    /// Build the evaluation prompt for a simulated conversation.
    fn build_prompt(&self, inv: &Invocation) -> String {
        let mut prompt = String::from(
            "You are an expert evaluator assessing USER SIMULATOR FIDELITY.\n\n\
             A user simulator was used to generate the user-side of a multi-turn \
             conversation with an AI agent. Your task is to evaluate the quality \
             of the simulated user messages.\n\n\
             Evaluate on these criteria:\n\
             1. REALISM: Do the simulated user messages sound like a real human?\n\
             2. COHERENCE: Does the simulated user maintain a consistent persona and goal?\n\
             3. COVERAGE: Does the simulation adequately exercise the agent's capabilities?\n\
             4. PROGRESSION: Does the conversation progress naturally toward resolution?\n",
        );

        if let Some(ref signal) = self.stop_signal {
            prompt.push_str(&format!(
                "5. TERMINATION: Was the stop signal \"{signal}\" used appropriately?\n"
            ));
        }

        prompt.push_str("\nCONVERSATION:\n");
        for turn in &inv.turns {
            prompt.push_str(&format!("[{}]: {}\n", turn.role, turn.content));
        }

        prompt.push_str(
            "\nRespond with ONLY a JSON object:\n\
             {\"realism\": <float 0-1>, \
             \"coherence\": <float 0-1>, \
             \"coverage\": <float 0-1>, \
             \"progression\": <float 0-1>, \
             \"overall_score\": <float 0-1>, \
             \"explanation\": \"<text>\"}\n",
        );

        prompt
    }

    /// Parse the judge response.
    fn parse_response(text: &str) -> (f64, String) {
        if let Some(result) = try_parse_response(text) {
            return result;
        }

        // Try to find JSON embedded in text
        if let Some(start) = text.find('{') {
            if let Some(end) = text[start..].rfind('}') {
                let json_str = &text[start..=start + end];
                if let Some(result) = try_parse_response(json_str) {
                    return result;
                }
            }
        }

        (0.0, format!("Failed to parse simulator judge response: {text}"))
    }

    /// Perform heuristic scoring without an LLM.
    ///
    /// Checks basic conversation structure: turn alternation, non-empty
    /// messages, reasonable lengths, and proper stop signal usage.
    fn heuristic_score(&self, inv: &Invocation) -> (f64, String) {
        let mut score = 1.0;
        let mut issues = Vec::new();

        let user_turns: Vec<&str> = inv
            .turns
            .iter()
            .filter(|t| t.role == "user")
            .map(|t| t.content.as_str())
            .collect();

        if user_turns.is_empty() {
            return (0.0, "No user turns in conversation".into());
        }

        // Check for empty user messages
        let empty_count = user_turns.iter().filter(|t| t.trim().is_empty()).count();
        if empty_count > 0 {
            score -= 0.2 * empty_count as f64;
            issues.push(format!("{empty_count} empty user messages"));
        }

        // Check for very short repetitive messages
        let mut prev = "";
        let mut repeat_count = 0;
        for msg in &user_turns {
            if *msg == prev && !msg.is_empty() {
                repeat_count += 1;
            }
            prev = msg;
        }
        if repeat_count > 0 {
            score -= 0.15 * repeat_count as f64;
            issues.push(format!("{repeat_count} consecutive repeated messages"));
        }

        // Check turn alternation (user/model/user/model)
        let mut last_role = "";
        let mut alternation_violations = 0;
        for turn in &inv.turns {
            if turn.role == last_role && turn.role == "user" {
                alternation_violations += 1;
            }
            last_role = &turn.role;
        }
        if alternation_violations > 0 {
            score -= 0.1 * alternation_violations as f64;
            issues.push(format!("{alternation_violations} turn alternation violations"));
        }

        // Check stop signal usage if configured
        if let Some(ref signal) = self.stop_signal {
            let has_stop = user_turns.iter().any(|t| t.contains(signal.as_str()));
            let last_user_has_stop = user_turns
                .last()
                .map(|t| t.contains(signal.as_str()))
                .unwrap_or(false);

            if has_stop && !last_user_has_stop {
                score -= 0.2;
                issues.push("Stop signal used in non-final user turn".into());
            }
        }

        score = score.clamp(0.0, 1.0);

        let explanation = if issues.is_empty() {
            "Heuristic check passed — no structural issues detected".into()
        } else {
            format!("Heuristic issues: {}", issues.join("; "))
        };

        (score, explanation)
    }
}

impl Default for UserSimulatorEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

/// Try to parse a JSON string into a score and explanation.
fn try_parse_response(text: &str) -> Option<(f64, String)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    let score = if let Some(overall) = v["overall_score"].as_f64() {
        overall.clamp(0.0, 1.0)
    } else {
        // Average sub-scores
        let sub_scores = ["realism", "coherence", "coverage", "progression"];
        let (sum, count) = sub_scores
            .iter()
            .filter_map(|k| v[k].as_f64())
            .fold((0.0, 0), |(s, c), v| (s + v.clamp(0.0, 1.0), c + 1));
        if count == 0 {
            return None;
        }
        sum / count as f64
    };

    let explanation = v["explanation"]
        .as_str()
        .unwrap_or("No explanation")
        .to_string();

    Some((score, explanation))
}

#[async_trait]
impl Evaluator for UserSimulatorEvaluator {
    async fn evaluate(
        &self,
        actual: &[Invocation],
        _expected: Option<&[Invocation]>,
    ) -> Result<EvalResult, EvalError> {
        let mut per_invocation = Vec::new();
        let mut total_score = 0.0;

        let use_llm = self.llm.is_some();

        for (i, actual_inv) in actual.iter().enumerate() {
            let (score, explanation) = if use_llm {
                let llm = self.llm.as_ref().unwrap();
                let prompt = self.build_prompt(actual_inv);
                let request = crate::llm::LlmRequest::from_text(&prompt);
                let response = llm
                    .generate(request)
                    .await
                    .map_err(|e| EvalError::Llm(e.to_string()))?;
                Self::parse_response(&response.text())
            } else {
                // Fall back to heuristic scoring
                self.heuristic_score(actual_inv)
            };

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
                name: "user_simulator_fidelity".into(),
                score: overall_score,
                per_invocation,
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::eval_case::InvocationTurn;

    fn make_conversation(turns: &[(&str, &str)]) -> Invocation {
        Invocation {
            id: String::new(),
            turns: turns
                .iter()
                .map(|(role, content)| InvocationTurn {
                    role: role.to_string(),
                    content: content.to_string(),
                    tool_calls: vec![],
                    tool_results: vec![],
                })
                .collect(),
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn heuristic_good_conversation() {
        let eval = UserSimulatorEvaluator::new();
        let inv = make_conversation(&[
            ("user", "What is the weather?"),
            ("model", "It's sunny."),
            ("user", "Thanks!"),
        ]);
        let result = eval.evaluate(&[inv], None).await.unwrap();
        assert!((result.overall_score - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn heuristic_detects_empty_messages() {
        let eval = UserSimulatorEvaluator::new();
        let inv = make_conversation(&[
            ("user", ""),
            ("model", "I didn't understand."),
            ("user", "Hello"),
        ]);
        let result = eval.evaluate(&[inv], None).await.unwrap();
        assert!(result.overall_score < 1.0);
    }

    #[tokio::test]
    async fn heuristic_detects_repetition() {
        let eval = UserSimulatorEvaluator::new();
        let inv = make_conversation(&[
            ("user", "Hello"),
            ("model", "Hi!"),
            ("user", "Hello"),
            ("model", "Hi again!"),
            ("user", "Hello"),
        ]);
        let result = eval.evaluate(&[inv], None).await.unwrap();
        assert!(result.overall_score < 1.0);
    }

    #[tokio::test]
    async fn heuristic_stop_signal_ok() {
        let eval = UserSimulatorEvaluator::new().with_stop_signal("[DONE]");
        let inv = make_conversation(&[
            ("user", "Check the weather"),
            ("model", "It's 22C."),
            ("user", "Thanks [DONE]"),
        ]);
        let result = eval.evaluate(&[inv], None).await.unwrap();
        assert!((result.overall_score - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn heuristic_stop_signal_misplaced() {
        let eval = UserSimulatorEvaluator::new().with_stop_signal("[DONE]");
        let inv = make_conversation(&[
            ("user", "Check the weather [DONE]"),
            ("model", "It's 22C."),
            ("user", "Wait actually..."),
        ]);
        let result = eval.evaluate(&[inv], None).await.unwrap();
        assert!(result.overall_score < 1.0);
    }

    #[tokio::test]
    async fn empty_invocations() {
        let eval = UserSimulatorEvaluator::new();
        let result = eval.evaluate(&[], None).await.unwrap();
        assert!((result.overall_score - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn no_user_turns() {
        let eval = UserSimulatorEvaluator::new();
        let inv = make_conversation(&[("model", "Hello!")]);
        let result = eval.evaluate(&[inv], None).await.unwrap();
        assert!((result.overall_score - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_valid_response() {
        let json = r#"{"realism": 0.9, "coherence": 0.8, "coverage": 0.7, "progression": 0.6, "overall_score": 0.75, "explanation": "Good"}"#;
        let (score, explanation) = UserSimulatorEvaluator::parse_response(json);
        assert!((score - 0.75).abs() < f64::EPSILON);
        assert_eq!(explanation, "Good");
    }

    #[test]
    fn parse_sub_scores_only() {
        let json = r#"{"realism": 0.8, "coherence": 0.6}"#;
        let (score, _) = UserSimulatorEvaluator::parse_response(json);
        assert!((score - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_invalid() {
        let (score, explanation) = UserSimulatorEvaluator::parse_response("not json");
        assert!((score - 0.0).abs() < f64::EPSILON);
        assert!(explanation.contains("Failed to parse"));
    }

    #[test]
    fn default_impl() {
        let eval = UserSimulatorEvaluator::default();
        assert!(eval.judge_model.is_none());
        assert!(eval.stop_signal.is_none());
    }

    #[test]
    fn builder_methods() {
        let eval = UserSimulatorEvaluator::new()
            .with_judge_model("gemini-2.0-flash")
            .with_stop_signal("[END]");
        assert_eq!(eval.judge_model.as_deref(), Some("gemini-2.0-flash"));
        assert_eq!(eval.stop_signal.as_deref(), Some("[END]"));
    }

    #[test]
    fn build_prompt_includes_conversation() {
        let eval = UserSimulatorEvaluator::new().with_stop_signal("[DONE]");
        let inv = make_conversation(&[("user", "Hello"), ("model", "Hi!")]);
        let prompt = eval.build_prompt(&inv);
        assert!(prompt.contains("USER SIMULATOR FIDELITY"));
        assert!(prompt.contains("[user]: Hello"));
        assert!(prompt.contains("[model]: Hi!"));
        assert!(prompt.contains("[DONE]"));
    }
}
