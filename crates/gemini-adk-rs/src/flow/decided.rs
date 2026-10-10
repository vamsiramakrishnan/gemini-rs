//! The `decided` guard atom: a guard on what a decision model concluded about
//! the conversation, rather than on a state key something else wrote.
//!
//! A question is declared once (see [`crate::decision::Decisions`]); a guard
//! names it and what it expects:
//!
//! | JSON | Holds when |
//! |---|---|
//! | `{"decided": "confirmed"}` | a boolean was answered yes, a choice picked an option, a score was given |
//! | `{"decided": {"confirmed": false}}` | a boolean was answered no, or a choice picked "none of these" |
//! | `{"decided": {"next_step": "book"}}` | a choice picked `book` |
//! | `{"decided": {"frustration": {"at_least": 2}}}` | a score of 2 or more |
//! | `{"decided": {"frustration": {"at_most": 1}}}` | a score of 1 or less |
//!
//! The runtime asks the questions the flow can act on at each decision point
//! (the caller's turn ends, or the model calls a tool) and records each answer
//! under `decision:{id}` with the caller turn it was about. An answer counts
//! only for that turn: a yes given three turns ago does not satisfy a guard
//! now, so nothing has to be latched or cleared. An unsure answer satisfies
//! no expectation.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::state::State;

/// Prefix of the state keys answers are recorded under.
pub const DECISION_PREFIX: &str = "decision:";
/// The state key holding the caller turn answers are currently about.
pub const DECISION_TURN_KEY: &str = "decision:turn";

/// The state key an answer to `question` is recorded under.
pub fn decision_key(question: &str) -> String {
    format!("{DECISION_PREFIX}{question}")
}

/// What a `decided` guard expects of an answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Expect {
    /// A boolean answered yes, a choice that picked an option, or any score.
    Yes,
    /// A boolean answered no, or a choice that picked "none of these".
    No,
    /// A choice that picked this option.
    Is(String),
    /// A score of at least this.
    AtLeast(f64),
    /// A score of at most this.
    AtMost(f64),
}

/// A guard on a decision: the question's id and what is expected.
#[derive(Clone, Debug, PartialEq)]
pub struct Decided {
    /// The question's id in the decision bank.
    pub question: String,
    /// What the answer must be.
    pub expect: Expect,
}

/// How an answer came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A boolean answered yes.
    Yes,
    /// A boolean answered no.
    No,
    /// A choice picked an option.
    Chosen,
    /// A choice picked "none of these".
    None,
    /// A score was given.
    Scored,
    /// Not sure enough to decide, refused, or the model could not be asked.
    Unsure,
}

/// An answer as recorded in state under [`decision_key`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// How it came out.
    pub outcome: Outcome,
    /// The chosen option's key (choice), or the score.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub value: Value,
    /// `P(true)` for a boolean, the certainty for a choice or score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// The caller turn the answer is about (see [`DECISION_TURN_KEY`]).
    pub turn: String,
}

impl DecisionRecord {
    /// The record for `question` in `state`, if it is about the current
    /// caller turn.
    pub fn current(state: &State, question: &str) -> Option<Self> {
        let record: Self = state.get(&decision_key(question))?;
        let turn: String = state.get(DECISION_TURN_KEY)?;
        (record.turn == turn).then_some(record)
    }
}

impl Decided {
    /// Whether the current answer meets the expectation.
    pub fn holds(&self, state: &State) -> bool {
        let Some(record) = DecisionRecord::current(state, &self.question) else {
            return false;
        };
        match (&self.expect, record.outcome) {
            (Expect::Yes, Outcome::Yes | Outcome::Chosen | Outcome::Scored) => true,
            (Expect::No, Outcome::No | Outcome::None) => true,
            (Expect::Is(option), Outcome::Chosen) => record.value.as_str() == Some(option),
            (Expect::AtLeast(x), Outcome::Scored) => record.value.as_f64().is_some_and(|s| s >= *x),
            (Expect::AtMost(x), Outcome::Scored) => record.value.as_f64().is_some_and(|s| s <= *x),
            _ => false,
        }
    }

    /// The guard as a prose clause, for a refusal the model reads.
    pub fn describe(&self) -> String {
        let q = &self.question;
        match &self.expect {
            Expect::Yes => format!("the caller's latest words must settle '{q}'"),
            Expect::No => format!("the caller's latest words must rule out '{q}'"),
            Expect::Is(o) => format!("the caller's latest words must settle '{q}' as '{o}'"),
            Expect::AtLeast(x) => format!("'{q}' must be at least {x}"),
            Expect::AtMost(x) => format!("'{q}' must be at most {x}"),
        }
    }

