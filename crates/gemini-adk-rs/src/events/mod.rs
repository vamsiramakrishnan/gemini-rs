//! Event system — structured events for agent invocations.
//!
//! Mirrors ADK-JS's event types. Each event captures a discrete action
//! within an agent invocation (user message, model response, tool call, etc.).

pub mod structured;
pub use structured::*;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A structured event within an agent invocation.
///
/// Events form the audit trail of an agent session. They capture user messages,
/// model responses, tool calls, state changes, and control flow actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event ID.
    pub id: String,
    /// Invocation ID grouping related events.
    pub invocation_id: String,
    /// Who authored this event (e.g., "user", agent name, tool name).
    pub author: String,
    /// Optional text content of the event.
    pub content: Option<String>,
    /// Actions triggered by this event.
    pub actions: EventActions,
    /// Unix timestamp (seconds).
    pub timestamp: u64,
}

impl Event {
    /// Create a new event with the given author and optional content.
    pub fn new(author: impl Into<String>, content: Option<String>) -> Self {
        let dur = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            invocation_id: String::new(),
            author: author.into(),
            content,
            actions: EventActions::default(),
            timestamp: dur.as_secs(),
        }
    }

    /// Set the invocation ID.
    pub fn with_invocation(mut self, invocation_id: impl Into<String>) -> Self {
        self.invocation_id = invocation_id.into();
        self
    }

    /// Set actions on this event.
    pub fn with_actions(mut self, actions: EventActions) -> Self {
        self.actions = actions;
        self
    }
}

/// Actions triggered by an event — control flow and state mutations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventActions {
    /// If true, escalate to a human or parent agent.
    #[serde(default)]
    pub escalate: bool,
    /// If true, skip summarization of this event's content.
    #[serde(default)]
    pub skip_summarization: bool,
    /// Transfer control to another agent by name.
    #[serde(default)]
    pub transfer_to_agent: Option<String>,
    /// State mutations (delta key → new value).
    ///
    /// Deletions travel in here too, under the reserved [`Self::REMOVED_KEYS`]
    /// entry — see that constant for why they are not spelled as `null`. State
    /// keys are written through [`Self::encode_key`] and read back through
    /// [`Self::decode_key`], so a state key that collides with the reserved
    /// name is carried rather than lost.
    #[serde(default)]
    pub state_delta: HashMap<String, serde_json::Value>,
}

impl EventActions {
    /// Reserved `state_delta` entry carrying the keys an event deleted, as a
    /// JSON array of strings.
    ///
    /// Deletion needs its own channel because `null` is a perfectly ordinary
    /// value an agent may store deliberately: using it as the tombstone made
    /// replay drop a real `null`, so persistence was not round-trip lossless
    /// for valid `State`. It rides inside `state_delta` rather than as a
    /// sibling field so that `EventActions` stays constructible by downstream
    /// code that already names every field.
    ///
    /// `State` accepts arbitrary keys, so this name is not reserved *from*
    /// applications — it is escaped around them by [`Self::encode_key`].
    pub const REMOVED_KEYS: &'static str = "adk:removed";

    /// Suffix appended by [`Self::encode_key`] to step a colliding state key
    /// out of the reserved channel's way.
    const LITERAL: &'static str = ":literal";

    /// Create actions that transfer to another agent.
    pub fn transfer(agent_name: impl Into<String>) -> Self {
        Self {
            transfer_to_agent: Some(agent_name.into()),
            ..Default::default()
        }
    }

    /// Create actions that escalate.
    pub fn escalate() -> Self {
        Self {
            escalate: true,
            ..Default::default()
        }
    }

    /// Create actions with a state delta.
    pub fn state_delta(delta: HashMap<String, serde_json::Value>) -> Self {
        Self {
            state_delta: delta,
            ..Default::default()
        }
    }

    /// Create actions that remove keys from state.
    pub fn state_removed(keys: impl IntoIterator<Item = String>) -> Self {
        let keys: Vec<serde_json::Value> =
            keys.into_iter().map(serde_json::Value::String).collect();
        let mut delta = HashMap::new();
        delta.insert(
            Self::REMOVED_KEYS.to_string(),
            serde_json::Value::Array(keys),
        );
        Self {
            state_delta: delta,
            ..Default::default()
        }
    }

    /// The state keys this event deletes, drawn from [`Self::REMOVED_KEYS`].
    ///
    /// Empty unless that entry holds an array of strings — a value of any
    /// other shape is an ordinary state value written there before the
    /// reserved entry existed, and [`Self::decode_key`] hands it back.
    pub fn removed_keys(&self) -> impl Iterator<Item = &str> {
        self.state_delta
            .get(Self::REMOVED_KEYS)
            .filter(|v| Self::is_removal_payload(v))
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(serde_json::Value::as_str)
    }

    /// True when a value at [`Self::REMOVED_KEYS`] is a removal list rather
    /// than an ordinary state value that happens to live at that key.
    pub(crate) fn is_removal_payload(value: &serde_json::Value) -> bool {
        value
            .as_array()
            .is_some_and(|items| items.iter().all(serde_json::Value::is_string))
    }

