//! Decision models: typed questions about shared state, answered with
//! calibrated probabilities instead of text.
//!
//! A decision model (a "System One" model, such as TypeSafe AI's Jev on
//! Vercel AI Gateway) takes a `state` (a string, an object or an array) and a
//! map of named questions. Each answer comes back typed:
//!
//! | Question | Answer |
//! |---|---|
//! | [`Question::Boolean`] | `probability` that the statement is true |
//! | [`Question::Choice`] | the chosen option and each option's probability |
//! | [`Question::Score`] | a probability-weighted position on an ordered scale |
//!
//! A decision model writes no text, so it cannot fill an open slot such as a
//! name or a date. It can pick among options the session already holds (the
//! time slots a tool offered) and decide categorical things (a confirmation,
//! an intent, which stage comes next), usually far faster than a language
//! model. [`DecisionExtractor`] turns those answers into session state with a
//! threshold per question; [`GatewayDecisionModel`] (feature `ai-gateway`)
//! calls Vercel AI Gateway.
//!
//! ```
//! use gemini_adk_rs::decision::{Answer, DecisionRequest, MockDecisionModel, Question, DecisionModel};
//! use serde_json::json;
//!
//! # tokio_test::block_on(async {
//! let model = MockDecisionModel::new(|_| {
//!     Ok([("confirmed".to_string(), Answer::boolean(0.97))].into())
//! });
//! let response = model
//!     .decide(DecisionRequest::new(
//!         json!("Caller: yes, book it."),
//!         [("confirmed", Question::boolean("Did the caller agree to book?"))],
//!     ))
//!     .await
//!     .unwrap();
//! assert_eq!(response.answers["confirmed"].probability(), Some(0.97));
//! # });
//! ```

mod extractor;
#[cfg(feature = "ai-gateway")]
mod gateway;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::llm::LlmError;

pub use extractor::{
    DEFAULT_BOOLEAN_THRESHOLD, DEFAULT_CERTAINTY_THRESHOLD, DEFAULT_DECISION_TIMEOUT,
    DecisionExtractor, DecisionQuestion, NONE_OPTION, Promote,
};
#[cfg(feature = "ai-gateway")]
pub use gateway::GatewayDecisionModel;

/// The most options a choice question may have.
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// The fewest and most levels a score question may have.
pub const SCORE_LEVELS: std::ops::RangeInclusive<usize> = 2..=10;

/// What the true and false cases of a boolean question mean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BooleanCriteria {
    /// What makes the statement true.
    #[serde(rename = "true")]
    pub when_true: String,
    /// What makes it false.
    #[serde(rename = "false")]
    pub when_false: String,
}

/// One typed question, as the decision API takes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Answered with the probability that the statement is true.
    Boolean {
        /// The question.
        instructions: String,
        /// Optional definitions of the true and false cases.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<BooleanCriteria>,
    },
    /// Answered with one option and each option's probability.
    Choice {
        /// The question.
        instructions: String,
        /// Option name to description, 1 to 255 options.
        criteria: BTreeMap<String, String>,
    },
    /// Answered with a position on an ordered scale.
    Score {
        /// The question.
        instructions: String,
        /// Level descriptions, lowest first, 2 to 10 levels.
        criteria: Vec<String>,
    },
}

