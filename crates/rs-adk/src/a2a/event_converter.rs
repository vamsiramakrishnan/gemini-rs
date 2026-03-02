//! Bidirectional Event <-> A2A Message conversion.

use crate::events::Event;
use super::types::{A2aMessage, A2aPart};

/// Convert an ADK Event to an A2A Message.
pub fn to_a2a_message(event: &Event) -> Option<A2aMessage> {
    let role = if event.author == "user" {
        "user"
    } else {
        "agent"
    };

    let mut parts = Vec::new();

    // Add text content as a text part
    if let Some(content) = &event.content {
        if !content.is_empty() {
            parts.push(A2aPart::Text {
                text: content.clone(),
                metadata: None,
            });
        }
    }

    if parts.is_empty() {
        return None;
    }

    Some(A2aMessage {
        message_id: event.id.clone(),
        role: role.to_string(),
        parts,
        metadata: None,
    })
}

/// Convert an A2A event/message JSON to an ADK Event.
pub fn to_adk_event(
    event_json: &serde_json::Value,
    invocation_id: &str,
    agent_name: &str,
) -> Option<Event> {
    // Try to parse as A2aMessage — look for "message" key or treat the whole object as a message
    let message = event_json.get("message").or(Some(event_json));

    if let Some(message) = message {
        let role = message
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");
        let author = if role == "user" {
            "user".to_string()
        } else {
            agent_name.to_string()
        };

        // Extract text from parts
        let content = message
            .get("parts")
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| {
                        if p.get("kind").and_then(|k| k.as_str()) == Some("text") {
                            p.get("text").and_then(|t| t.as_str()).map(String::from)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|s| !s.is_empty());

        let mut event = Event::new(author, content);
        event.invocation_id = invocation_id.to_string();
        Some(event)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_event_to_a2a_message() {
        let event = Event::new("user", Some("Hello, agent!".to_string()));
        let msg = to_a2a_message(&event).unwrap();
        assert_eq!(msg.role, "user");
        assert_eq!(msg.message_id, event.id);
        assert_eq!(msg.parts.len(), 1);
        match &msg.parts[0] {
            A2aPart::Text { text, .. } => assert_eq!(text, "Hello, agent!"),
            _ => panic!("Expected Text part"),
        }
    }

    #[test]
    fn agent_event_to_a2a_message() {
        let event = Event::new("my-agent", Some("Here is your answer.".to_string()));
        let msg = to_a2a_message(&event).unwrap();
        assert_eq!(msg.role, "agent");
        assert_eq!(msg.parts.len(), 1);
        match &msg.parts[0] {
            A2aPart::Text { text, .. } => assert_eq!(text, "Here is your answer."),
            _ => panic!("Expected Text part"),
        }
    }

    #[test]
    fn empty_content_returns_none() {
        let event = Event::new("user", None);
        assert!(to_a2a_message(&event).is_none());
    }

    #[test]
    fn empty_string_content_returns_none() {
        let event = Event::new("user", Some(String::new()));
        assert!(to_a2a_message(&event).is_none());
    }

    #[test]
    fn to_adk_event_basic() {
        let json = serde_json::json!({
            "role": "agent",
            "parts": [
                { "kind": "text", "text": "Done!" }
            ]
        });
        let event = to_adk_event(&json, "inv-1", "helper").unwrap();
        assert_eq!(event.author, "helper");
        assert_eq!(event.invocation_id, "inv-1");
        assert_eq!(event.content.as_deref(), Some("Done!"));
    }

    #[test]
    fn to_adk_event_user_role() {
        let json = serde_json::json!({
            "role": "user",
            "parts": [
                { "kind": "text", "text": "Request" }
            ]
        });
        let event = to_adk_event(&json, "inv-2", "agent-x").unwrap();
        assert_eq!(event.author, "user");
        assert_eq!(event.content.as_deref(), Some("Request"));
    }

    #[test]
    fn to_adk_event_nested_message() {
        let json = serde_json::json!({
            "message": {
                "role": "agent",
                "parts": [
                    { "kind": "text", "text": "Nested" }
                ]
            }
        });
        let event = to_adk_event(&json, "inv-3", "bot").unwrap();
        assert_eq!(event.author, "bot");
        assert_eq!(event.content.as_deref(), Some("Nested"));
    }

    #[test]
    fn to_adk_event_no_text_parts() {
        let json = serde_json::json!({
            "role": "agent",
            "parts": [
                { "kind": "data", "data": {"key": "val"} }
            ]
        });
        let event = to_adk_event(&json, "inv-4", "bot").unwrap();
        // No text parts means content is None
        assert!(event.content.is_none());
    }

    #[test]
    fn to_adk_event_multiple_text_parts_concatenated() {
        let json = serde_json::json!({
            "role": "agent",
            "parts": [
                { "kind": "text", "text": "Hello " },
                { "kind": "text", "text": "World" }
            ]
        });
        let event = to_adk_event(&json, "inv-5", "bot").unwrap();
        assert_eq!(event.content.as_deref(), Some("Hello World"));
    }
}
