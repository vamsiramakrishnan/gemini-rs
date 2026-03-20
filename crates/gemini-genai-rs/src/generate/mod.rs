//! generateContent and streamGenerateContent REST API.
//!
//! This module provides typed request/response types and a client for the
//! Gemini generateContent REST API. Feature-gated behind `generate`.
//!
//! # Usage
//!
//! ```ignore
//! use gemini_genai_rs::prelude::*;
//!
//! let client = Client::from_api_key("your-key")
//!     .model(GeminiModel::Custom("gemini-2.5-flash".into()));
//!
//! let response = client.generate_content("What is Rust?").await?;
//! println!("{}", response.text().unwrap_or_default());
//! ```

mod config;
mod response;

pub use config::GenerateContentConfig;
pub use response::{BlockReason, Candidate, GenerateContentResponse, PromptFeedback};

use crate::client::http::HttpError;
use crate::client::Client;
use crate::protocol::types::GeminiModel;
use crate::transport::auth::ServiceEndpoint;

impl Client {
    /// Generate content from a text prompt using the default model.
    pub async fn generate_content(
        &self,
        prompt: impl Into<String>,
    ) -> Result<GenerateContentResponse, GenerateError> {
        let config = GenerateContentConfig::from_text(prompt);
        self.generate_content_with(config, None).await
    }

    /// Generate content with full configuration and optional model override.
    pub async fn generate_content_with(
        &self,
        config: GenerateContentConfig,
        model: Option<&GeminiModel>,
    ) -> Result<GenerateContentResponse, GenerateError> {
        let model = model.unwrap_or(self.default_model());
        let url = self.rest_url_for(ServiceEndpoint::GenerateContent, model);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| GenerateError::Auth(e.to_string()))?;

        let body = config.to_request_body();
        let json = self
            .http_client()
            .post_json(&url, headers, &body)
            .await
            .map_err(GenerateError::from)?;

        let response: GenerateContentResponse = serde_json::from_value(json)?;
        Ok(response)
    }
}

/// Errors specific to the Generate API.
#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    /// HTTP transport error.
    #[error(transparent)]
    Http(#[from] HttpError),

    /// JSON deserialization error.
    #[error("Failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),

    /// Authentication error.
    #[error("Auth error: {0}")]
    Auth(String),

    /// Content was blocked by safety filters.
    #[error("Content blocked: {reason:?}")]
    SafetyBlocked {
        /// The reason the content was blocked.
        reason: BlockReason,
    },

    /// Prompt was rejected.
    #[error("Prompt blocked: {reason:?}")]
    PromptBlocked {
        /// The reason the prompt was blocked.
        reason: BlockReason,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_error_display() {
        let err = GenerateError::SafetyBlocked {
            reason: BlockReason::Safety,
        };
        assert!(err.to_string().contains("blocked"));
    }

    #[test]
    fn generate_content_config_from_text() {
        let config = GenerateContentConfig::from_text("Hello");
        let body = config.to_request_body();
        let contents = body.get("contents").unwrap();
        assert!(contents.is_array());
        let parts = contents[0].get("parts").unwrap();
        assert!(parts[0].get("text").unwrap().as_str().unwrap() == "Hello");
    }

    #[test]
    fn generate_content_config_with_system() {
        let config = GenerateContentConfig::from_text("Hello")
            .system_instruction("You are a helpful assistant");
        let body = config.to_request_body();
        assert!(body.get("systemInstruction").is_some());
    }

    #[test]
    fn parse_generate_response() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello world!"}],
                    "role": "model"
                },
                "finishReason": "STOP",
                "safetyRatings": [{
                    "category": "HARM_CATEGORY_HARASSMENT",
                    "probability": "NEGLIGIBLE"
                }]
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 10,
                "totalTokenCount": 15
            }
        });

        let resp: GenerateContentResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.candidates.len(), 1);
        assert_eq!(resp.text().unwrap(), "Hello world!");
        assert!(resp.usage_metadata.is_some());
    }
}