impl Question {
    /// A boolean question.
    pub fn boolean(instructions: impl Into<String>) -> Self {
        Self::Boolean {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    /// A choice among named options, each with a description.
    pub fn choice<K: Into<String>, D: Into<String>>(
        instructions: impl Into<String>,
        options: impl IntoIterator<Item = (K, D)>,
    ) -> Self {
        Self::Choice {
            instructions: instructions.into(),
            criteria: options
                .into_iter()
                .map(|(k, d)| (k.into(), d.into()))
                .collect(),
        }
    }

    /// A score on an ordered scale, lowest level first.
    pub fn score<L: Into<String>>(
        instructions: impl Into<String>,
        levels: impl IntoIterator<Item = L>,
    ) -> Self {
        Self::Score {
            instructions: instructions.into(),
            criteria: levels.into_iter().map(Into::into).collect(),
        }
    }

    /// Define what the true and false cases of a boolean question mean. A
    /// no-op on other question types.
    #[must_use]
    pub fn when(mut self, when_true: impl Into<String>, when_false: impl Into<String>) -> Self {
        if let Self::Boolean { criteria, .. } = &mut self {
            *criteria = Some(BooleanCriteria {
                when_true: when_true.into(),
                when_false: when_false.into(),
            });
        }
        self
    }

    /// The question's instructions.
    pub fn instructions(&self) -> &str {
        match self {
            Self::Boolean { instructions, .. }
            | Self::Choice { instructions, .. }
            | Self::Score { instructions, .. } => instructions,
        }
    }

    /// Check the limits the decision API enforces, so a bad question fails
    /// when it is built rather than on every call.
    pub fn validate(&self) -> Result<(), String> {
        if self.instructions().trim().is_empty() {
            return Err("a question needs instructions".into());
        }
        match self {
            Self::Boolean { .. } => Ok(()),
            Self::Choice { criteria, .. } if criteria.is_empty() => {
                Err("a choice question needs at least one option".into())
            }
            Self::Choice { criteria, .. } if criteria.len() > MAX_CHOICE_OPTIONS => Err(format!(
                "a choice question takes at most {MAX_CHOICE_OPTIONS} options, not {}",
                criteria.len()
            )),
            Self::Choice { .. } => Ok(()),
            Self::Score { criteria, .. } if !SCORE_LEVELS.contains(&criteria.len()) => {
                Err(format!(
                    "a score question takes {} to {} levels, not {}",
                    SCORE_LEVELS.start(),
                    SCORE_LEVELS.end(),
                    criteria.len()
                ))
            }
            Self::Score { .. } => Ok(()),
        }
    }
}

/// One answer, typed like its question.
///
/// Probabilities and scores arrive rounded to two decimals, so a
/// distribution may not sum to exactly 1; it is not renormalized.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// The probability that the statement is true (not a confidence in a
    /// yes/no answer).
    Boolean {
        /// `P(true)`, 0 to 1.
        probability: f64,
    },
    /// The chosen option.
    Choice {
        /// The selected option name.
        choice: String,
        /// Each option's probability.
        probabilities: BTreeMap<String, f64>,
        /// How concentrated the distribution is, 0 (even) to 1 (all on one
        /// option), when the model reports it.
        confidence: Option<f64>,
    },
    /// A position on the question's scale.
    Score {
        /// The probability-weighted level, indexed from 0.
        score: f64,
        /// Each level's probability, keyed `"0"`, `"1"`, …
        probabilities: BTreeMap<String, f64>,
        /// As for [`Answer::Choice`].
        confidence: Option<f64>,
    },
    /// The model declined to answer.
    Refusal(Value),
    /// An answer type this client does not know, kept as received.
    Other(Value),
}

impl Answer {
    /// A boolean answer.
    pub fn boolean(probability: f64) -> Self {
        Self::Boolean { probability }
    }

    /// A choice answer with no distribution.
    pub fn choice(choice: impl Into<String>, confidence: f64) -> Self {
        Self::Choice {
            choice: choice.into(),
            probabilities: BTreeMap::new(),
            confidence: Some(confidence),
        }
    }

    /// A score answer with no distribution.
    pub fn score(score: f64, confidence: f64) -> Self {
        Self::Score {
            score,
            probabilities: BTreeMap::new(),
            confidence: Some(confidence),
        }
    }

    /// `P(true)` of a boolean answer.
    pub fn probability(&self) -> Option<f64> {
        match self {
            Self::Boolean { probability } => Some(*probability),
            _ => None,
        }
    }

    /// How sure a choice or score answer is: the model's reported confidence,
    /// or else the probability of the chosen option (the most likely level
    /// for a score).
    pub fn certainty(&self) -> Option<f64> {
        match self {
            Self::Choice {
                choice,
                probabilities,
                confidence,
            } => confidence.or_else(|| probabilities.get(choice).copied()),
            Self::Score {
                probabilities,
                confidence,
                ..
            } => confidence.or_else(|| probabilities.values().copied().reduce(f64::max)),
            _ => None,
        }
    }

