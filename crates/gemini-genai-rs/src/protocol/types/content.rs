//! Content primitives: Blob, FunctionCall, FunctionResponse, Part, Role, Content.

use serde::{Deserialize, Serialize};

use super::enums::FunctionResponseScheduling;

// ---------------------------------------------------------------------------
// Content primitives
// ---------------------------------------------------------------------------

/// A blob of inline data (audio, image, etc.) sent to or received from Gemini.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    /// MIME type of the data (e.g. `"audio/pcm"`, `"image/jpeg"`).
    pub mime_type: String,
    /// Base64-encoded binary data.
    pub data: String, // base64-encoded
}

/// A function call request from the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    /// Function name to call.
    pub name: String,
    /// JSON arguments for the function.
    pub args: serde_json::Value,
    /// Unique call ID for matching responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// A function call response sent back to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    /// Name of the function that was called.
    pub name: String,
    /// JSON response from the function execution.
    pub response: serde_json::Value,
    /// Call ID matching the original `FunctionCall::id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Scheduling mode for non-blocking tool responses.
    ///
    /// Only meaningful when the function was declared with
    /// [`super::FunctionCallingBehavior::NonBlocking`]. Controls how the model
    /// processes this result: immediately (interrupt), after finishing
    /// current output (when_idle), or silently (no user notification).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduling: Option<FunctionResponseScheduling>,
}

/// Executable code returned by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableCode {
    /// Programming language (e.g. `"python"`).
    pub language: String,
    /// Source code to execute.
    pub code: String,
}

/// Result of code execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeExecutionResult {
    /// Execution outcome (e.g. `"OUTCOME_OK"`, `"OUTCOME_FAILED"`).
    pub outcome: String,
    /// Standard output from execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// A single part of a `Content` message.
/// Parts are polymorphic — discriminated by field presence, not a type tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    /// A thought/reasoning part from the model (when includeThoughts is enabled).
    Thought {
        /// The thought content.
        text: String,
        /// Always true for thought parts.
        thought: bool,
    },
    /// A text part.
    Text {
        /// The text content.
        text: String,
    },
    /// An inline data blob (audio, image, etc.).
    InlineData {
        /// The blob data.
        #[serde(rename = "inlineData")]
        inline_data: Blob,
    },
    /// A function call from the model.
    FunctionCall {
        /// The function call details.
        #[serde(rename = "functionCall")]
        function_call: FunctionCall,
    },
    /// A function response sent back to the model.
    FunctionResponse {
        /// The function response details.
        #[serde(rename = "functionResponse")]
        function_response: FunctionResponse,
    },
    /// Executable code returned by the model.
    ExecutableCode {
        /// The executable code details.
        #[serde(rename = "executableCode")]
        executable_code: ExecutableCode,
    },
    /// Result of code execution.
    CodeExecutionResult {
        /// The code execution result details.
        #[serde(rename = "codeExecutionResult")]
        code_execution_result: CodeExecutionResult,
    },
}

impl Part {
    /// Create a text part.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gemini_genai_rs::protocol::types::Part;
    ///
    /// let part = Part::text("Hello, world!");
    /// ```
    pub fn text(s: impl Into<String>) -> Self {
        Part::Text { text: s.into() }
    }

    /// Create a thought part.
    pub fn thought(s: impl Into<String>) -> Self {
        Part::Thought {
            text: s.into(),
            thought: true,
        }
    }

    /// Create an inline data part (e.g. audio or image blob).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gemini_genai_rs::protocol::types::Part;
    ///
    /// let audio = Part::inline_data("audio/pcm", "AQIDBA==");
    /// let image = Part::inline_data("image/jpeg", "/9j/4AAQ...");
    /// ```
    pub fn inline_data(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Part::InlineData {
            inline_data: Blob {
                mime_type: mime_type.into(),
                data: data.into(),
            },
        }
    }

    /// Create a function call part.
    pub fn function_call(call: FunctionCall) -> Self {
        Part::FunctionCall {
            function_call: call,
        }
    }
}

/// Role in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// User role.
    User,
    /// Model role.
    Model,
    /// System role (for instructions).
    System,
}

/// A content message containing a role and a sequence of parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Content {
    /// Role of the content author (User, Model, or System).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Ordered parts that compose this content.
    pub parts: Vec<Part>,
}

