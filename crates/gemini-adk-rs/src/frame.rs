//! Frames & slots — typed, first-class fields for conversation authoring.
//!
//! Voice authors think in *frames* (a `Booking`, a `PaymentFrame`), not bare
//! state keys. A slot carries a prompt, reprompt, confirmation policy, and
//! PII/redaction policy alongside its `State` key. `#[derive(Frame)]` generates
//! the [`Frame`] impl from a struct's `#[slot(..)]` attributes; the conversation
//! compiler consumes a frame's slots for `collect` completion, and the metadata
//! drives confirmations and repair.
//!
//! ```ignore
//! use gemini_adk_rs::Frame; // the derive
//!
//! #[derive(Frame)]
//! #[frame(name = "booking")]
//! struct Booking {
//!     #[slot(prompt = "For how many people?", confirm = "low_confidence")]
//!     party_size: u8,
//!     #[slot(prompt = "What day and time?")]
//!     slot: String,
//!     #[slot(prompt = "Name for the reservation?", pii)]
//!     name: String,
//! }
//!
//! let spec = Booking::frame();
//! assert_eq!(spec.slot_keys(), vec!["party_size", "slot", "name"]);
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::extract::{Extract, Recognizer};

/// A serializable validator applied to a recognized slot value; a value failing
/// it is rejected (the slot stays unfilled until a valid value is recognized).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SlotValidator {
    /// Numeric range with optional inclusive bounds (accepts numbers, or numeric
    /// strings).
    Range {
        /// Inclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        /// Inclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
    },
    /// A non-empty (after trim) string.
    NonEmpty,
    /// A string matching this regex pattern.
    Regex(String),
    /// One of a fixed set (case-insensitive for strings).
    OneOf(Vec<String>),
}

impl SlotValidator {
    /// Whether `value` passes this validator.
    pub fn check(&self, value: &Value) -> bool {
        match self {
            SlotValidator::Range { min, max } => {
                let n = match value {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.trim().parse::<f64>().ok(),
                    _ => None,
                };
                match n {
                    Some(n) => min.is_none_or(|lo| n >= lo) && max.is_none_or(|hi| n <= hi),
                    None => false,
                }
            }
            SlotValidator::NonEmpty => value.as_str().is_some_and(|s| !s.trim().is_empty()),
            SlotValidator::Regex(pat) => regex::Regex::new(pat)
                .ok()
                .zip(value.as_str())
                .is_some_and(|(re, s)| re.is_match(s)),
            SlotValidator::OneOf(opts) => value
                .as_str()
                .is_some_and(|s| opts.iter().any(|o| o.eq_ignore_ascii_case(s))),
        }
    }
}

/// A serializable description of the deterministic recognizer that fills a slot.
///
/// Mirrors [`Recognizer`] but is serde-friendly (it holds patterns/options as
/// data, not a compiled `Regex`), so a [`FrameSpec`] — and the conversation spec
/// that embeds it — round-trips through JSON/YAML. Lower to a runtime recognizer
/// with [`SlotRecognizer::to_recognizer`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SlotRecognizer {
    /// First integer in the text.
    Integer,
    /// First integer, but only when one of these anchor words is present.
    IntegerNear(Vec<String>),
    /// A monetary amount.
    Money,
    /// First capture (or whole match) of this regex pattern.
    Regex(String),
    /// The first of these options to appear (case-insensitive substring).
    OneOf(Vec<String>),
    /// The best Jaro-Winkler match among these options.
    Fuzzy(Vec<String>),
    /// Affirmative/negative → boolean.
    YesNo,
    /// A calendar/clock expression → a JSON object.
    DateTime,
}

impl SlotRecognizer {
    /// Lower to a runtime [`Recognizer`].
    pub fn to_recognizer(&self) -> Recognizer {
        match self {
            SlotRecognizer::Integer => Recognizer::integer(),
            SlotRecognizer::IntegerNear(anchors) => Recognizer::integer_near(anchors.clone()),
            SlotRecognizer::Money => Recognizer::money(),
            SlotRecognizer::Regex(pat) => Recognizer::regex(pat),
            SlotRecognizer::OneOf(opts) => Recognizer::one_of(opts.clone()),
            SlotRecognizer::Fuzzy(opts) => Recognizer::fuzzy(opts.clone()),
            SlotRecognizer::YesNo => Recognizer::yes_no(),
            SlotRecognizer::DateTime => Recognizer::datetime(),
        }
    }
}

/// When a slot's value should be confirmed back to the user before it is trusted.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmPolicy {
    /// Never explicitly confirm.
    #[default]
    Never,
    /// Confirm only when the slot's evidence confidence is low.
    LowConfidence,
    /// Always confirm before trusting the value.
    Always,
}

impl ConfirmPolicy {
    /// Parse from an attribute string (`never`/`low_confidence`/`always`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "never" => Some(ConfirmPolicy::Never),
            "low_confidence" => Some(ConfirmPolicy::LowConfidence),
            "always" => Some(ConfirmPolicy::Always),
            _ => None,
        }
    }
}

