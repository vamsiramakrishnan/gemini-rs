//! Embedding API — embedContent.
//!
//! Feature-gated behind `embed`.

use serde::{Deserialize, Serialize};

use crate::client::http::HttpError;
use crate::client::Client;
use crate::protocol::types::{Content, GeminiModel};
use crate::transport::auth::ServiceEndpoint;

/// Configuration for embed requests.
#[derive(Debug, Clone)]
pub struct EmbedContentConfig {
    /// Content to embed.
    pub content: Content,
    /// Optional task type for better embeddings.
    pub task_type: Option<TaskType>,
    /// Optional title (for RETRIEVAL_DOCUMENT task type).
    pub title: Option<String>,
    /// Optional output dimensionality.
    pub output_dimensionality: Option<u32>,
}

impl EmbedContentConfig {
    /// Create an embed config from text.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            content: Content::user(text),
            task_type: None,
            title: None,
            output_dimensionality: None,
        }
    }

    /// Set the task type.
    pub fn task_type(mut self, task_type: TaskType) -> Self {
        self.task_type = Some(task_type);
        self
    }

    /// Set the title (for document retrieval).
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the output dimensionality.
    pub fn output_dimensionality(mut self, dim: u32) -> Self {
        self.output_dimensionality = Some(dim);
        self
    }
}

/// Task type for embedding optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskType {
    RetrievalQuery,
    RetrievalDocument,
    SemanticSimilarity,
    Classification,
    Clustering,
    QuestionAnswering,
    FactVerification,
}

/// Response from embedContent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedContentResponse {
    /// The embedding values.
    pub embedding: ContentEmbedding,
}

/// Embedding vector.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentEmbedding {
    /// The embedding values (float vector).
    pub values: Vec<f32>,
}

/// Errors from the Embed API.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Auth error: {0}")]
    Auth(String),
}

impl Client {
    /// Embed text content using the default model.
    pub async fn embed_content(
        &self,
        text: impl Into<String>,
    ) -> Result<EmbedContentResponse, EmbedError> {
        self.embed_content_with(EmbedContentConfig::from_text(text), None)
            .await
    }

    /// Embed content with full configuration.
    pub async fn embed_content_with(
        &self,
        config: EmbedContentConfig,
        model: Option<&GeminiModel>,
    ) -> Result<EmbedContentResponse, EmbedError> {
        let model = model.unwrap_or(self.default_model());
        let url = self.rest_url_for(ServiceEndpoint::EmbedContent, model);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| EmbedError::Auth(e.to_string()))?;

        let mut body = serde_json::json!({
            "content": config.content,
        });

        if let Some(task_type) = config.task_type {
            body["taskType"] = serde_json::to_value(task_type).unwrap();
        }
        if let Some(title) = config.title {
            body["title"] = serde_json::Value::String(title);
        }
        if let Some(dim) = config.output_dimensionality {
            body["outputDimensionality"] = serde_json::json!(dim);
        }

        let json = self.http_client().post_json(&url, headers, &body).await?;
        Ok(serde_json::from_value(json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_embed_response() {
        let json = serde_json::json!({
            "embedding": {
                "values": [0.1, 0.2, 0.3, 0.4]
            }
        });
        let resp: EmbedContentResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.embedding.values.len(), 4);
        assert!((resp.embedding.values[0] - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn embed_config_builder() {
        let config = EmbedContentConfig::from_text("Hello")
            .task_type(TaskType::RetrievalQuery)
            .output_dimensionality(256);
        assert_eq!(config.task_type, Some(TaskType::RetrievalQuery));
        assert_eq!(config.output_dimensionality, Some(256));
    }
}
