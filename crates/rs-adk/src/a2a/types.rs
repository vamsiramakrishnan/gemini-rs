use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// An A2A message exchanged between agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aMessage {
    /// Unique identifier for this message.
    pub message_id: String,
    /// Role of the message sender ("user" or "agent").
    pub role: String,
    /// The content parts of the message.
    pub parts: Vec<A2aPart>,
    /// Optional metadata attached to the message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// A2A part — discriminated by kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum A2aPart {
    /// A plain text part.
    #[serde(rename = "text")]
    Text {
        /// The text content.
        text: String,
        /// Optional metadata for this part.
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<HashMap<String, serde_json::Value>>,
    },
    /// A file attachment part.
    #[serde(rename = "file")]
    File {
        /// The file content (inline bytes or URI).
        file: A2aFileContent,
        /// Optional metadata for this part.
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<HashMap<String, serde_json::Value>>,
    },
    /// A structured data part (function calls, code execution, etc.).
    #[serde(rename = "data")]
    Data {
        /// The structured data payload.
        data: serde_json::Value,
        /// Optional metadata for this part.
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<HashMap<String, serde_json::Value>>,
    },
}

/// File content in A2A — either inline bytes or URI reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aFileContent {
    /// File name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// MIME type of the file content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Inline base64-encoded bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
    /// URI reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// Status of an A2A task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    /// Task has been submitted but not yet started.
    Submitted,
    /// Task is actively being processed.
    Working,
    /// Task requires additional input from the user.
    InputRequired,
    /// Task has completed successfully.
    Completed,
    /// Task was canceled by the user or system.
    Canceled,
    /// Task failed with an error.
    Failed,
    /// Task state is unknown.
    Unknown,
}