/// Metadata for a single slot within a [`FrameSpec`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SlotSpec {
    /// The slot (field) name.
    pub name: String,
    /// The `State` key the slot is stored under (defaults to `name`).
    pub state_key: String,
    /// Prompt asked to elicit the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Reprompt used after a failed/empty first attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reprompt: Option<String>,
    /// When to confirm the slot's value.
    #[serde(default, skip_serializing_if = "is_default_confirm")]
    pub confirm: ConfirmPolicy,
    /// Whether the slot holds PII (redact in logs/transcripts).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pii: bool,
    /// The deterministic recognizer that fills this slot, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recognizer: Option<SlotRecognizer>,
    /// A validator applied to recognized values; invalid values are rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validate: Option<SlotValidator>,
}

fn is_default_confirm(c: &ConfirmPolicy) -> bool {
    *c == ConfirmPolicy::Never
}

impl SlotSpec {
    /// A bare slot with name == state key and no metadata.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            state_key: name.clone(),
            name,
            prompt: None,
            reprompt: None,
            confirm: ConfirmPolicy::Never,
            pii: false,
            recognizer: None,
            validate: None,
        }
    }
}

/// The slot definition of a frame — the source of truth for what a stage that
/// `collect`s this frame must gather, plus the metadata that drives confirmation
/// and repair.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FrameSpec {
    /// Frame name (defaults to the struct name in snake_case).
    pub name: String,
    /// The slots, in declaration order.
    pub slots: Vec<SlotSpec>,
}

impl FrameSpec {
    /// The `State` keys of every slot, in order — what a `collect` completes on.
    pub fn slot_keys(&self) -> Vec<String> {
        self.slots.iter().map(|s| s.state_key.clone()).collect()
    }

    /// Look up a slot by name.
    pub fn slot(&self, name: &str) -> Option<&SlotSpec> {
        self.slots.iter().find(|s| s.name == name)
    }

    /// Lower the frame's recognizer-bearing slots into an [`Extract`] record that
    /// fills them from the transcript. Returns `None` when no slot has a
    /// recognizer (a frame whose slots are gathered some other way).
    pub fn to_extract(&self) -> Option<Extract> {
        let mut builder = Extract::record(self.name.clone());
        let mut any = false;
        for slot in &self.slots {
            if let Some(rec) = &slot.recognizer {
                builder = builder.field_to(
                    slot.name.clone(),
                    slot.state_key.clone(),
                    rec.to_recognizer(),
                );
                if let Some(validator) = slot.validate.clone() {
                    builder = builder.validate(move |v| validator.check(v));
                }
                any = true;
            }
        }
        any.then(|| builder.build())
    }
}

/// A typed conversation frame. Implement via `#[derive(Frame)]`.
pub trait Frame {
    /// The frame's slot definition.
    fn frame() -> FrameSpec;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_policy_parses() {
        assert_eq!(ConfirmPolicy::parse("always"), Some(ConfirmPolicy::Always));
        assert_eq!(
            ConfirmPolicy::parse("low_confidence"),
            Some(ConfirmPolicy::LowConfidence)
        );
        assert_eq!(ConfirmPolicy::parse("nope"), None);
    }

    #[test]
    fn slot_validator_checks() {
        let range = SlotValidator::Range {
            min: Some(1.0),
            max: Some(12.0),
        };
        assert!(range.check(&serde_json::json!(6)));
        assert!(range.check(&serde_json::json!("4"))); // numeric string
        assert!(!range.check(&serde_json::json!(0)));
        assert!(!range.check(&serde_json::json!(13)));
        assert!(!range.check(&serde_json::json!("x")));

        assert!(SlotValidator::NonEmpty.check(&serde_json::json!("hi")));
        assert!(!SlotValidator::NonEmpty.check(&serde_json::json!("  ")));

        let one_of = SlotValidator::OneOf(vec!["pizza".into(), "salad".into()]);
        assert!(one_of.check(&serde_json::json!("PIZZA")));
        assert!(!one_of.check(&serde_json::json!("soda")));
    }

    #[test]
    fn to_extract_lowers_recognizer_slots() {
        let spec = FrameSpec {
            name: "order".into(),
            slots: vec![
                SlotSpec {
                    recognizer: Some(SlotRecognizer::OneOf(vec!["pizza".into(), "salad".into()])),
                    ..SlotSpec::new("item")
                },
                // No recognizer — not part of the extract record.
                SlotSpec::new("note"),
            ],
        };
        let extract = spec.to_extract().expect("has a recognizer slot");
        // Round-trips as part of the extract pipeline (built without panic).
        let _ = extract;

        // A frame with no recognizers lowers to no extractor.
        let bare = FrameSpec {
            name: "bare".into(),
            slots: vec![SlotSpec::new("x")],
        };
        assert!(bare.to_extract().is_none());
    }

    #[test]
    fn frame_spec_slot_keys_and_lookup() {
        let spec = FrameSpec {
            name: "booking".into(),
            slots: vec![
                SlotSpec::new("party_size"),
                SlotSpec {
                    pii: true,
                    ..SlotSpec::new("name")
                },
            ],
        };
        assert_eq!(spec.slot_keys(), vec!["party_size", "name"]);
        assert!(spec.slot("name").unwrap().pii);
        assert!(spec.slot("missing").is_none());
    }
}
