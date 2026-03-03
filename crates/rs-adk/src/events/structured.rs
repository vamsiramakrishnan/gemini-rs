//! Structured event types — typed wrappers over raw events.
//!
//! Converts persisted `Event` records into typed `StructuredEvent` variants
//! for easier consumption by UI layers and analytics.

use serde_json::Value;

/// Classification of a structured event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventType {
    /// Internal reasoning or chain-of-thought.
    Thought,
    /// User-facing content output.
    Content,
    /// A tool/function call from the model.
    ToolCall,
    /// The result of a tool/function call.
    ToolResult,
    /// A code execution request.
    CallCode,
    /// The result of a code execution.
    CodeResult,
    /// An error event.
    Error,
    /// An activity/status update (e.g., agent transfer).
    Activity,
    /// A tool confirmation request or response.
    ToolConfirmation,
    /// The agent has finished processing.
    Finished,
}

/// A typed, structured representation of an agent event.
#[derive(Debug, Clone)]
pub enum StructuredEvent {
    /// Internal reasoning or chain-of-thought text.
    Thought {
        /// The reasoning text.
        text: String,
    },
    /// User-facing content with author attribution.
    Content {
        /// The content text.
        text: String,
        /// Who authored this content (e.g., "model", "agent").
        author: String,
    },
    /// A tool/function call from the model.
    ToolCall {
        /// Tool name.
        name: String,
        /// Tool arguments as JSON.
        args: Value,
        /// Optional unique call identifier.
        call_id: Option<String>,
    },
    /// The result of a tool/function call.
    ToolResult {
        /// Tool name.
        name: String,
        /// The result value.
        result: Value,
    },
    /// An error event.
    Error {
        /// Error description.
        message: String,
    },
    /// An activity/status update (e.g., agent transfer, escalation).
    Activity {
        /// Human-readable description of the activity.
        description: String,
    },
    /// A tool confirmation request or response.
    ToolConfirmation {
        /// Optional hint for the confirmation UI.
        hint: Option<String>,
        /// Whether the tool call was confirmed.
        confirmed: bool,
    },
    /// The agent has finished processing.
    Finished,
}

impl StructuredEvent {
    /// Returns the classification of this event.
    pub fn event_type(&self) -> EventType {
        match self {
            Self::Thought { .. } => EventType::Thought,
            Self::Content { .. } => EventType::Content,
            Self::ToolCall { .. } => EventType::ToolCall,
            Self::ToolResult { .. } => EventType::ToolResult,
            Self::Error { .. } => EventType::Error,
            Self::Activity { .. } => EventType::Activity,
            Self::ToolConfirmation { .. } => EventType::ToolConfirmation,
            Self::Finished => EventType::Finished,
        }
    }
}

/// Convert a persisted `Event` into typed `StructuredEvent`s.
///
/// A single `Event` may produce multiple structured events (e.g., content + transfer activity).
pub fn to_structured_events(event: &super::Event) -> Vec<StructuredEvent> {
    let mut out = Vec::new();

    // Content → Content or Thought
    if let Some(text) = &event.content {
        if !text.is_empty() {
            out.push(StructuredEvent::Content {
                text: text.clone(),
                author: event.author.clone(),
            });
        }
    }

    // Transfer → Activity
    if let Some(target) = &event.actions.transfer_to_agent {
        out.push(StructuredEvent::Activity {
            description: format!("Transferring to agent: {target}"),
        });
    }

    // Escalate → Activity
    if event.actions.escalate {
        out.push(StructuredEvent::Activity {
            description: "Escalating to human/parent agent".to_string(),
        });
    }

    // State delta with tool-related keys → ToolResult hints
    for (key, value) in &event.actions.state_delta {
        if key.starts_with("tool:result:") {
            let tool_name = key.strip_prefix("tool:result:").unwrap_or(key);
            out.push(StructuredEvent::ToolResult {
                name: tool_name.to_string(),
                result: value.clone(),
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventActions};
    use std::collections::HashMap;

    #[test]
    fn event_type_classification() {
        let thought = StructuredEvent::Thought { text: "hmm".into() };
        assert_eq!(thought.event_type(), EventType::Thought);

        let content = StructuredEvent::Content { text: "hi".into(), author: "model".into() };
        assert_eq!(content.event_type(), EventType::Content);

        assert_eq!(StructuredEvent::Finished.event_type(), EventType::Finished);
    }

    #[test]
    fn convert_event_with_content() {
        let event = Event::new("model", Some("Hello there!".to_string()));
        let structured = to_structured_events(&event);
        assert_eq!(structured.len(), 1);
        match &structured[0] {
            StructuredEvent::Content { text, author } => {
                assert_eq!(text, "Hello there!");
                assert_eq!(author, "model");
            }
            other => panic!("expected Content, got: {other:?}"),
        }
    }

    #[test]
    fn convert_event_with_transfer() {
        let actions = EventActions::transfer("helper");
        let event = Event::new("agent", None).with_actions(actions);
        let structured = to_structured_events(&event);
        assert_eq!(structured.len(), 1);
        match &structured[0] {
            StructuredEvent::Activity { description } => {
                assert!(description.contains("helper"));
            }
            other => panic!("expected Activity, got: {other:?}"),
        }
    }

    #[test]
    fn convert_event_with_content_and_transfer() {
        let actions = EventActions::transfer("target");
        let event = Event::new("agent", Some("Handing off".to_string())).with_actions(actions);
        let structured = to_structured_events(&event);
        assert_eq!(structured.len(), 2);
        assert_eq!(structured[0].event_type(), EventType::Content);
        assert_eq!(structured[1].event_type(), EventType::Activity);
    }

    #[test]
    fn convert_empty_event() {
        let event = Event::new("system", None);
        let structured = to_structured_events(&event);
        assert!(structured.is_empty());
    }

    #[test]
    fn convert_event_with_tool_result_in_state_delta() {
        let mut delta = HashMap::new();
        delta.insert("tool:result:get_weather".to_string(), serde_json::json!({"temp": 22}));
        let actions = EventActions::state_delta(delta);
        let event = Event::new("model", None).with_actions(actions);
        let structured = to_structured_events(&event);
        assert_eq!(structured.len(), 1);
        match &structured[0] {
            StructuredEvent::ToolResult { name, result } => {
                assert_eq!(name, "get_weather");
                assert_eq!(result["temp"], 22);
            }
            other => panic!("expected ToolResult, got: {other:?}"),
        }
    }

    #[test]
    fn convert_event_with_escalation() {
        let actions = EventActions::escalate();
        let event = Event::new("agent", None).with_actions(actions);
        let structured = to_structured_events(&event);
        assert_eq!(structured.len(), 1);
        assert_eq!(structured[0].event_type(), EventType::Activity);
    }
}