impl Content {
    /// Create a user-role content with a single text part.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gemini_genai_rs::protocol::types::{Content, Role};
    ///
    /// let msg = Content::user("What is the weather?");
    /// assert_eq!(msg.role, Some(Role::User));
    /// assert_eq!(msg.parts.len(), 1);
    /// ```
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Some(Role::User),
            parts: vec![Part::text(text)],
        }
    }

    /// Create a model-role content with a single text part.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gemini_genai_rs::protocol::types::{Content, Role};
    ///
    /// let msg = Content::model("The weather is sunny.");
    /// assert_eq!(msg.role, Some(Role::Model));
    /// ```
    pub fn model(text: impl Into<String>) -> Self {
        Self {
            role: Some(Role::Model),
            parts: vec![Part::text(text)],
        }
    }

    /// Create a user-role content with a single function response part.
    pub fn function_response(name: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            role: Some(Role::User),
            parts: vec![Part::FunctionResponse {
                function_response: self::FunctionResponse {
                    name: name.into(),
                    response,
                    id: None,
                    scheduling: None,
                },
            }],
        }
    }

    /// Create a content from an explicit role and parts list.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gemini_genai_rs::protocol::types::{Content, Part, Role};
    ///
    /// let parts = vec![
    ///     Part::text("Hello"),
    ///     Part::inline_data("audio/pcm", "AQID"),
    /// ];
    /// let msg = Content::from_parts(Role::User, parts);
    /// assert_eq!(msg.parts.len(), 2);
    /// ```
    pub fn from_parts(role: Role, parts: Vec<Part>) -> Self {
        Self {
            role: Some(role),
            parts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_text_round_trip() {
        let part = Part::Text {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&part).unwrap();
        let parsed: Part = serde_json::from_str(&json).unwrap();
        assert_eq!(part, parsed);
    }

    #[test]
    fn part_inline_data_round_trip() {
        let part = Part::InlineData {
            inline_data: Blob {
                mime_type: "audio/pcm".to_string(),
                data: "AQID".to_string(),
            },
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("inlineData"));
        let parsed: Part = serde_json::from_str(&json).unwrap();
        assert_eq!(part, parsed);
    }

    #[test]
    fn part_function_call_round_trip() {
        let part = Part::FunctionCall {
            function_call: FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "London"}),
                id: Some("call-1".to_string()),
            },
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("functionCall"));
        let parsed: Part = serde_json::from_str(&json).unwrap();
        assert_eq!(part, parsed);
    }

    // ── Role enum tests ──

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(serde_json::to_string(&Role::Model).unwrap(), "\"model\"");
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
    }

    #[test]
    fn role_deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<Role>("\"user\"").unwrap(),
            Role::User
        );
        assert_eq!(
            serde_json::from_str::<Role>("\"model\"").unwrap(),
            Role::Model
        );
        assert_eq!(
            serde_json::from_str::<Role>("\"system\"").unwrap(),
            Role::System
        );
    }

    #[test]
    fn role_round_trip() {
        for role in [Role::User, Role::Model, Role::System] {
            let json = serde_json::to_string(&role).unwrap();
            let parsed: Role = serde_json::from_str(&json).unwrap();
            assert_eq!(role, parsed);
        }
    }

    // ── Content builder tests ──

    #[test]
    fn content_user_builder() {
        let c = Content::user("Hello");
        assert_eq!(c.role, Some(Role::User));
        assert_eq!(c.parts.len(), 1);
        assert!(matches!(&c.parts[0], Part::Text { text } if text == "Hello"));
    }

    #[test]
    fn content_model_builder() {
        let c = Content::model("Hi there");
        assert_eq!(c.role, Some(Role::Model));
        assert_eq!(c.parts.len(), 1);
        assert!(matches!(&c.parts[0], Part::Text { text } if text == "Hi there"));
    }

    #[test]
    fn content_function_response_builder() {
        let c = Content::function_response("get_weather", serde_json::json!({"temp": 22}));
        assert_eq!(c.role, Some(Role::User));
        assert_eq!(c.parts.len(), 1);
        match &c.parts[0] {
            Part::FunctionResponse { function_response } => {
                assert_eq!(function_response.name, "get_weather");
                assert_eq!(function_response.response, serde_json::json!({"temp": 22}));
                assert!(function_response.id.is_none());
            }
            _ => panic!("Expected FunctionResponse part"),
        }
    }

    #[test]
    fn content_from_parts_builder() {
        let parts = vec![Part::text("a"), Part::text("b")];
        let c = Content::from_parts(Role::Model, parts);
        assert_eq!(c.role, Some(Role::Model));
        assert_eq!(c.parts.len(), 2);
    }

    // ── Part builder tests ──

    #[test]
    fn part_text_builder() {
        let p = Part::text("hello");
        assert_eq!(
            p,
            Part::Text {
                text: "hello".to_string()
            }
        );
    }

    #[test]
    fn part_inline_data_builder() {
        let p = Part::inline_data("audio/pcm", "AQID");
        assert_eq!(
            p,
            Part::InlineData {
                inline_data: Blob {
                    mime_type: "audio/pcm".to_string(),
                    data: "AQID".to_string(),
                }
            }
        );
    }

    #[test]
    fn part_function_call_builder() {
        let call = FunctionCall {
            name: "test".to_string(),
            args: serde_json::json!({}),
            id: None,
        };
        let p = Part::function_call(call.clone());
        assert_eq!(
            p,
            Part::FunctionCall {
                function_call: call
            }
        );
    }

    // ── Content serialization round-trip with Role ──

    #[test]
    fn content_with_role_round_trip() {
        let c = Content::user("test message");
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        let parsed: Content = serde_json::from_str(&json).unwrap();
        assert_eq!(c, parsed);
    }
}
