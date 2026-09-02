//! Google AI authentication providers (API key and OAuth2 token).

use async_trait::async_trait;

use crate::protocol::types::ModelId;
use crate::session::AuthError;

use super::url_builders::build_google_ai_rest_url;
use super::{AuthProvider, RestAuth, ServiceEndpoint};

// ---------------------------------------------------------------------------
// Google AI — API key authentication
// ---------------------------------------------------------------------------

/// Google AI API key authentication.
///
/// The API key is included as a query parameter in the WebSocket URL.
pub struct GoogleAIAuth {
    api_key: String,
}

impl GoogleAIAuth {
    /// Create a new Google AI auth provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
        }
    }
}

#[async_trait]
impl AuthProvider for GoogleAIAuth {
    fn ws_url(&self, _model: &ModelId) -> String {
        format!(
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={}",
            self.api_key
        )
    }

    /// REST requests carry the key in the `x-goog-api-key` header rather
    /// than the query string, so it never lands in access logs, proxies, or
    /// error messages that echo the URL. (The Live WebSocket upgrade still
    /// passes it as `?key=`, which is what that endpoint accepts.)
    async fn auth_headers(&self) -> Result<Vec<(String, String)>, AuthError> {
        Ok(vec![("x-goog-api-key".to_string(), self.api_key.clone())])
    }

    fn query_params(&self) -> Vec<(String, String)> {
        vec![("key".to_string(), self.api_key.clone())]
    }
}

impl RestAuth for GoogleAIAuth {
    fn rest_url(&self, endpoint: ServiceEndpoint, model: Option<&ModelId>) -> String {
        let base = "https://generativelanguage.googleapis.com/v1beta";
        build_google_ai_rest_url(base, endpoint, model)
    }
}

// ---------------------------------------------------------------------------
// Google AI — OAuth2 access token authentication
// ---------------------------------------------------------------------------

/// Google AI OAuth2 access token authentication.
///
/// The access token is included directly in the WebSocket URL.
pub struct GoogleAITokenAuth {
    access_token: String,
}

impl GoogleAITokenAuth {
    /// Create a new Google AI token auth provider with the given access token.
    pub fn new(access_token: impl Into<String>) -> Self {
        Self {
            access_token: access_token.into(),
        }
    }
}

#[async_trait]
impl AuthProvider for GoogleAITokenAuth {
    fn ws_url(&self, _model: &ModelId) -> String {
        format!(
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1alpha.GenerativeService.BidiGenerateContentConstrained?access_token={}",
            self.access_token
        )
    }

    async fn auth_headers(&self) -> Result<Vec<(String, String)>, AuthError> {
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.access_token),
        )])
    }
}

impl RestAuth for GoogleAITokenAuth {
    fn rest_url(&self, endpoint: ServiceEndpoint, model: Option<&ModelId>) -> String {
        let base = "https://generativelanguage.googleapis.com/v1beta";
        // Token auth uses Bearer header, not query param — build URL without key
        build_google_ai_rest_url(base, endpoint, model)
    }
}
