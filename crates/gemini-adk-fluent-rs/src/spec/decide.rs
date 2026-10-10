//! `decide` entries: decision-model questions in a session spec.
//!
//! ```json
//! "decide": [{
//!   "name": "caller_signals",
//!   "window": 2,
//!   "facts": ["party_size", "slot"],
//!   "questions": {
//!     "confirmed": {
//!       "type": "boolean",
//!       "instructions": "Did the caller agree to the booking that was read back?",
//!       "active_in": ["confirm"],
//!       "promote": { "to": "book_table_confirmed", "at_least": 0.85 }
//!     },
//!     "picked": {
//!       "type": "choice",
//!       "instructions": "Which offered time did the caller pick?",
//!       "options_from": "availability.options",
//!       "promote": { "to": "slot" }
//!     }
//!   }
//! }]
//! ```
//!
//! Each entry lowers to a [`DecisionExtractor`]. Uncertain answers go to the
//! extraction model by default (`"fallback": "llm"`); see [`DecideFallback`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use gemini_adk_rs::decision::{
    BooleanCriteria, DecisionExtractor, DecisionModel, DecisionQuestion, Promote, Question,
};
use gemini_adk_rs::llm::BaseLlm;

use super::TriggerSpec;

fn default_window() -> usize {
    2
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One decision-model pipeline: typed questions about the latest turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecideSpec {
    /// The state key holding the decided values and the raw answers.
    pub name: String,
    /// Transcript window in turns. Default 2: the agent's last turn and the
    /// caller's reply.
    #[serde(default = "default_window")]
    pub window: usize,
    /// State keys sent with the conversation as `facts`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
    /// Questions by id. Each id is also the answer's field in the result.
    pub questions: BTreeMap<String, DecideQuestionSpec>,
    /// What answers a question whose answer was uncertain.
    #[serde(default, skip_serializing_if = "DecideFallback::is_default")]
    pub fallback: DecideFallback,
    /// When it runs.
    #[serde(default)]
    pub trigger: TriggerSpec,
    /// How long to wait for the decision model, in milliseconds (default
    /// 2000). A slow or failed call goes to the fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// One question and where its answer goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecideQuestionSpec {
    /// The question and its options.
    #[serde(flatten)]
    pub kind: DecideQuestionKind,
    /// For a choice: the state path (`key` or `key.field`) holding an array
    /// of options, read each turn, such as the slots a tool offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_from: Option<String>,
    /// Ask only while one of these stages is active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_in: Vec<String>,
    /// For a choice: an extra option, `none_of_these`, with this
    /// description. Choosing it decides nothing. A choice always picks some
    /// option, so "which offered time did the caller pick?" needs a way to
    /// say they have not picked yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none: Option<String>,
    /// Write the answer to state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promote: Option<DecidePromoteSpec>,
}

/// The question types a decision model answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DecideQuestionKind {
    /// Answered with the probability that it is true.
    Boolean {
        /// The question.
        instructions: String,
        /// What the true and false cases mean.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<BooleanCriteriaSpec>,
    },
    /// Answered with one option.
    Choice {
        /// The question.
        instructions: String,
        /// Option name to description (1 to 255). May be empty with
        /// `options_from`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        criteria: BTreeMap<String, String>,
    },
    /// Answered with a position on an ordered scale, from 0.
    Score {
        /// The question.
        instructions: String,
        /// Level descriptions, lowest first (2 to 10).
        criteria: Vec<String>,
    },
}

/// What the true and false cases of a boolean question mean.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BooleanCriteriaSpec {
    /// When it is true.
    #[serde(rename = "true")]
    pub when_true: String,
    /// When it is false.
    #[serde(rename = "false")]
    pub when_false: String,
}

/// Where an answer goes, and how sure it must be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecidePromoteSpec {
    /// The state key written.
    pub to: String,
    /// Boolean: the probability needed for `true` (default 0.85). Choice and
    /// score: the certainty needed (default 0.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least: Option<f64>,
    /// Boolean: also write `false` for a confident no. Off by default, so a
    /// signal latches true.
    #[serde(default, skip_serializing_if = "is_false")]
    pub write_false: bool,
    /// Keep a value already in state.
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_known: bool,
}

