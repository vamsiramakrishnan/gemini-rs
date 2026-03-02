//! Structured event types — typed wrappers over raw events.
//!
//! Converts persisted `Event` records into typed `StructuredEvent` variants
//! for easier consumption by UI layers and analytics.

use serde_json::Value;

/// Classification of a structured event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventType {
    Thought,
    Content,
    ToolCall,
    ToolResult,
    CallCode,
    CodeResult,
    Error,
    Activity,
    ToolConfirmation,
    Finished,
}

/// A typed, structured representation of an agent event.
#[derive(Debug, Clone)]
pub enum StructuredEvent {
    Thought { text: String },
    Content { text: String, author: String },
    ToolCall { name: String, args: Value, call_id: Option<String> },
    ToolResult { name: String, result: Value },
    Error { message: String },
    Activity { description: String },
    ToolConfirmation { hint: Option<String>, confirmed: bool },
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
