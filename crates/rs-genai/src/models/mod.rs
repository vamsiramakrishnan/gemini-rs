//! Models API — list and get model metadata.
//!
//! Feature-gated behind `models`.

use serde::{Deserialize, Serialize};

use crate::client::http::HttpError;
use crate::client::Client;
use crate::protocol::types::GeminiModel;
use crate::transport::auth::ServiceEndpoint;

/// Model metadata returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// Full model resource name (e.g., "models/gemini-2.5-flash").
    pub name: String,

    /// Human-readable display name.
    #[serde(default)]
    pub display_name: Option<String>,

    /// Model description.
    #[serde(default)]
    pub description: Option<String>,

    /// Model version.
    #[serde(default)]
    pub version: Option<String>,

    /// Supported generation methods.
    #[serde(default)]
    pub supported_generation_methods: Vec<String>,

    /// Maximum input tokens.
    #[serde(default)]
    pub input_token_limit: Option<u32>,

    /// Maximum output tokens.
    #[serde(default)]
    pub output_token_limit: Option<u32>,

    /// Default temperature.
    #[serde(default)]
    pub temperature: Option<f32>,

    /// Default top_p.
    #[serde(default)]
    pub top_p: Option<f32>,

    /// Default top_k.
    #[serde(default)]
    pub top_k: Option<u32>,
}

/// Response from list models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListModelsResponse {
    /// Available models.
    #[serde(default)]
    pub models: Vec<ModelInfo>,

    /// Pagination token for next page.
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// Errors from the Models API.
#[derive(Debug, thiserror::Error)]
pub enum ModelsError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Auth error: {0}")]
    Auth(String),
}

impl Client {
    /// List available models.
    pub async fn list_models(&self) -> Result<ListModelsResponse, ModelsError> {
        let url = self.rest_url_for(ServiceEndpoint::ListModels, self.default_model());
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| ModelsError::Auth(e.to_string()))?;

        let json = self.http_client().get_json(&url, headers).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Get metadata for a specific model.
    pub async fn get_model(&self, model: &GeminiModel) -> Result<ModelInfo, ModelsError> {
        let url = self.rest_url_for(ServiceEndpoint::GetModel, model);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| ModelsError::Auth(e.to_string()))?;

        let json = self.http_client().get_json(&url, headers).await?;
        Ok(serde_json::from_value(json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_info() {
        let json = serde_json::json!({
            "name": "models/gemini-2.5-flash",
            "displayName": "Gemini 2.5 Flash",
            "description": "Fast model",
            "supportedGenerationMethods": ["generateContent", "countTokens"],
            "inputTokenLimit": 1048576,
            "outputTokenLimit": 8192,
            "temperature": 1.0,
            "topP": 0.95,
            "topK": 40
        });
        let model: ModelInfo = serde_json::from_value(json).unwrap();
        assert_eq!(model.name, "models/gemini-2.5-flash");
        assert_eq!(model.input_token_limit, Some(1048576));
        assert_eq!(model.supported_generation_methods.len(), 2);
    }

    #[test]
    fn parse_list_models_response() {
        let json = serde_json::json!({
            "models": [
                {"name": "models/gemini-2.5-flash"},
                {"name": "models/gemini-2.5-pro"}
            ],
            "nextPageToken": "abc123"
        });
        let resp: ListModelsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.models.len(), 2);
        assert_eq!(resp.next_page_token, Some("abc123".to_string()));
    }
}