    /// Read an answer from the decision API's JSON.
    pub fn from_value(value: Value) -> Self {
        let number = |v: &Value, key: &str| v.get(key).and_then(Value::as_f64);
        let distribution = |v: &Value| -> BTreeMap<String, f64> {
            v.get("probabilities")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, p)| p.as_f64().map(|p| (k.clone(), p)))
                        .collect()
                })
                .unwrap_or_default()
        };
        match value.get("type").and_then(Value::as_str) {
            Some("boolean") => match number(&value, "probability") {
                Some(probability) => Self::Boolean { probability },
                None => Self::Other(value),
            },
            Some("choice") => match value.get("choice").and_then(Value::as_str) {
                Some(choice) => Self::Choice {
                    choice: choice.to_string(),
                    probabilities: distribution(&value),
                    confidence: number(&value, "confidence"),
                },
                None => Self::Other(value),
            },
            Some("score") => match number(&value, "score") {
                Some(score) => Self::Score {
                    score,
                    probabilities: distribution(&value),
                    confidence: number(&value, "confidence"),
                },
                None => Self::Other(value),
            },
            Some("refusal") => Self::Refusal(value),
            _ => Self::Other(value),
        }
    }

    /// The answer as the decision API writes it.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Boolean { probability } => {
                json!({ "type": "boolean", "probability": probability })
            }
            Self::Choice {
                choice,
                probabilities,
                confidence,
            } => {
                let mut v =
                    json!({ "type": "choice", "choice": choice, "probabilities": probabilities });
                if let Some(c) = confidence {
                    v["confidence"] = json!(c);
                }
                v
            }
            Self::Score {
                score,
                probabilities,
                confidence,
            } => {
                let mut v =
                    json!({ "type": "score", "score": score, "probabilities": probabilities });
                if let Some(c) = confidence {
                    v["confidence"] = json!(c);
                }
                v
            }
            Self::Refusal(v) | Self::Other(v) => v.clone(),
        }
    }
}

/// When AI Gateway should rerun a decision with another model.
///
/// Serializes to the `when` object of a Gateway decision fallback.
#[derive(Debug, Clone, PartialEq)]
pub enum FallbackWhen {
    /// A choice or score answer's confidence is below `below` (all such
    /// questions when `question` is `None`).
    ConfidenceBelow {
        /// The question to check, or every choice and score question.
        question: Option<String>,
        /// The threshold, 0 to 1.
        below: f64,
    },
    /// A boolean answer's `P(true)` is inside `[low, high]`.
    ProbabilityBetween {
        /// The question to check, or every boolean question.
        question: Option<String>,
        /// Inclusive lower bound.
        low: f64,
        /// Inclusive upper bound.
        high: f64,
    },
    /// At least one condition matches.
    Any(Vec<FallbackWhen>),
    /// Every condition matches.
    All(Vec<FallbackWhen>),
    /// At least `count` conditions match.
    AtLeast {
        /// How many must match.
        count: usize,
        /// The conditions.
        conditions: Vec<FallbackWhen>,
    },
}

impl FallbackWhen {
    /// The Gateway's JSON form.
    pub fn to_value(&self) -> Value {
        let with_question = |mut v: Value, question: &Option<String>| {
            if let Some(q) = question {
                v["question"] = json!(q);
            }
            v
        };
        match self {
            Self::ConfidenceBelow { question, below } => {
                with_question(json!({ "confidenceBelow": below }), question)
            }
            Self::ProbabilityBetween {
                question,
                low,
                high,
            } => with_question(json!({ "probabilityBetween": [low, high] }), question),
            Self::Any(c) => json!({ "any": c.iter().map(Self::to_value).collect::<Vec<_>>() }),
            Self::All(c) => json!({ "all": c.iter().map(Self::to_value).collect::<Vec<_>>() }),
            Self::AtLeast { count, conditions } => json!({
                "atLeast": {
                    "count": count,
                    "conditions": conditions.iter().map(Self::to_value).collect::<Vec<_>>(),
                }
            }),
        }
    }
}

