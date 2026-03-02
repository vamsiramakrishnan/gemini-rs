//! Authentication providers for Gemini API connections.
//!
//! This module defines the [`AuthProvider`] trait and built-in implementations
//! for Google AI (API key and OAuth2 token) and Vertex AI (Bearer token).

use async_trait::async_trait;

use crate::protocol::types::GeminiModel;
use crate::session::AuthError;

/// Provides authentication credentials and URL construction for Gemini API connections.
#[async_trait]
pub trait AuthProvider: Send + Sync + 'static {
    /// Build the WebSocket URL for the given model.
    fn ws_url(&self, model: &GeminiModel) -> String;

    /// HTTP headers for the WebSocket upgrade request (e.g., Bearer token).
    async fn auth_headers(&self) -> Result<Vec<(String, String)>, AuthError>;

    /// Query parameters to append to the URL (e.g., API key).
    fn query_params(&self) -> Vec<(String, String)> {
        vec![]
    }

    /// Called on auth failure to allow token refresh. Default: no-op.
    async fn refresh(&self) -> Result<(), AuthError> {
        Ok(())
    }
}

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

    async fn auth_headers(&self) -> Result<Vec<(String, String)>, AuthError> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Vertex AI — Bearer token authentication
// ---------------------------------------------------------------------------

/// Vertex AI Bearer token authentication.
///
/// Uses a project/location pair to construct the Vertex AI WebSocket URL,
/// and a Bearer token for the `Authorization` header.
pub struct VertexAIAuth {
    project: String,
    location: String,
    token: parking_lot::Mutex<String>,
}

impl VertexAIAuth {
    /// Create a new Vertex AI auth provider.
    pub fn new(
        project: impl Into<String>,
        location: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            location: location.into(),
            token: parking_lot::Mutex::new(token.into()),
        }
    }
}

#[async_trait]
impl AuthProvider for VertexAIAuth {
    fn ws_url(&self, model: &GeminiModel) -> String {
        let host = if self.location == "global" {
            "aiplatform.googleapis.com".to_string()
        } else {
            format!("{}-aiplatform.googleapis.com", self.location)
        };
        let model_id = model
            .to_string()
            .trim_start_matches("models/")
            .to_string();
        format!(
            "wss://{host}/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent\
             ?alt=json\
             &x-goog-project-id={project}\
             &model={model_id}",
            host = host,
            project = self.project,
            model_id = model_id,
        )
    }

    async fn auth_headers(&self) -> Result<Vec<(String, String)>, AuthError> {
        let token = self.token.lock().clone();
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {token}"),
        )])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::GeminiModel;

    #[test]
    fn google_ai_auth_url() {
        let auth = GoogleAIAuth::new("test-key-123");
        let url = auth.ws_url(&GeminiModel::default());
        assert!(url.contains("generativelanguage.googleapis.com"));
        assert!(url.contains("v1beta"));
        assert!(url.contains("key=test-key-123"));
    }

    #[test]
    fn google_ai_auth_query_params() {
        let auth = GoogleAIAuth::new("my-api-key");
        let params = auth.query_params();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].0, "key");
        assert_eq!(params[0].1, "my-api-key");
    }

    #[tokio::test]
    async fn google_ai_auth_headers_empty() {
        let auth = GoogleAIAuth::new("test-key");
        let headers = auth.auth_headers().await.unwrap();
        assert!(headers.is_empty());
    }

    #[test]
    fn google_ai_token_auth_url() {
        let auth = GoogleAITokenAuth::new("oauth2-token-abc");
        let url = auth.ws_url(&GeminiModel::default());
        assert!(url.contains("generativelanguage.googleapis.com"));
        assert!(url.contains("access_token=oauth2-token-abc"));
        assert!(url.contains("v1alpha"));
    }

    #[test]
    fn vertex_ai_auth_url_regional() {
        let auth = VertexAIAuth::new("my-project", "us-central1", "token");
        let url = auth.ws_url(&GeminiModel::default());
        assert!(url.contains("us-central1-aiplatform.googleapis.com"));
        assert!(url.contains("v1beta1"));
        assert!(url.contains("x-goog-project-id=my-project"));
    }

    #[test]
    fn vertex_ai_auth_url_global() {
        let auth = VertexAIAuth::new("my-project", "global", "token");
        let url = auth.ws_url(&GeminiModel::default());
        // Global uses aiplatform.googleapis.com without location prefix.
        assert!(url.starts_with("wss://aiplatform.googleapis.com/"));
        assert!(!url.contains("global-aiplatform"));
    }

    #[tokio::test]
    async fn vertex_ai_auth_headers() {
        let auth = VertexAIAuth::new("proj", "us-central1", "my-bearer-token");
        let headers = auth.auth_headers().await.unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Authorization");
        assert_eq!(headers[0].1, "Bearer my-bearer-token");
    }

    #[test]
    fn vertex_ai_auth_url_contains_model() {
        let auth = VertexAIAuth::new("proj", "us-central1", "tok");
        let url = auth.ws_url(&GeminiModel::Gemini2_0FlashLive);
        assert!(url.contains("model=gemini-2.0-flash-live-001"));
    }

    #[test]
    fn auth_provider_is_object_safe() {
        fn _assert(_: &dyn AuthProvider) {}
    }

    #[tokio::test]
    async fn default_refresh_is_noop() {
        let auth = GoogleAIAuth::new("key");
        // Should succeed without error.
        auth.refresh().await.unwrap();
    }

    #[tokio::test]
    async fn default_query_params_empty_for_vertex() {
        let auth = VertexAIAuth::new("proj", "loc", "tok");
        let params = auth.query_params();
        assert!(params.is_empty());
    }
}
