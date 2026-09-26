//! Voice timing per stage: how the conversation *sounds* while a step is
//! active.
//!
//! A flow decides what may happen. A [`VoiceTiming`] decides the pacing:
//!
//! - how long to wait out the user's silence before asking again;
//! - when to cue a filler while a tool runs;
//! - whether the user can talk over the model;
//! - how long a pause must last before the user's turn is over;
//! - whether steering context goes out at once or rides the user's next
//!   message.
//!
//! Timings attach to steps on the [`FlowStack`](super::FlowStack). Whenever
//! the active step changes, the stack publishes the merged timing of the
//! active steps to [`VOICE_TIMING_KEY`] in state, and the runtime reads it
//! there:
//!
//! | Setting | Applied by |
//! |---|---|
//! | `reprompt_after_ms` | the control lane: after that much user silence, it sends the reprompt and emits `LiveEvent::Reprompted` |
//! | `filler_after_ms` | tool dispatch: a call still running after that long emits `LiveEvent::FillerCue` for the app to play an earcon or line |
//! | `interruptible: false` | `LiveHandle::send_audio`: while the model speaks, mic audio is replaced by silence, so neither VAD can cut the model off |
//! | `end_of_speech_ms` | `LiveHandle::send_audio`: the turn-commit end-of-turn hold, under client activity authority. The server's VAD is fixed at setup, so with server authority this has no effect. |
//! | `context_delivery` | the turn lifecycle: overrides the session's context delivery for this stage |
//!
//! Everything here is serializable, so a `ConversationSpec` carries it and a
//! simulator can inspect it.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::live::steering::ContextDelivery;

/// The state key the active stage's merged timing is published under.
pub const VOICE_TIMING_KEY: &str = "session:voice_timing";

/// The reprompt sent when a stage sets `reprompt_after_ms` but no text.
pub const DEFAULT_REPROMPT: &str = "The user has not answered. Briefly repeat or rephrase your last question, and do not add anything new.";

/// Voice pacing for one stage. Every field is optional; an unset field leaves
/// the session's own behaviour alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct VoiceTiming {
    /// Reprompt once the user has been silent this long (ms) with the floor
    /// theirs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reprompt_after_ms: Option<u64>,
    /// What to tell the model when reprompting. Defaults to
    /// [`DEFAULT_REPROMPT`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reprompt: Option<String>,
    /// Cue a filler when a tool call runs longer than this (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filler_after_ms: Option<u64>,
    /// `Some(false)` holds the floor for the model: the user cannot barge in
    /// while it speaks (statutory readouts, disclosures).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interruptible: Option<bool>,
    /// How long a pause must last before the user's turn ends (ms), under
    /// client activity authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_of_speech_ms: Option<u64>,
    /// Context delivery while this stage is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_delivery: Option<ContextDelivery>,
}

impl VoiceTiming {
    /// No timing overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reprompt after `silence` with the default reprompt.
    pub fn reprompt_after(mut self, silence: Duration) -> Self {
        self.reprompt_after_ms = Some(millis(silence));
        self
    }

    /// Reprompt after `silence`, telling the model `text`.
    pub fn reprompt_with(mut self, silence: Duration, text: impl Into<String>) -> Self {
        self.reprompt_after_ms = Some(millis(silence));
        self.reprompt = Some(text.into());
        self
    }

    /// Cue a filler once a tool call has run for `after`.
    pub fn filler_after(mut self, after: Duration) -> Self {
        self.filler_after_ms = Some(millis(after));
        self
    }

    /// The user cannot interrupt the model in this stage.
    pub fn uninterruptible(mut self) -> Self {
        self.interruptible = Some(false);
        self
    }

    /// The user's turn ends after a pause of `pause`.
    pub fn end_of_speech(mut self, pause: Duration) -> Self {
        self.end_of_speech_ms = Some(millis(pause));
        self
    }

    /// Deliver steering context this way while the stage is active.
    pub fn context_delivery(mut self, delivery: ContextDelivery) -> Self {
        self.context_delivery = Some(delivery);
        self
    }

    /// Whether nothing is overridden.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Whether the model holds the floor (the user cannot barge in).
    pub fn holds_floor(&self) -> bool {
        self.interruptible == Some(false)
    }

    /// The reprompt text to send.
    pub fn reprompt_text(&self) -> &str {
        self.reprompt.as_deref().unwrap_or(DEFAULT_REPROMPT)
    }

    /// Combine the timing of two steps active at once, taking the more
    /// cautious setting of each: the shorter reprompt and filler waits, the
    /// longer end-of-speech pause, and no barge-in if either forbids it.
    /// Context delivery and reprompt text come from `self` when set.
    pub fn merge(&self, other: &Self) -> Self {
        fn min(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        Self {
            reprompt_after_ms: min(self.reprompt_after_ms, other.reprompt_after_ms),
            reprompt: self.reprompt.clone().or_else(|| other.reprompt.clone()),
            filler_after_ms: min(self.filler_after_ms, other.filler_after_ms),
            interruptible: match (self.interruptible, other.interruptible) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (a, b) => a.or(b),
            },
            end_of_speech_ms: self.end_of_speech_ms.max(other.end_of_speech_ms),
            context_delivery: self.context_delivery.or(other.context_delivery),
        }
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_takes_the_cautious_setting() {
        let a = VoiceTiming::new()
            .reprompt_after(Duration::from_secs(8))
            .end_of_speech(Duration::from_millis(400));
        let b = VoiceTiming::new()
            .reprompt_after(Duration::from_secs(5))
            .uninterruptible()
            .end_of_speech(Duration::from_millis(900));
        let m = a.merge(&b);
        assert_eq!(m.reprompt_after_ms, Some(5_000));
        assert!(m.holds_floor());
        assert_eq!(m.end_of_speech_ms, Some(900));
    }

    #[test]
    fn round_trips_json_and_omits_unset_fields() {
        let t = VoiceTiming::new()
            .filler_after(Duration::from_millis(1500))
            .context_delivery(ContextDelivery::Deferred);
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "filler_after_ms": 1500, "context_delivery": "deferred" })
        );
        assert_eq!(serde_json::from_value::<VoiceTiming>(json).unwrap(), t);
        assert!(VoiceTiming::new().is_empty());
    }
}