/// A decision request: one state and its questions.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRequest {
    /// The shared state every question is asked about.
    pub state: Value,
    /// Named questions; each name keys its answer.
    pub questions: BTreeMap<String, Question>,
    /// Provider options passed through unchanged (for AI Gateway,
    /// `{"gateway": {...}}`). Merged over the model's defaults.
    pub provider_options: Option<Value>,
}

impl DecisionRequest {
    /// A request with no provider options.
    pub fn new<K: Into<String>>(
        state: Value,
        questions: impl IntoIterator<Item = (K, Question)>,
    ) -> Self {
        Self {
            state,
            questions: questions.into_iter().map(|(k, q)| (k.into(), q)).collect(),
            provider_options: None,
        }
    }
}

/// Token usage of one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecisionUsage {
    /// Input tokens billed.
    pub input_tokens: u64,
    /// Output tokens reported.
    pub output_tokens: u64,
}

/// A decision model's answers and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionResponse {
    /// The model that produced the answers (a fallback model when one ran).
    pub model: String,
    /// One answer per question.
    pub answers: BTreeMap<String, Answer>,
    /// Token usage, when reported.
    pub usage: Option<DecisionUsage>,
    /// The cost in US dollars, as the provider reports it.
    pub cost: Option<String>,
    /// Questions that triggered a Gateway fallback, with the reason
    /// (`confidence_below`, `probability_between`, `refused`, …). Empty when
    /// the primary model's answers were returned.
    pub fallback_triggered_by: Vec<(String, String)>,
    /// Wall-clock time of the call, as the client measured it.
    pub latency: Duration,
    /// Provider metadata as received.
    pub provider_metadata: Value,
}

impl DecisionResponse {
    /// A response with only answers, for mocks and tests.
    pub fn with_answers(model: impl Into<String>, answers: BTreeMap<String, Answer>) -> Self {
        Self {
            model: model.into(),
            answers,
            usage: None,
            cost: None,
            fallback_triggered_by: Vec::new(),
            latency: Duration::ZERO,
            provider_metadata: Value::Null,
        }
    }
}

/// A model that answers typed questions about a state.
#[async_trait]
pub trait DecisionModel: Send + Sync {
    /// The model's identifier, such as `typesafe-ai/jev`.
    fn model_id(&self) -> &str;

    /// Answer `request`'s questions.
    async fn decide(&self, request: DecisionRequest) -> Result<DecisionResponse, LlmError>;
}

type MockAnswerFn =
    dyn Fn(&DecisionRequest) -> Result<BTreeMap<String, Answer>, LlmError> + Send + Sync;

/// A [`DecisionModel`] whose answers come from a closure, for offline tests.
/// It records every request it receives.
pub struct MockDecisionModel {
    answer: Arc<MockAnswerFn>,
    requests: parking_lot::Mutex<Vec<DecisionRequest>>,
    latency: Duration,
}

impl MockDecisionModel {
    /// A mock answering with `answer`.
    pub fn new(
        answer: impl Fn(&DecisionRequest) -> Result<BTreeMap<String, Answer>, LlmError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            answer: Arc::new(answer),
            requests: parking_lot::Mutex::new(Vec::new()),
            latency: Duration::ZERO,
        }
    }

    /// Wait `latency` before answering.
    #[must_use]
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Every request received so far.
    pub fn requests(&self) -> Vec<DecisionRequest> {
        self.requests.lock().clone()
    }
}

#[async_trait]
impl DecisionModel for MockDecisionModel {
    fn model_id(&self) -> &str {
        "mock-decision"
    }

