//! Google AI authentication providers (API key and OAuth2 token).

use async_trait::async_trait;

use crate::protocol::types::GeminiModel;
use crate::session::AuthError;

use super::url_builders::{build_google_ai_rest_url, build_google_ai_rest_url_no_key};
use super::{AuthProvider, ServiceEndpoint};

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
    fn ws_url(&self, _model: &GeminiModel) -> String {
        format!(
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={}",
            self.api_key
        )
    }

    fn rest_url(&self, endpoint: ServiceEndpoint, model: Option<&GeminiModel>) -> String {
        let base = "https://generativelanguage.googleapis.com/v1beta";
        build_google_ai_rest_url(base, endpoint, model, &self.api_key)
    }

    async fn auth_headers(&self) -> Result<Vec<(String, String)>, AuthError> {
        Ok(vec![]) // API key is in the URL
    }

    fn query_params(&self) -> Vec<(String, String)> {
        vec![("key".to_string(), self.api_key.clone())]
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
    fn ws_url(&self, _model: &GeminiModel) -> String {
        format!(
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1alpha.GenerativeService.BidiGenerateContentConstrained?access_token={}",
            self.access_token
        )
    }

    fn rest_url(&self, endpoint: ServiceEndpoint, model: Option<&GeminiModel>) -> String {
        let base = "https://generativelanguage.googleapis.com/v1beta";
        // Token auth uses Bearer header, not query param — build URL without key
        build_google_ai_rest_url_no_key(base, endpoint, model)
    }

    async fn auth_headers(&self) -> Result<Vec<(String, String)>, AuthError> {
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.access_token),
        )])
    }
}
