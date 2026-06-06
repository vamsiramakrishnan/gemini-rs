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

/// When a slot's value should be confirmed back to the user before it is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        }
    }
}

/// The slot definition of a frame — the source of truth for what a stage that
/// `collect`s this frame must gather, plus the metadata that drives confirmation
/// and repair.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