    async fn decide(&self, request: DecisionRequest) -> Result<DecisionResponse, LlmError> {
        self.requests.lock().push(request.clone());
        if !self.latency.is_zero() {
            tokio::time::sleep(self.latency).await;
        }
        let answers = (self.answer)(&request)?;
        let mut response = DecisionResponse::with_answers("mock-decision", answers);
        response.latency = self.latency;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_serialize_as_the_decision_api_takes_them() {
        let q = Question::boolean("Did the caller agree?").when("said yes", "anything else");
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({
                "type": "boolean",
                "instructions": "Did the caller agree?",
                "criteria": { "true": "said yes", "false": "anything else" }
            })
        );
        let q = Question::choice(
            "Route it.",
            [("billing", "charges"), ("shipping", "delivery")],
        );
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({
                "type": "choice",
                "instructions": "Route it.",
                "criteria": { "billing": "charges", "shipping": "delivery" }
            })
        );
        let q = Question::score("How urgent?", ["low", "medium", "high"]);
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({ "type": "score", "instructions": "How urgent?", "criteria": ["low", "medium", "high"] })
        );
    }

    #[test]
    fn question_limits_are_checked() {
        assert!(Question::boolean("").validate().is_err());
        let none: [(&str, &str); 0] = [];
        assert!(Question::choice("Pick.", none).validate().is_err());
        let many = (0..256).map(|i| (format!("o{i}"), "x".to_string()));
        assert!(Question::choice("Pick.", many).validate().is_err());
        assert!(Question::score("Rate.", ["only one"]).validate().is_err());
        assert!(Question::score("Rate.", ["a", "b"]).validate().is_ok());
    }

    #[test]
    fn answers_are_read_from_the_decision_api() {
        let a = Answer::from_value(json!({ "type": "boolean", "probability": 0.98 }));
        assert_eq!(a.probability(), Some(0.98));

        let a = Answer::from_value(json!({
            "type": "choice", "choice": "billing",
            "probabilities": { "billing": 0.9, "shipping": 0.1 }
        }));
        assert_eq!(
            a.certainty(),
            Some(0.9),
            "no confidence: the chosen option's probability"
        );
        let a = Answer::from_value(json!({
            "type": "choice", "choice": "billing", "confidence": 0.4,
            "probabilities": { "billing": 0.9, "shipping": 0.1 }
        }));
        assert_eq!(a.certainty(), Some(0.4), "the reported confidence wins");

        let a = Answer::from_value(json!({
            "type": "score", "score": 2.97,
            "probabilities": { "0": 0, "1": 0, "2": 0.02, "3": 0.98 }
        }));
        assert!(matches!(a, Answer::Score { score, .. } if (score - 2.97).abs() < 1e-9));
        assert_eq!(a.certainty(), Some(0.98));

        assert!(matches!(
            Answer::from_value(json!({ "type": "refusal" })),
            Answer::Refusal(_)
        ));
        assert!(matches!(
            Answer::from_value(json!({ "type": "boolean" })),
            Answer::Other(_)
        ));
        let round = json!({ "type": "choice", "choice": "a", "probabilities": { "a": 1.0 }, "confidence": 1.0 });
        assert_eq!(Answer::from_value(round.clone()).to_value(), round);
    }

    #[test]
    fn fallback_conditions_take_the_gateway_shape() {
        let when = FallbackWhen::Any(vec![
            FallbackWhen::ConfidenceBelow {
                question: Some("route".into()),
                below: 0.6,
            },
            FallbackWhen::ProbabilityBetween {
                question: None,
                low: 0.3,
                high: 0.8,
            },
            FallbackWhen::AtLeast {
                count: 1,
                conditions: vec![FallbackWhen::All(vec![FallbackWhen::ConfidenceBelow {
                    question: None,
                    below: 0.5,
                }])],
            },
        ]);
        assert_eq!(
            when.to_value(),
            json!({ "any": [
                { "question": "route", "confidenceBelow": 0.6 },
                { "probabilityBetween": [0.3, 0.8] },
                { "atLeast": { "count": 1, "conditions": [ { "all": [ { "confidenceBelow": 0.5 } ] } ] } }
            ] })
        );
    }

    #[tokio::test]
    async fn the_mock_records_requests() {
        let model = MockDecisionModel::new(|req| {
            Ok(req
                .questions
                .keys()
                .map(|k| (k.clone(), Answer::boolean(0.5)))
                .collect())
        });
        let response = model
            .decide(DecisionRequest::new(
                json!("x"),
                [("a", Question::boolean("A?"))],
            ))
            .await
            .unwrap();
        assert_eq!(response.answers.len(), 1);
        assert_eq!(model.requests().len(), 1);
    }
}
