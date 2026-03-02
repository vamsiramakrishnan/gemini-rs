//! LLM abstraction — decouples agents from specific model providers.
//!
//! The `BaseLlm` trait provides a unified interface for generating content
//! from any LLM. The `GeminiLlm` implementation wraps rs-genai's `Client`
//! for Gemini models.

pub mod gemini;
pub mod registry;

pub use gemini::{GeminiLlm, GeminiLlmParams};
pub use registry::LlmRegistry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use rs_genai::prelude::{Content, Part, Tool};

/// Configuration for an LLM generation request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmRequest {
    /// The messages/contents to send.
    pub contents: Vec<Content>,
    /// System instruction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<String>,
    /// Available tools.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<Tool>,
    /// Temperature for generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl LlmRequest {
    /// Create a request from a single user message.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            contents: vec![Content {
                role: Some(rs_genai::prelude::Role::User),
                parts: vec![Part::Text {
                    text: text.into(),
                }],
            }],
            ..Default::default()
        }
    }

    /// Create a request from existing contents.
    pub fn from_contents(contents: Vec<Content>) -> Self {
        Self {
            contents,
            ..Default::default()
        }
    }
}

/// The response from an LLM generation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    /// The generated content.
    pub content: Content,
    /// Finish reason (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Token usage (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

impl LlmResponse {
    /// Extract text from the response, concatenating all text parts.
    pub fn text(&self) -> String {
        self.content
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract function calls from the response.
    pub fn function_calls(&self) -> Vec<&rs_genai::prelude::FunctionCall> {
        self.content
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::FunctionCall { function_call } => Some(function_call),
                _ => None,
            })
            .collect()
    }
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input/prompt tokens.
    pub prompt_tokens: u32,
    /// Output/completion tokens.
    pub completion_tokens: u32,
    /// Total tokens.
    pub total_tokens: u32,
}

/// Errors from LLM operations.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM request failed: {0}")]
    RequestFailed(String),
    #[error("Model not available: {0}")]
    ModelNotAvailable(String),
    #[error("Rate limited")]
    RateLimited,
    #[error("Content filtered")]
    ContentFiltered,
    #[error("{0}")]
    Other(String),
}

/// Trait for LLM providers — decouples agents from specific models.
///
/// Implementations must be `Send + Sync` for use across async tasks.
#[async_trait]
pub trait BaseLlm: Send + Sync {
    /// The model identifier (e.g., "gemini-2.5-flash").
    fn model_id(&self) -> &str;

    /// Generate content from the LLM.
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_request_from_text() {
        let req = LlmRequest::from_text("Hello!");
        assert_eq!(req.contents.len(), 1);
        assert!(req.system_instruction.is_none());
        assert!(req.tools.is_empty());
    }

    #[test]
    fn llm_request_from_contents() {
        let contents = vec![Content {
            role: Some(rs_genai::prelude::Role::User),
            parts: vec![Part::Text {
                text: "Hello".into(),
            }],
        }];
        let req = LlmRequest::from_contents(contents);
        assert_eq!(req.contents.len(), 1);
    }

    #[test]
    fn llm_response_text() {
        let resp = LlmResponse {
            content: Content {
                role: Some(rs_genai::prelude::Role::Model),
                parts: vec![
                    Part::Text {
                        text: "Hello ".into(),
                    },
                    Part::Text {
                        text: "world!".into(),
                    },
                ],
            },
            finish_reason: Some("STOP".into()),
            usage: None,
        };
        assert_eq!(resp.text(), "Hello world!");
    }

    #[test]
    fn llm_response_function_calls() {
        let resp = LlmResponse {
            content: Content {
                role: Some(rs_genai::prelude::Role::Model),
                parts: vec![Part::FunctionCall {
                    function_call: rs_genai::prelude::FunctionCall {
                        name: "get_weather".into(),
                        args: serde_json::json!({"city": "London"}),
                        id: None,
                    },
                }],
            },
            finish_reason: None,
            usage: None,
        };
        let calls = resp.function_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
    }

    #[test]
    fn base_llm_is_object_safe() {
        fn _assert(_: &dyn BaseLlm) {}
    }

    #[test]
    fn token_usage() {
        let usage = TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
        };
        assert_eq!(usage.total_tokens, 30);
    }
}