    /// Map a state key to the `state_delta` key that carries it.
    ///
    /// Ordinary keys pass through. A key that would land on the reserved
    /// removal entry — `adk:removed`, or an already-escaped `adk:removed`
    /// followed by any number of `:literal` suffixes — gains one more
    /// `:literal`. That ladder is injective and never produces the bare
    /// reserved name, so the channel stays free without any state key being
    /// dropped or overwritten.
    pub fn encode_key(key: &str) -> std::borrow::Cow<'_, str> {
        if Self::is_escape_ladder(key) {
            std::borrow::Cow::Owned(format!("{key}{}", Self::LITERAL))
        } else {
            std::borrow::Cow::Borrowed(key)
        }
    }

    /// Inverse of [`Self::encode_key`]: recover the state key a `state_delta`
    /// entry carries.
    ///
    /// The bare reserved name decodes to itself, which only happens for events
    /// written before the reserved entry existed; those stored a real value
    /// there and it is restored under its own name.
    pub fn decode_key(key: &str) -> std::borrow::Cow<'_, str> {
        match key.strip_suffix(Self::LITERAL) {
            Some(stripped) if Self::is_escape_ladder(stripped) => {
                std::borrow::Cow::Borrowed(stripped)
            }
            _ => std::borrow::Cow::Borrowed(key),
        }
    }

    /// `adk:removed` followed by zero or more `:literal` suffixes.
    fn is_escape_ladder(key: &str) -> bool {
        let Some(mut rest) = key.strip_prefix(Self::REMOVED_KEYS) else {
            return false;
        };
        while let Some(next) = rest.strip_prefix(Self::LITERAL) {
            rest = next;
        }
        rest.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_new() {
        let event = Event::new("user", Some("Hello!".to_string()));
        assert_eq!(event.author, "user");
        assert_eq!(event.content, Some("Hello!".to_string()));
        assert!(!event.id.is_empty());
        assert!(event.timestamp > 0);
    }

    #[test]
    fn event_with_invocation() {
        let event = Event::new("agent", None).with_invocation("inv-123");
        assert_eq!(event.invocation_id, "inv-123");
    }

    #[test]
    fn event_actions_transfer() {
        let actions = EventActions::transfer("helper-agent");
        assert_eq!(actions.transfer_to_agent, Some("helper-agent".to_string()));
        assert!(!actions.escalate);
    }

    #[test]
    fn event_actions_escalate() {
        let actions = EventActions::escalate();
        assert!(actions.escalate);
        assert!(actions.transfer_to_agent.is_none());
    }

    #[test]
    fn event_actions_state_delta() {
        let mut delta = HashMap::new();
        delta.insert("topic".to_string(), serde_json::json!("Rust"));
        let actions = EventActions::state_delta(delta);
        assert_eq!(
            actions.state_delta.get("topic"),
            Some(&serde_json::json!("Rust"))
        );
    }

    #[test]
    fn event_actions_state_removed_round_trips_through_the_reserved_entry() {
        let actions = EventActions::state_removed(["a".to_string(), "b".to_string()]);
        assert_eq!(actions.removed_keys().collect::<Vec<_>>(), ["a", "b"]);

        // Removals must survive the wire, not just the in-process struct.
        let json = serde_json::to_string(&actions).unwrap();
        let parsed: EventActions = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.removed_keys().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn actions_without_the_reserved_entry_remove_nothing() {
        let mut delta = HashMap::new();
        // A deliberately stored `null` is a value, not a tombstone.
        delta.insert("maybe".to_string(), serde_json::Value::Null);
        let actions = EventActions::state_delta(delta);
        assert_eq!(actions.removed_keys().count(), 0);
    }

    #[test]
    fn key_escaping_is_injective_and_frees_the_reserved_name() {
        // Ordinary keys are untouched, including near-misses.
        for key in ["turn_count", "adk:removedish", "adk:removed:literalish"] {
            assert_eq!(EventActions::encode_key(key), key);
            assert_eq!(EventActions::decode_key(key), key);
        }

        // The reserved name and every already-escaped form step one rung up,
        // so nothing an application stores can land on the bare channel.
        let mut key = EventActions::REMOVED_KEYS.to_string();
        for _ in 0..4 {
            let encoded = EventActions::encode_key(&key).into_owned();
            assert_ne!(encoded, EventActions::REMOVED_KEYS);
            assert_ne!(encoded, key);
            assert_eq!(EventActions::decode_key(&encoded), key);
            key = encoded;
        }
    }

    /// An event written before the reserved entry existed could hold a real
    /// state value at that key. Only an array of strings reads as a removal
    /// list; anything else is handed back as the value it is.
    #[test]
    fn a_non_removal_value_at_the_reserved_key_is_not_read_as_removals() {
        let mut delta = HashMap::new();
        delta.insert(
            EventActions::REMOVED_KEYS.to_string(),
            serde_json::json!({"legacy": true}),
        );
        let actions = EventActions::state_delta(delta);
        assert_eq!(actions.removed_keys().count(), 0);

        let mut delta = HashMap::new();
        delta.insert(
            EventActions::REMOVED_KEYS.to_string(),
            serde_json::json!(["a", "b"]),
        );
        let actions = EventActions::state_delta(delta);
        assert_eq!(actions.removed_keys().collect::<Vec<_>>(), ["a", "b"]);
    }

    /// `EventActions` is exhaustively constructible by downstream code, so its
    /// field set is part of the public API and cannot grow in a patch release.
    /// This literal names every field: if one is added, this test stops
    /// compiling here rather than in someone else's crate after publish.
    #[test]
    fn event_actions_stays_exhaustively_constructible() {
        let actions = EventActions {
            escalate: false,
            skip_summarization: false,
            transfer_to_agent: None,
            state_delta: HashMap::new(),
        };
        assert!(!actions.escalate);
    }

    #[test]
    fn event_serialization() {
        let event = Event::new("model", Some("Response text".to_string()));
        let json = serde_json::to_string(&event).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.author, "model");
        assert_eq!(parsed.content, Some("Response text".to_string()));
    }
}