    fn to_value(&self) -> Value {
        let expect = match &self.expect {
            Expect::Yes => return Value::String(self.question.clone()),
            Expect::No => json!(false),
            Expect::Is(o) => json!(o),
            Expect::AtLeast(x) => json!({ "at_least": x }),
            Expect::AtMost(x) => json!({ "at_most": x }),
        };
        json!({ self.question.clone(): expect })
    }

    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::String(question) => Ok(Self {
                question,
                expect: Expect::Yes,
            }),
            Value::Object(map) if map.len() == 1 => {
                let (question, expect) = map.into_iter().next().expect("one entry");
                let expect = match expect {
                    Value::Bool(true) => Expect::Yes,
                    Value::Bool(false) => Expect::No,
                    Value::String(o) => Expect::Is(o),
                    Value::Object(bound) => match (
                        bound.get("at_least").and_then(Value::as_f64),
                        bound.get("at_most").and_then(Value::as_f64),
                    ) {
                        (Some(x), None) if bound.len() == 1 => Expect::AtLeast(x),
                        (None, Some(x)) if bound.len() == 1 => Expect::AtMost(x),
                        _ => {
                            return Err(format!(
                                "decided '{question}': a score bound is {{\"at_least\": n}} or {{\"at_most\": n}}"
                            ));
                        }
                    },
                    other => {
                        return Err(format!(
                            "decided '{question}': expected true, false, an option or a score bound, not {other}"
                        ));
                    }
                };
                Ok(Self { question, expect })
            }
            other => Err(format!(
                "decided takes a question id, or one {{question: expectation}} entry, not {other}"
            )),
        }
    }
}

impl Serialize for Decided {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Decided {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::from_value(Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Decided {
    fn schema_name() -> String {
        "Decided".to_string()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        #[derive(schemars::JsonSchema)]
        #[serde(untagged)]
        #[allow(dead_code)]
        enum Bound {
            AtLeast { at_least: f64 },
            AtMost { at_most: f64 },
        }
        #[derive(schemars::JsonSchema)]
        #[serde(untagged)]
        #[allow(dead_code)]
        enum Expectation {
            /// `true` (yes) or `false` (no).
            Boolean(bool),
            /// A choice's option.
            Option(String),
            /// A score bound.
            Score(Bound),
        }
        /// A question id (yes, picked, or scored), or one
        /// `{question: expectation}` entry.
        #[derive(schemars::JsonSchema)]
        #[serde(untagged)]
        #[allow(dead_code)]
        enum DecidedSchema {
            Question(String),
            Expecting(std::collections::BTreeMap<String, Expectation>),
        }
        DecidedSchema::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(state: &State, question: &str, outcome: Outcome, value: Value, turn: &str) {
        let _ = state.set(
            decision_key(question),
            serde_json::to_value(DecisionRecord {
                outcome,
                value,
                confidence: None,
                turn: turn.into(),
            })
            .unwrap(),
        );
    }

    #[test]
    fn json_forms_round_trip() {
        for (json, expect) in [
            (json!("confirmed"), Expect::Yes),
            (json!({ "confirmed": true }), Expect::Yes),
            (json!({ "confirmed": false }), Expect::No),
            (json!({ "next": "book" }), Expect::Is("book".into())),
            (json!({ "anger": { "at_least": 2 } }), Expect::AtLeast(2.0)),
            (json!({ "anger": { "at_most": 1 } }), Expect::AtMost(1.0)),
        ] {
            let d: Decided = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(d.expect, expect, "{json}");
            let back: Decided = serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
            assert_eq!(back, d);
        }
        assert!(serde_json::from_value::<Decided>(json!({ "a": 1, "b": 2 })).is_err());
        assert!(serde_json::from_value::<Decided>(json!({ "a": { "over": 2 } })).is_err());
        assert!(serde_json::from_value::<Decided>(json!(3)).is_err());
    }

    #[test]
    fn an_answer_counts_only_for_its_turn() {
        let state = State::new();
        let yes = Decided {
            question: "confirmed".into(),
            expect: Expect::Yes,
        };
        assert!(!yes.holds(&state), "never asked");
        let _ = state.set(DECISION_TURN_KEY, "t1");
        record(&state, "confirmed", Outcome::Yes, Value::Null, "t1");
        assert!(yes.holds(&state));
        let _ = state.set(DECISION_TURN_KEY, "t2");
        assert!(
            !yes.holds(&state),
            "a yes from an earlier turn is not a yes now"
        );
    }

    #[test]
    fn expectations_match_outcomes() {
        let state = State::new();
        let _ = state.set(DECISION_TURN_KEY, "t");
        let d = |q: &str, expect| Decided {
            question: q.into(),
            expect,
        };
        record(&state, "ok", Outcome::No, Value::Null, "t");
        assert!(d("ok", Expect::No).holds(&state));
        assert!(!d("ok", Expect::Yes).holds(&state));

        record(&state, "pick", Outcome::Chosen, json!("book"), "t");
        assert!(d("pick", Expect::Yes).holds(&state));
        assert!(d("pick", Expect::Is("book".into())).holds(&state));
        assert!(!d("pick", Expect::Is("stay".into())).holds(&state));

        record(&state, "none", Outcome::None, Value::Null, "t");
        assert!(d("none", Expect::No).holds(&state));
        assert!(!d("none", Expect::Yes).holds(&state));

        record(&state, "anger", Outcome::Scored, json!(2.4), "t");
        assert!(d("anger", Expect::AtLeast(2.0)).holds(&state));
        assert!(!d("anger", Expect::AtMost(2.0)).holds(&state));

        record(&state, "unsure", Outcome::Unsure, Value::Null, "t");
        for e in [Expect::Yes, Expect::No] {
            assert!(!d("unsure", e).holds(&state), "unsure satisfies nothing");
        }
    }
}