/// What answers an uncertain question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum DecideFallback {
    /// `"llm"` (the default): the session's extraction model, through
    /// structured output, for the uncertain questions only. `"none"`: leave
    /// them undecided.
    Mode(DecideFallbackMode),
    /// AI Gateway reruns the whole decision with `model` when `when`
    /// matches (a Gateway decision fallback; both stages are billed).
    Gateway {
        /// The fallback.
        gateway: GatewayFallbackSpec,
    },
}

impl Default for DecideFallback {
    fn default() -> Self {
        Self::Mode(DecideFallbackMode::Llm)
    }
}

impl DecideFallback {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// See [`DecideFallback::Mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecideFallbackMode {
    /// Ask the extraction model.
    Llm,
    /// No fallback.
    None,
}

/// A Gateway decision fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GatewayFallbackSpec {
    /// The model that reruns the decision.
    pub model: String,
    /// The Gateway condition, such as
    /// `{"probabilityBetween": [0.15, 0.85]}` or
    /// `{"question": "route", "confidenceBelow": 0.6}`.
    pub when: Value,
}

impl DecideQuestionSpec {
    fn question(&self) -> Question {
        match &self.kind {
            DecideQuestionKind::Boolean {
                instructions,
                criteria,
            } => Question::Boolean {
                instructions: instructions.clone(),
                criteria: criteria.as_ref().map(|c| BooleanCriteria {
                    when_true: c.when_true.clone(),
                    when_false: c.when_false.clone(),
                }),
            },
            DecideQuestionKind::Choice {
                instructions,
                criteria,
            } => Question::Choice {
                instructions: instructions.clone(),
                criteria: criteria.clone(),
            },
            DecideQuestionKind::Score {
                instructions,
                criteria,
            } => Question::Score {
                instructions: instructions.clone(),
                criteria: criteria.clone(),
            },
        }
    }
}

impl DecideSpec {
    /// The state keys this entry writes: its name and promotion targets.
    pub fn written_keys(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(
            self.questions
                .values()
                .filter_map(|q| q.promote.as_ref().map(|p| p.to.as_str())),
        )
    }

    /// Problems with this entry; `stages` are the conversation's stage ids.
    pub fn problems(&self, stages: &BTreeSet<String>) -> Vec<String> {
        let mut out = Vec::new();
        let at = |msg: String| format!("decide '{}': {msg}", self.name);
        if self.name.trim().is_empty() {
            out.push("a decide entry needs a name".into());
        }
        if self.window == 0 {
            out.push(at("window must be at least 1".into()));
        }
        if self.timeout_ms == Some(0) {
            out.push(at("timeout_ms must be at least 1".into()));
        }
        if self.questions.is_empty() {
            out.push(at("needs at least one question".into()));
        }
        for (id, q) in &self.questions {
            let mut question = q.question();
            // Options read from state are filled in at each turn.
            if let (Some(_), Question::Choice { criteria, .. }) = (&q.options_from, &mut question)
                && criteria.is_empty()
            {
                criteria.insert("_".into(), "_".into());
            }
            if let Err(e) = question.validate() {
                out.push(at(format!("question '{id}': {e}")));
            }
            if q.options_from.is_some() && !matches!(q.kind, DecideQuestionKind::Choice { .. }) {
                out.push(at(format!(
                    "question '{id}': options_from applies to choice questions only"
                )));
            }
            if q.none.is_some() && !matches!(q.kind, DecideQuestionKind::Choice { .. }) {
                out.push(at(format!(
                    "question '{id}': none applies to choice questions only"
                )));
            }
            for stage in &q.active_in {
                if !stages.contains(stage) {
                    out.push(at(format!(
                        "question '{id}': active_in names unknown stage '{stage}'"
                    )));
                }
            }
            if let Some(p) = &q.promote {
                if p.to.trim().is_empty() {
                    out.push(at(format!("question '{id}': promote.to is empty")));
                }
                if let Some(t) = p.at_least {
                    let boolean = matches!(q.kind, DecideQuestionKind::Boolean { .. });
                    let ok = if boolean {
                        t > 0.5 && t <= 1.0
                    } else {
                        t > 0.0 && t <= 1.0
                    };
                    if !ok {
                        out.push(at(format!(
                            "question '{id}': at_least {t} must be {}",
                            if boolean {
                                "above 0.5 and at most 1"
                            } else {
                                "above 0 and at most 1"
                            }
                        )));
                    }
                }
                if p.write_false && !matches!(q.kind, DecideQuestionKind::Boolean { .. }) {
                    out.push(at(format!(
                        "question '{id}': write_false applies to boolean questions only"
                    )));
                }
            }
        }
        if let DecideFallback::Gateway { gateway } = &self.fallback
            && (gateway.model.trim().is_empty() || !gateway.when.is_object())
        {
            out.push(at(
                "a gateway fallback needs a model and a `when` condition object".into(),
            ));
        }
        out
    }

