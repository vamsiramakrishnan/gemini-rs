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
    /// [`Self::decode_key`], so a state key that collides with a reserved name
    /// is carried rather than lost, and [`Self::FORMAT`] marks a delta that
    /// went through that encoding so a pre-1.0.1 one is read literally.
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
    /// applications — it is escaped around them by [`Self::encode_key`]. It is
    /// only read as a removal list on an event that carries [`Self::FORMAT`].
    pub const REMOVED_KEYS: &'static str = "adk:removed";

    /// Reserved `state_delta` entry marking an event whose keys went through
    /// [`Self::encode_key`] and whose removals live at [`Self::REMOVED_KEYS`].
    ///
    /// Events written before 1.0.1 have no such marker and no escaping: every
    /// entry in them is a literal state key, including one that happens to be
    /// named `adk:removed` or to end in `:literal`. Without this discriminator
    /// there is nothing in the delta itself to tell the two eras apart, and
    /// decoding a legacy event would shift those keys or read a stored array
    /// as a deletion list. Replay therefore decodes only marked events.
    ///
    /// A residue is structural: the marker shares the arbitrary key/value space
    /// of the deltas it discriminates, so a legacy event that stored exactly
    /// [`Self::FORMAT_VERSION`] at exactly this key would still be misread.
    /// Closing that completely takes a field outside `state_delta`, which
    /// `EventActions` cannot grow without breaking source compatibility — so
    /// the marker value is chosen to make the collision unreachable in
    /// practice rather than merely unlikely.
    pub const FORMAT: &'static str = "adk:format";

    /// Value written at [`Self::FORMAT`] by this version.
    ///
    /// A sentinel string rather than a version number: a plain `1` is a value
    /// an application could conceivably have stored under its own key, whereas
    /// this one only appears if someone wrote this crate's marker by hand.
    pub const FORMAT_VERSION: &'static str = "gemini-adk/state-delta/1";

    /// Suffix appended by [`Self::encode_key`] to step a colliding state key
    /// out of a reserved entry's way.
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
        let mut actions = Self {
            state_delta: delta,
            ..Default::default()
        };
        actions.mark_format();
        actions
    }

    /// Stamp [`Self::FORMAT`] on this delta, declaring its keys encoded and
    /// its [`Self::REMOVED_KEYS`] entry a removal list.
    pub fn mark_format(&mut self) {
        self.state_delta.insert(
            Self::FORMAT.to_string(),
            serde_json::Value::from(Self::FORMAT_VERSION),
        );
    }

    /// True when this delta carries a [`Self::FORMAT`] marker this build
    /// understands — i.e. it was written by 1.0.1 or later.
    ///
    /// An unmarked delta is a legacy one: every entry in it is a literal state
    /// key and it has no removal channel. A marker from a *newer* format than
    /// this build knows also reads as unmarked, so a forward-dated event is
    /// replayed literally rather than decoded under the wrong rules.
    pub fn is_format_marked(&self) -> bool {
        self.state_delta
            .get(Self::FORMAT)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|v| v == Self::FORMAT_VERSION)
    }

    /// The state keys this event deletes, drawn from [`Self::REMOVED_KEYS`].
    ///
    /// Always empty on a delta without [`Self::FORMAT`]: a legacy event that
    /// happens to hold an array of strings at that key stored it as a value,
    /// and replaying it as a deletion list would delete every key it names.
    pub fn removed_keys(&self) -> impl Iterator<Item = &str> {
        self.is_format_marked()
            .then(|| self.state_delta.get(Self::REMOVED_KEYS))
            .flatten()
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(serde_json::Value::as_str)
    }

    /// Map a state key to the `state_delta` key that carries it.
    ///
    /// Ordinary keys pass through. A key that would land on a reserved entry —
    /// `adk:removed` or `adk:format`, or an already-escaped form of either
    /// followed by any number of `:literal` suffixes — gains one more
    /// `:literal`. That ladder is injective and never produces a bare reserved
    /// name, so both channels stay free without any state key being dropped or
    /// overwritten.
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
    /// Apply this only to a delta where [`Self::is_format_marked`] holds. On a
    /// legacy delta every key is already literal, and stripping a `:literal`
    /// suffix there would rename a state key the application chose.
    pub fn decode_key(key: &str) -> std::borrow::Cow<'_, str> {
        match key.strip_suffix(Self::LITERAL) {
            Some(stripped) if Self::is_escape_ladder(stripped) => {
                std::borrow::Cow::Borrowed(stripped)
            }
            _ => std::borrow::Cow::Borrowed(key),
        }
    }

    /// A reserved name followed by zero or more `:literal` suffixes.
    fn is_escape_ladder(key: &str) -> bool {
        let Some(mut rest) = [Self::REMOVED_KEYS, Self::FORMAT]
            .iter()
            .find_map(|reserved| key.strip_prefix(reserved))
        else {
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
        for key in [
            "turn_count",
            "adk:removedish",
            "adk:removed:literalish",
            "adk:formatting",
        ] {
            assert_eq!(EventActions::encode_key(key), key);
            assert_eq!(EventActions::decode_key(key), key);
        }

        // Both reserved names, and every already-escaped form of either, step
        // one rung up — so nothing an application stores lands on a channel.
        for reserved in [EventActions::REMOVED_KEYS, EventActions::FORMAT] {
            let mut key = reserved.to_string();
            for _ in 0..4 {
                let encoded = EventActions::encode_key(&key).into_owned();
                assert_ne!(encoded, EventActions::REMOVED_KEYS);
                assert_ne!(encoded, EventActions::FORMAT);
                assert_ne!(encoded, key);
                assert_eq!(EventActions::decode_key(&encoded), key);
                key = encoded;
            }
        }
    }

    /// A pre-1.0.1 event can hold a real state value at the reserved key —
    /// including a string array, which is exactly the shape a removal list
    /// has. Without the format marker it is a value, never a deletion list.
    #[test]
    fn a_legacy_string_array_at_the_reserved_key_is_not_read_as_removals() {
        for payload in [
            serde_json::json!(["a", "b"]),
            serde_json::json!([]),
            serde_json::json!({"legacy": true}),
        ] {
            let mut delta = HashMap::new();
            delta.insert(EventActions::REMOVED_KEYS.to_string(), payload.clone());
            let actions = EventActions::state_delta(delta);
            assert!(!actions.is_format_marked());
            assert_eq!(
                actions.removed_keys().count(),
                0,
                "unmarked delta must have no removal channel, got {payload}"
            );
        }

        // The same array on a marked delta *is* the removal list.
        let actions = EventActions::state_removed(["a".to_string(), "b".to_string()]);
        assert!(actions.is_format_marked());
        assert_eq!(actions.removed_keys().collect::<Vec<_>>(), ["a", "b"]);
    }

    /// The marker value is a sentinel string, not a bare number: a legacy
    /// delta holding `1` — or any other ordinary value — at the marker key
    /// must not be mistaken for a marked one.
    #[test]
    fn an_ordinary_value_at_the_marker_key_does_not_mark_a_delta() {
        for value in [
            serde_json::json!(1),
            serde_json::json!("1"),
            serde_json::json!(true),
            serde_json::json!(null),
        ] {
            let mut delta = HashMap::new();
            delta.insert(EventActions::FORMAT.to_string(), value.clone());
            delta.insert(
                EventActions::REMOVED_KEYS.to_string(),
                serde_json::json!(["victim"]),
            );
            let actions = EventActions::state_delta(delta);
            assert!(
                !actions.is_format_marked(),
                "{value} at the marker key must not mark the delta"
            );
            assert_eq!(actions.removed_keys().count(), 0);
        }
    }

    /// A marker this build does not recognise must not be decoded under this
    /// build's rules — a forward-dated event replays literally instead.
    #[test]
    fn an_unrecognised_format_version_reads_as_unmarked() {
        let mut actions = EventActions::state_removed(["a".to_string()]);
        actions.state_delta.insert(
            EventActions::FORMAT.to_string(),
            serde_json::json!("gemini-adk/state-delta/2"),
        );
        assert!(!actions.is_format_marked());
        assert_eq!(actions.removed_keys().count(), 0);
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
