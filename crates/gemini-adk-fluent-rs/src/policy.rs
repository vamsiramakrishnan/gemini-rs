//! Policy aspects — reusable, cross-cutting governance attached to a whole
//! conversation rather than scattered across stages.
//!
//! Compliance is where regulated voice flows live, and it should *feel* like
//! attaching an aspect, not hand-wiring guards everywhere. A [`Policy`] is a
//! serializable aspect applied with
//! [`Conversation::policy`](crate::conversation::Conversation::policy); the
//! compiler lowers it into concrete machinery (a safety digression, a redaction
//! set, commit governance), always through the validated IR.
//!
//! ```ignore
//! Conversation::new("payment")
//!     .policy(Policy::redact(["card_number", "cvv"]))
//!     .policy(Policy::commit("charge_card").idempotency_key("{user_id}:{amount}").compensate_with("refund"))
//!     .policy(Policy::safety_handoff(["self_harm", "abuse"]))
//!     /* … stages … */
//!     .compile()?;
//! ```

use serde::{Deserialize, Serialize};

/// A reusable, cross-cutting policy aspect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Policy {
    /// Hand off (terminate the conversation) when any of these intents is
    /// detected (the `intent:{name}` flag becomes true). Lowered to a `safety`
    /// digression with `Resume::Terminate`.
    SafetyHandoff {
        /// Intent names that trigger handoff.
        intents: Vec<String>,
    },
    /// Redact these state keys in logs/transcripts. Recorded for the runtime's
    /// logging layer; pairs with `#[slot(pii)]`.
    Redact {
        /// State keys to redact.
        keys: Vec<String>,
    },
    /// Commit-tool governance: idempotency and compensation metadata for a
    /// confirm-before-act tool.
    Commit {
        /// The committing tool.
        tool: String,
        /// Idempotency key template (`{key}` interpolated from `State`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
        /// Tool that compensates (undoes) this commit on failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compensate_with: Option<String>,
    },
}

impl Policy {
    /// Terminate/hand off when any of `intents` is detected.
    pub fn safety_handoff<I, S>(intents: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Policy::SafetyHandoff {
            intents: intents.into_iter().map(Into::into).collect(),
        }
    }

    /// Redact these state keys in logs/transcripts.
    pub fn redact<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Policy::Redact {
            keys: keys.into_iter().map(Into::into).collect(),
        }
    }

    /// Begin a commit-governance policy for `tool`.
    pub fn commit(tool: impl Into<String>) -> CommitPolicy {
        CommitPolicy {
            tool: tool.into(),
            idempotency_key: None,
            compensate_with: None,
        }
    }

    /// The state keys this policy marks for redaction (empty for non-redact).
    pub fn redacted_keys(&self) -> &[String] {
        match self {
            Policy::Redact { keys } => keys,
            _ => &[],
        }
    }
}

/// Builder for a [`Policy::Commit`] governance aspect.
#[derive(Debug, Clone)]
pub struct CommitPolicy {
    tool: String,
    idempotency_key: Option<String>,
    compensate_with: Option<String>,
}

impl CommitPolicy {
    /// Set the idempotency key template (`{key}` interpolated from `State`).
    pub fn idempotency_key(mut self, template: impl Into<String>) -> Self {
        self.idempotency_key = Some(template.into());
        self
    }

    /// Set the compensating tool (undoes the commit on failure).
    pub fn compensate_with(mut self, tool: impl Into<String>) -> Self {
        self.compensate_with = Some(tool.into());
        self
    }
}

impl From<CommitPolicy> for Policy {
    fn from(c: CommitPolicy) -> Self {
        Policy::Commit {
            tool: c.tool,
            idempotency_key: c.idempotency_key,
            compensate_with: c.compensate_with,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_builder_into_policy() {
        let p: Policy = Policy::commit("charge_card")
            .idempotency_key("{user_id}:{amount}")
            .compensate_with("refund")
            .into();
        assert_eq!(
            p,
            Policy::Commit {
                tool: "charge_card".into(),
                idempotency_key: Some("{user_id}:{amount}".into()),
                compensate_with: Some("refund".into()),
            }
        );
    }

    #[test]
    fn policies_round_trip_through_json() {
        let policies = vec![
            Policy::redact(["card_number", "cvv"]),
            Policy::safety_handoff(["self_harm"]),
            Policy::commit("book").idempotency_key("{id}").into(),
        ];
        let json = serde_json::to_string(&policies).unwrap();
        let back: Vec<Policy> = serde_json::from_str(&json).unwrap();
        assert_eq!(policies, back);
        assert_eq!(back[0].redacted_keys(), &["card_number", "cvv"]);
    }
}