/// A2A task status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    /// Current state of the task.
    pub state: TaskState,
    /// Optional message associated with the status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<A2aMessage>,
    /// ISO 8601 timestamp of the status update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// An A2A task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aTask {
    /// Unique task identifier.
    pub id: String,
    /// Context identifier for grouping related tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// Current status of the task.
    pub status: TaskStatus,
    /// Artifacts produced by the task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<A2aArtifact>>,
    /// Optional task-level metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// An A2A artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aArtifact {
    /// Artifact name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Parts composing this artifact.
    pub parts: Vec<A2aPart>,
    /// Optional artifact-level metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Task status update event (streaming).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdateEvent {
    /// Task identifier.
    pub id: String,
    /// Updated task status.
    pub status: TaskStatus,
    /// Whether this is the final status update for the task.
    #[serde(rename = "final")]
    pub is_final: bool,
    /// Optional event-level metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Task artifact update event (streaming).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdateEvent {
    /// Task identifier.
    pub id: String,
    /// The updated artifact.
    pub artifact: A2aArtifact,
    /// Optional event-level metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a2a_message_round_trip_with_text_part() {
        let msg = A2aMessage {
            message_id: "msg-1".to_string(),
            role: "user".to_string(),
            parts: vec![A2aPart::Text {
                text: "Hello, agent!".to_string(),
                metadata: None,
            }],
            metadata: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: A2aMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.message_id, "msg-1");
        assert_eq!(deserialized.role, "user");
        assert_eq!(deserialized.parts.len(), 1);
        match &deserialized.parts[0] {
            A2aPart::Text { text, metadata } => {
                assert_eq!(text, "Hello, agent!");
                assert!(metadata.is_none());
            }
            _ => panic!("Expected Text part"),
        }

        // Verify camelCase field naming
        assert!(json.contains("\"messageId\""));
        assert!(!json.contains("\"message_id\""));
    }

    #[test]
    fn a2a_part_text_serializes_with_kind_tag() {
        let part = A2aPart::Text {
            text: "hello".to_string(),
            metadata: None,
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"kind\":\"text\""));
        assert!(json.contains("\"text\":\"hello\""));
    }

    #[test]
    fn a2a_part_file_serializes_with_kind_tag() {
        let part = A2aPart::File {
            file: A2aFileContent {
                name: Some("image.png".to_string()),
                mime_type: Some("image/png".to_string()),
                bytes: Some("base64data".to_string()),
                uri: None,
            },
            metadata: None,
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"kind\":\"file\""));
        assert!(json.contains("\"image.png\""));
        assert!(json.contains("\"mimeType\""));

        let deserialized: A2aPart = serde_json::from_str(&json).unwrap();
        match deserialized {
            A2aPart::File { file, .. } => {
                assert_eq!(file.name.as_deref(), Some("image.png"));
                assert_eq!(file.mime_type.as_deref(), Some("image/png"));
                assert_eq!(file.bytes.as_deref(), Some("base64data"));
                assert!(file.uri.is_none());
            }
            _ => panic!("Expected File part"),
        }
    }

    #[test]
    fn a2a_part_data_serializes_with_kind_tag() {
        let part = A2aPart::Data {
            data: serde_json::json!({"key": "value", "count": 42}),
            metadata: None,
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"kind\":\"data\""));

        let deserialized: A2aPart = serde_json::from_str(&json).unwrap();
        match deserialized {
            A2aPart::Data { data, .. } => {
                assert_eq!(data["key"], "value");
                assert_eq!(data["count"], 42);
            }
            _ => panic!("Expected Data part"),
        }
    }

    #[test]
    fn a2a_file_content_with_bytes_vs_uri() {
        // With bytes
        let with_bytes = A2aFileContent {
            name: Some("doc.pdf".to_string()),
            mime_type: Some("application/pdf".to_string()),
            bytes: Some("cGRmY29udGVudA==".to_string()),
            uri: None,
        };
        let json = serde_json::to_string(&with_bytes).unwrap();
        assert!(json.contains("\"bytes\""));
        assert!(!json.contains("\"uri\""));

        // With URI
        let with_uri = A2aFileContent {
            name: Some("doc.pdf".to_string()),
            mime_type: Some("application/pdf".to_string()),
            bytes: None,
            uri: Some("gs://bucket/doc.pdf".to_string()),
        };
        let json = serde_json::to_string(&with_uri).unwrap();
        assert!(!json.contains("\"bytes\""));
        assert!(json.contains("\"uri\""));
        assert!(json.contains("gs://bucket/doc.pdf"));
    }

    #[test]
    fn task_state_serde_camel_case() {
        assert_eq!(
            serde_json::to_string(&TaskState::Submitted).unwrap(),
            "\"submitted\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::Working).unwrap(),
            "\"working\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::InputRequired).unwrap(),
            "\"inputRequired\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::Canceled).unwrap(),
            "\"canceled\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::Unknown).unwrap(),
            "\"unknown\""
        );

        // Round-trip
        let state: TaskState = serde_json::from_str("\"inputRequired\"").unwrap();
        assert_eq!(state, TaskState::InputRequired);
    }

    #[test]
    fn a2a_task_with_status_and_artifacts() {
        let task = A2aTask {
            id: "task-123".to_string(),
            context_id: Some("ctx-456".to_string()),
            status: TaskStatus {
                state: TaskState::Completed,
                message: Some(A2aMessage {
                    message_id: "msg-done".to_string(),
                    role: "agent".to_string(),
                    parts: vec![A2aPart::Text {
                        text: "Done!".to_string(),
                        metadata: None,
                    }],
                    metadata: None,
                }),
                timestamp: Some("2026-03-02T12:00:00Z".to_string()),
            },
            artifacts: Some(vec![A2aArtifact {
                name: Some("result".to_string()),
                parts: vec![A2aPart::Data {
                    data: serde_json::json!({"answer": 42}),
                    metadata: None,
                }],
                metadata: None,
            }]),
            metadata: None,
        };

        let json = serde_json::to_string_pretty(&task).unwrap();
        let deserialized: A2aTask = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "task-123");
        assert_eq!(deserialized.context_id.as_deref(), Some("ctx-456"));
        assert_eq!(deserialized.status.state, TaskState::Completed);
        assert!(deserialized.status.message.is_some());
        assert!(deserialized.artifacts.is_some());
        assert_eq!(deserialized.artifacts.as_ref().unwrap().len(), 1);

        // Verify camelCase for contextId
        assert!(json.contains("\"contextId\""));
    }

    #[test]
    fn task_status_update_event_final_field_rename() {
        let event = TaskStatusUpdateEvent {
            id: "task-789".to_string(),
            status: TaskStatus {
                state: TaskState::Working,
                message: None,
                timestamp: None,
            },
            is_final: false,
            metadata: None,
        };

        let json = serde_json::to_string(&event).unwrap();
        // "final" is used in JSON, not "is_final" or "isFinal"
        assert!(json.contains("\"final\""));
        assert!(!json.contains("\"is_final\""));
        assert!(!json.contains("\"isFinal\""));

        // Deserialize from JSON with "final" key
        let json_input = r#"{"id":"task-789","status":{"state":"completed"},"final":true}"#;
        let deserialized: TaskStatusUpdateEvent = serde_json::from_str(json_input).unwrap();
        assert!(deserialized.is_final);
        assert_eq!(deserialized.status.state, TaskState::Completed);
    }

    #[test]
    fn task_artifact_update_event_round_trip() {
        let event = TaskArtifactUpdateEvent {
            id: "task-100".to_string(),
            artifact: A2aArtifact {
                name: Some("output".to_string()),
                parts: vec![A2aPart::Text {
                    text: "Generated content".to_string(),
                    metadata: None,
                }],
                metadata: None,
            },
            metadata: Some({
                let mut m = HashMap::new();
                m.insert("source".to_string(), serde_json::json!("agent-a"));
                m
            }),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: TaskArtifactUpdateEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "task-100");
        assert_eq!(deserialized.artifact.name.as_deref(), Some("output"));
        assert!(deserialized.metadata.is_some());
        assert_eq!(
            deserialized.metadata.as_ref().unwrap()["source"],
            serde_json::json!("agent-a")
        );
    }
}
