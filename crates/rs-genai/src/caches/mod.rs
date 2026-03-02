//! Caches API — create, list, get, update, delete cached content.
//!
//! Feature-gated behind `caches`.

use serde::{Deserialize, Serialize};

use crate::client::http::HttpError;
use crate::client::Client;
use crate::protocol::types::Content;
use crate::transport::auth::ServiceEndpoint;

/// Cached content resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedContent {
    /// Resource name (e.g., `cachedContents/abc123`).
    #[serde(default)]
    pub name: String,
    /// Display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The model used for this cached content.
    #[serde(default)]
    pub model: Option<String>,
    /// System instruction.
    #[serde(default)]
    pub system_instruction: Option<Content>,
    /// Cached content items.
    #[serde(default)]
    pub contents: Option<Vec<Content>>,
    /// Expiration time (RFC3339).
    #[serde(default)]
    pub expire_time: Option<String>,
    /// TTL duration string (e.g., "3600s").
    #[serde(default)]
    pub ttl: Option<String>,
    /// Usage metadata.
    #[serde(default)]
    pub usage_metadata: Option<CachedContentUsageMetadata>,
    /// Creation time (RFC3339).
    #[serde(default)]
    pub create_time: Option<String>,
    /// Update time (RFC3339).
    #[serde(default)]
    pub update_time: Option<String>,
}

/// Usage metadata for cached content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedContentUsageMetadata {
    /// Total token count of the cached content.
    #[serde(default)]
    pub total_token_count: u64,
}

/// Configuration for creating cached content.
#[derive(Debug, Clone)]
pub struct CreateCachedContentConfig {
    /// Model to use for caching.
    pub model: String,
    /// Display name.
    pub display_name: Option<String>,
    /// System instruction to cache.
    pub system_instruction: Option<Content>,
    /// Content to cache.
    pub contents: Vec<Content>,
    /// TTL duration string (e.g., "3600s").
    pub ttl: Option<String>,
}

/// Updates to apply to a cached content resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCachedContentRequest {
    /// New TTL duration string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    /// New expiration time (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_time: Option<String>,
}

/// Response from listCachedContents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListCachedContentsResponse {
    /// List of cached contents.
    #[serde(default)]
    pub cached_contents: Vec<CachedContent>,
    /// Pagination token for the next page.
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// Errors from the Caches API.
#[derive(Debug, thiserror::Error)]
pub enum CachesError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Auth error: {0}")]
    Auth(String),
}

impl Client {
    /// List cached contents.
    pub async fn list_cached_contents(&self) -> Result<ListCachedContentsResponse, CachesError> {
        let url = self.rest_url(ServiceEndpoint::CachedContents);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| CachesError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;
        if json.is_null() {
            return Ok(ListCachedContentsResponse {
                cached_contents: vec![],
                next_page_token: None,
            });
        }
        Ok(serde_json::from_value(json)?)
    }

    /// Create a new cached content.
    pub async fn create_cached_content(
        &self,
        config: CreateCachedContentConfig,
    ) -> Result<CachedContent, CachesError> {
        let url = self.rest_url(ServiceEndpoint::CachedContents);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| CachesError::Auth(e.to_string()))?;

        let mut body = serde_json::json!({
            "model": config.model,
            "contents": config.contents,
        });

        if let Some(name) = config.display_name {
            body["displayName"] = serde_json::Value::String(name);
        }
        if let Some(instruction) = &config.system_instruction {
            body["systemInstruction"] = serde_json::to_value(instruction).unwrap();
        }
        if let Some(ttl) = config.ttl {
            body["ttl"] = serde_json::Value::String(ttl);
        }

        let json = self.http_client().post_json(&url, headers, &body).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Get a cached content by name.
    pub async fn get_cached_content(&self, name: &str) -> Result<CachedContent, CachesError> {
        let base_url = self.rest_url(ServiceEndpoint::CachedContents);
        let url = format!("{base_url}/{name}");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| CachesError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Update a cached content (TTL or expiration time).
    pub async fn update_cached_content(
        &self,
        name: &str,
        updates: UpdateCachedContentRequest,
    ) -> Result<CachedContent, CachesError> {
        let base_url = self.rest_url(ServiceEndpoint::CachedContents);
        let url = format!("{base_url}/{name}");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| CachesError::Auth(e.to_string()))?;
        let json = self.http_client().patch_json(&url, headers, &updates).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Delete a cached content by name.
    pub async fn delete_cached_content(&self, name: &str) -> Result<(), CachesError> {
        let base_url = self.rest_url(ServiceEndpoint::CachedContents);
        let url = format!("{base_url}/{name}");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| CachesError::Auth(e.to_string()))?;
        self.http_client().delete(&url, headers).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cached_content() {
        let json = serde_json::json!({
            "name": "cachedContents/abc123",
            "displayName": "my-cache",
            "model": "models/gemini-1.5-flash",
            "expireTime": "2026-03-02T12:00:00Z",
            "usageMetadata": {
                "totalTokenCount": 5000
            },
            "createTime": "2026-03-01T12:00:00Z",
            "updateTime": "2026-03-01T12:00:00Z"
        });
        let cached: CachedContent = serde_json::from_value(json).unwrap();
        assert_eq!(cached.name, "cachedContents/abc123");
        assert_eq!(cached.display_name, Some("my-cache".to_string()));
        assert_eq!(cached.model, Some("models/gemini-1.5-flash".to_string()));
        assert_eq!(cached.usage_metadata.unwrap().total_token_count, 5000);
    }

    #[test]
    fn parse_list_cached_contents_response() {
        let json = serde_json::json!({
            "cachedContents": [
                {"name": "cachedContents/a", "model": "models/gemini-1.5-flash"},
                {"name": "cachedContents/b", "model": "models/gemini-1.5-pro"}
            ],
            "nextPageToken": "token123"
        });
        let resp: ListCachedContentsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.cached_contents.len(), 2);
        assert_eq!(resp.next_page_token, Some("token123".to_string()));
    }

    #[test]
    fn update_request_serialization() {
        let update = UpdateCachedContentRequest {
            ttl: Some("7200s".to_string()),
            expire_time: None,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["ttl"], "7200s");
        assert!(json.get("expireTime").is_none());
    }

    #[test]
    fn usage_metadata_serialization() {
        let meta = CachedContentUsageMetadata {
            total_token_count: 12345,
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["totalTokenCount"], 12345);
    }

    #[test]
    fn empty_list_response() {
        let json = serde_json::json!({"cachedContents": []});
        let resp: ListCachedContentsResponse = serde_json::from_value(json).unwrap();
        assert!(resp.cached_contents.is_empty());
        assert!(resp.next_page_token.is_none());
    }
}