    /// Whether lowering needs the extraction model.
    pub fn needs_llm(&self) -> bool {
        self.fallback == DecideFallback::Mode(DecideFallbackMode::Llm)
    }

    /// Lower to a [`DecisionExtractor`]. `llm` backs the `"llm"` fallback;
    /// without one, uncertain answers stay undecided.
    pub fn compile(
        &self,
        model: Arc<dyn DecisionModel>,
        llm: Option<Arc<dyn BaseLlm>>,
    ) -> DecisionExtractor {
        let mut ex = DecisionExtractor::new(self.name.clone(), model, self.window)
            .facts(self.facts.clone())
            .with_trigger(self.trigger.to_trigger());
        if let Some(ms) = self.timeout_ms {
            ex = ex.with_timeout(std::time::Duration::from_millis(ms));
        }
        for (id, q) in &self.questions {
            let mut dq =
                DecisionQuestion::new(id.clone(), q.question()).active_in(q.active_in.clone());
            if let Some(path) = &q.options_from {
                dq = dq.options_from(path.clone());
            }
            if let Some(none) = &q.none {
                dq = dq.or_none(none.clone());
            }
            if let Some(p) = &q.promote {
                let mut promote = Promote::to(p.to.clone());
                if let Some(t) = p.at_least {
                    promote = promote.at_least(t);
                }
                if p.write_false {
                    promote = promote.write_false();
                }
                if p.keep_known {
                    promote = promote.keep_known();
                }
                dq = dq.promote(promote);
            }
            ex = ex.question(dq);
        }
        match &self.fallback {
            DecideFallback::Mode(DecideFallbackMode::Llm) => {
                if let Some(llm) = llm {
                    ex = ex.with_llm_fallback(llm);
                }
            }
            DecideFallback::Mode(DecideFallbackMode::None) => {}
            DecideFallback::Gateway { gateway } => {
                ex = ex.with_provider_options(json!({
                    "gateway": { "models": [{ "model": gateway.model, "when": gateway.when }] }
                }));
            }
        }
        ex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_adk_rs::decision::{Answer, MockDecisionModel};
    use gemini_adk_rs::live::extractor::TurnExtractor;

    fn entry() -> Value {
        json!({
            "name": "caller_signals",
            "facts": ["party_size"],
            "questions": {
                "confirmed": {
                    "type": "boolean",
                    "instructions": "Did the caller agree?",
                    "criteria": { "true": "said yes", "false": "anything else" },
                    "active_in": ["confirm"],
                    "promote": { "to": "book_table_confirmed", "at_least": 0.9 }
                },
                "picked": {
                    "type": "choice",
                    "instructions": "Which offered time?",
                    "options_from": "availability.options",
                    "promote": { "to": "slot" }
                },
                "frustration": {
                    "type": "score",
                    "instructions": "How frustrated?",
                    "criteria": ["calm", "upset", "angry"]
                }
            }
        })
    }

    #[test]
    fn an_entry_round_trips_with_its_defaults() {
        let spec: DecideSpec = serde_json::from_value(entry()).unwrap();
        assert_eq!(spec.window, 2);
        assert_eq!(spec.fallback, DecideFallback::Mode(DecideFallbackMode::Llm));
        let back = serde_json::to_value(&spec).unwrap();
        assert!(back.get("fallback").is_none(), "the default is not written");
        assert_eq!(
            back["questions"]["confirmed"]["criteria"]["true"],
            "said yes"
        );
        assert_eq!(serde_json::from_value::<DecideSpec>(back).unwrap(), spec);
        let gateway: DecideSpec = serde_json::from_value(json!({
            "name": "x", "questions": { "a": { "type": "boolean", "instructions": "A?" } },
            "fallback": { "gateway": { "model": "google/gemini-3.8-flash", "when": { "probabilityBetween": [0.2, 0.8] } } }
        }))
        .unwrap();
        assert!(matches!(gateway.fallback, DecideFallback::Gateway { .. }));
        let none: DecideSpec = serde_json::from_value(json!({
            "name": "x", "questions": { "a": { "type": "boolean", "instructions": "A?" } }, "fallback": "none"
        }))
        .unwrap();
        assert!(!none.needs_llm());
    }

    #[test]
    fn problems_are_reported_by_question() {
        let mut v = entry();
        v["questions"]["confirmed"]["promote"]["at_least"] = json!(0.4);
        v["questions"]["confirmed"]["active_in"] = json!(["nowhere"]);
        v["questions"]["frustration"]["criteria"] = json!(["only one"]);
        v["questions"]["frustration"]["options_from"] = json!("x");
        let spec: DecideSpec = serde_json::from_value(v).unwrap();
        let stages: BTreeSet<String> = ["confirm".to_string()].into();
        let problems = spec.problems(&stages);
        let joined = problems.join("\n");
        assert!(
            joined.contains("at_least 0.4 must be above 0.5"),
            "{joined}"
        );
        assert!(joined.contains("unknown stage 'nowhere'"), "{joined}");
        assert!(
            joined.contains("score question takes 2 to 10 levels"),
            "{joined}"
        );
        assert!(
            joined.contains("options_from applies to choice"),
            "{joined}"
        );

        let ok: DecideSpec = serde_json::from_value(entry()).unwrap();
        assert!(
            ok.problems(&stages).is_empty(),
            "{:?}",
            ok.problems(&stages)
        );
    }

    #[test]
    fn written_keys_are_the_name_and_promotion_targets() {
        let spec: DecideSpec = serde_json::from_value(entry()).unwrap();
        let keys: BTreeSet<&str> = spec.written_keys().collect();
        assert_eq!(
            keys,
            ["book_table_confirmed", "caller_signals", "slot"].into()
        );
    }

    #[tokio::test]
    async fn an_entry_lowers_to_a_working_extractor() {
        let spec: DecideSpec = serde_json::from_value(entry()).unwrap();
        let model = Arc::new(MockDecisionModel::new(|_| {
            Ok([
                ("confirmed".to_string(), Answer::boolean(0.95)),
                ("picked".to_string(), Answer::choice("19:00", 0.9)),
                ("frustration".to_string(), Answer::score(0.1, 0.9)),
            ]
            .into())
        }));
        let ex = spec.compile(model.clone(), None);
        let rules: Vec<&str> = ex
            .promotion_rules()
            .iter()
            .map(|r| r.state_key.as_str())
            .collect();
        assert_eq!(rules, ["book_table_confirmed", "slot"]);
        let state = gemini_adk_rs::State::new();
        let _ = state.set("flow:active", json!(["confirm"]));
        let _ = state.set("availability", json!({ "options": ["19:00", "19:30"] }));
        let window = [gemini_adk_rs::live::transcript::TranscriptTurn {
            turn_number: 0,
            user: "Seven, yes book it.".into(),
            model: String::new(),
            tool_calls: Vec::new(),
            timestamp: std::time::Instant::now(),
        }];
        let value = ex.extract_with_state(&window, &state).await.unwrap();
        assert_eq!(value["confirmed"], json!(true), "0.95 clears 0.9");
        assert_eq!(value["picked"], json!("19:00"));
        assert_eq!(model.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_gateway_fallback_becomes_provider_options() {
        let spec: DecideSpec = serde_json::from_value(json!({
            "name": "x", "questions": { "a": { "type": "boolean", "instructions": "A?" } },
            "fallback": { "gateway": { "model": "google/gemini-3.8-flash", "when": { "probabilityBetween": [0.2, 0.8] } } }
        }))
        .unwrap();
        let model = Arc::new(MockDecisionModel::new(|_| Ok(BTreeMap::new())));
        let ex = spec.compile(model.clone(), None);
        ex.extract(&[]).await.unwrap();
        assert_eq!(
            model.requests()[0].provider_options,
            Some(json!({ "gateway": { "models": [
                { "model": "google/gemini-3.8-flash", "when": { "probabilityBetween": [0.2, 0.8] } }
            ] } }))
        );
    }
}
