//! Authentication providers for Gemini API connections.
//!
//! This module defines the [`AuthProvider`] trait and built-in implementations
//! for Google AI (API key and OAuth2 token) and Vertex AI (Bearer token).
//!
//! The [`ServiceEndpoint`] enum allows constructing URLs for both WebSocket (Live)
//! and REST API endpoints from the same auth provider.

pub mod google_ai;
pub mod vertex;
pub(crate) mod url_builders;

pub use google_ai::*;
pub use vertex::*;

use async_trait::async_trait;

use crate::protocol::types::GeminiModel;
use crate::session::AuthError;

/// Identifies which Gemini API service to connect to.
///
/// Used by [`AuthProvider::rest_url`] to construct the correct REST endpoint URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceEndpoint {
    /// WebSocket Live/Bidi streaming endpoint.
    LiveWs,
    /// POST /models/{model}:generateContent
    GenerateContent,
    /// POST /models/{model}:streamGenerateContent
    StreamGenerateContent,
    /// POST /models/{model}:embedContent
    EmbedContent,
    /// POST /models/{model}:countTokens
    CountTokens,
    /// POST /models/{model}:computeTokens
    ComputeTokens,
    /// GET /models
    ListModels,
    /// GET /models/{model}
    GetModel,
    /// Files CRUD (upload, get, list, delete)
    Files,
    /// Cached content CRUD
    CachedContents,
    /// Tuning jobs CRUD
    TuningJobs,
    /// Batch jobs CRUD
    BatchJobs,
}

impl ServiceEndpoint {
    /// REST method suffix appended to the model path (e.g., `:generateContent`).
    /// Returns `None` for endpoints that don't use a model suffix.
    pub fn model_method(&self) -> Option<&'static str> {
        match self {
            Self::GenerateContent => Some("generateContent"),
            Self::StreamGenerateContent => Some("streamGenerateContent"),
            Self::EmbedContent => Some("embedContent"),
            Self::CountTokens => Some("countTokens"),
            Self::ComputeTokens => Some("computeTokens"),
            _ => None,
        }
    }

    /// Whether this endpoint requires a model ID in the path.
    pub fn requires_model(&self) -> bool {
        matches!(
            self,
            Self::GenerateContent
                | Self::StreamGenerateContent
                | Self::EmbedContent
                | Self::CountTokens
                | Self::ComputeTokens
                | Self::GetModel
        )
    }
}

/// Provides authentication credentials and URL construction for Gemini API connections.
#[async_trait]
pub trait AuthProvider: Send + Sync + 'static {
    /// Build the WebSocket URL for the given model.
    fn ws_url(&self, model: &GeminiModel) -> String;

    /// Build a REST API URL for the given service endpoint and model.
    ///
    /// Default implementation panics — override when using HTTP client features.
    fn rest_url(&self, endpoint: ServiceEndpoint, model: Option<&GeminiModel>) -> String {
        let _ = (endpoint, model);
        unimplemented!("REST URLs require a concrete auth provider (GoogleAIAuth or VertexAIAuth)")
    }

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

    // -----------------------------------------------------------------------
    // REST URL tests
    // -----------------------------------------------------------------------

    #[test]
    fn google_ai_rest_url_generate_content() {
        let auth = GoogleAIAuth::new("test-key");
        let model = GeminiModel::Gemini2_0FlashLive;
        let url = auth.rest_url(ServiceEndpoint::GenerateContent, Some(&model));
        assert!(url.starts_with("https://generativelanguage.googleapis.com/v1beta/"));
        assert!(url.contains(":generateContent"));
        assert!(url.contains("key=test-key"));
    }

    #[test]
    fn google_ai_rest_url_list_models() {
        let auth = GoogleAIAuth::new("key123");
        let url = auth.rest_url(ServiceEndpoint::ListModels, None);
        assert!(url.contains("/models?key=key123"));
    }

    #[test]
    fn google_ai_rest_url_files() {
        let auth = GoogleAIAuth::new("key");
        let url = auth.rest_url(ServiceEndpoint::Files, None);
        assert!(url.contains("/files?key=key"));
    }

    #[test]
    fn google_ai_token_rest_url_no_key_in_url() {
        let auth = GoogleAITokenAuth::new("oauth-token");
        let url = auth.rest_url(ServiceEndpoint::CountTokens, Some(&GeminiModel::default()));
        assert!(url.contains(":countTokens"));
        assert!(!url.contains("key="));
        assert!(!url.contains("access_token="));
    }

    #[test]
    fn vertex_rest_url_generate_content() {
        let auth = VertexAIAuth::new("my-project", "us-central1", "token");
        let model = GeminiModel::Gemini2_0FlashLive;
        let url = auth.rest_url(ServiceEndpoint::GenerateContent, Some(&model));
        assert!(url.starts_with("https://us-central1-aiplatform.googleapis.com/v1beta1/"));
        assert!(url.contains("projects/my-project/locations/us-central1"));
        assert!(url.contains(":generateContent"));
    }

    #[test]
    fn vertex_rest_url_list_models() {
        let auth = VertexAIAuth::new("proj", "us-east1", "tok");
        let url = auth.rest_url(ServiceEndpoint::ListModels, None);
        assert!(url.contains("publishers/google/models"));
    }

    #[test]
    fn vertex_rest_url_global() {
        let auth = VertexAIAuth::new("proj", "global", "tok");
        let model = GeminiModel::default();
        let url = auth.rest_url(ServiceEndpoint::EmbedContent, Some(&model));
        assert!(url.starts_with("https://aiplatform.googleapis.com/"));
        assert!(!url.contains("global-aiplatform"));
        assert!(url.contains(":embedContent"));
    }

    #[test]
    fn service_endpoint_model_method() {
        assert_eq!(
            ServiceEndpoint::GenerateContent.model_method(),
            Some("generateContent")
        );
        assert_eq!(
            ServiceEndpoint::StreamGenerateContent.model_method(),
            Some("streamGenerateContent")
        );
        assert_eq!(ServiceEndpoint::ListModels.model_method(), None);
        assert_eq!(ServiceEndpoint::Files.model_method(), None);
    }

    #[tokio::test]
    async fn vertex_ai_refreshable_token() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let counter = std::sync::Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let auth = VertexAIAuth::with_token_refresher("proj", "us-central1", move || {
            c.fetch_add(1, Ordering::SeqCst);
            format!("token-{}", c.load(Ordering::SeqCst))
        });
        let h1 = auth.auth_headers().await.unwrap();
        assert!(h1[0].1.starts_with("Bearer token-"));
        let h2 = auth.auth_headers().await.unwrap();
        assert!(h2[0].1.starts_with("Bearer token-"));
        // Refresher called twice (once per auth_headers)
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn service_endpoint_requires_model() {
        assert!(ServiceEndpoint::GenerateContent.requires_model());
        assert!(ServiceEndpoint::CountTokens.requires_model());
        assert!(!ServiceEndpoint::ListModels.requires_model());
        assert!(!ServiceEndpoint::Files.requires_model());
    }
}
