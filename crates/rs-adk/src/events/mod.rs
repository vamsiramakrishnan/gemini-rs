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
    /// State mutations (key → new value).
    #[serde(default)]
    pub state_delta: HashMap<String, serde_json::Value>,
}

impl EventActions {
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
    fn event_serialization() {
        let event = Event::new("model", Some("Response text".to_string()));
        let json = serde_json::to_string(&event).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.author, "model");
        assert_eq!(parsed.content, Some("Response text".to_string()));
    }
}
