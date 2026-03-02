//! Token counting API — countTokens and computeTokens.
//!
//! Feature-gated behind `tokens`.

use serde::{Deserialize, Serialize};

use crate::client::http::HttpError;
use crate::client::Client;
use crate::protocol::types::{Content, GeminiModel};
use crate::transport::auth::ServiceEndpoint;

/// Response from countTokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CountTokensResponse {
    /// Total number of tokens.
    pub total_tokens: u32,
    /// Cached content tokens (if applicable).
    #[serde(default)]
    pub cached_content_token_count: Option<u32>,
}

/// Errors from the Tokens API.
#[derive(Debug, thiserror::Error)]
pub enum TokensError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Auth error: {0}")]
    Auth(String),
}

impl Client {
    /// Count tokens for text content.
    pub async fn count_tokens(
        &self,
        text: impl Into<String>,
    ) -> Result<CountTokensResponse, TokensError> {
        self.count_tokens_for(
            vec![Content::user(text)],
            None,
        )
        .await
    }

    /// Count tokens for content with optional model override.
    pub async fn count_tokens_for(
        &self,
        contents: Vec<Content>,
        model: Option<&GeminiModel>,
    ) -> Result<CountTokensResponse, TokensError> {
        let model = model.unwrap_or(self.default_model());
        let url = self.rest_url_for(ServiceEndpoint::CountTokens, model);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| TokensError::Auth(e.to_string()))?;

        let body = serde_json::json!({ "contents": contents });
        let json = self.http_client().post_json(&url, headers, &body).await?;
        Ok(serde_json::from_value(json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_count_tokens_response() {
        let json = serde_json::json!({
            "totalTokens": 42
        });
        let resp: CountTokensResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.total_tokens, 42);
        assert!(resp.cached_content_token_count.is_none());
    }

    #[test]
    fn parse_count_tokens_with_cached() {
        let json = serde_json::json!({
            "totalTokens": 100,
            "cachedContentTokenCount": 50
        });
        let resp: CountTokensResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.total_tokens, 100);
        assert_eq!(resp.cached_content_token_count, Some(50));
    }
}
