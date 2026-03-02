//! Authentication providers for Gemini API connections.
//!
//! This module defines the [`AuthProvider`] trait and built-in implementations
//! for Google AI (API key and OAuth2 token) and Vertex AI (Bearer token).
//!
//! The [`ServiceEndpoint`] enum allows constructing URLs for both WebSocket (Live)
//! and REST API endpoints from the same auth provider.

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

    fn rest_url(&self, endpoint: ServiceEndpoint, model: Option<&GeminiModel>) -> String {
        let host = if self.location == "global" {
            "aiplatform.googleapis.com".to_string()
        } else {
            format!("{}-aiplatform.googleapis.com", self.location)
        };
        build_vertex_rest_url(&host, &self.project, &self.location, endpoint, model)
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
// REST URL builders (shared by auth implementations)
// ---------------------------------------------------------------------------

/// Build a Google AI REST URL with API key as query parameter.
fn build_google_ai_rest_url(
    base: &str,
    endpoint: ServiceEndpoint,
    model: Option<&GeminiModel>,
    api_key: &str,
) -> String {
    let path = build_rest_path(endpoint, model);
    format!("{base}/{path}?key={api_key}")
}

/// Build a Google AI REST URL without an API key (for token-based auth).
fn build_google_ai_rest_url_no_key(
    base: &str,
    endpoint: ServiceEndpoint,
    model: Option<&GeminiModel>,
) -> String {
    let path = build_rest_path(endpoint, model);
    format!("{base}/{path}")
}

/// Build a Vertex AI REST URL.
fn build_vertex_rest_url(
    host: &str,
    project: &str,
    location: &str,
    endpoint: ServiceEndpoint,
    model: Option<&GeminiModel>,
) -> String {
    let base = format!(
        "https://{host}/v1beta1/projects/{project}/locations/{location}",
    );
    match endpoint {
        ServiceEndpoint::LiveWs => {
            // LiveWs should use ws_url(), not rest_url()
            panic!("Use ws_url() for LiveWs endpoints")
        }
        ServiceEndpoint::Files => {
            // Vertex AI files are at project/location level
            format!("{base}/files")
        }
        ServiceEndpoint::CachedContents => {
            format!("{base}/cachedContents")
        }
        ServiceEndpoint::TuningJobs => {
            format!("{base}/tuningJobs")
        }
        ServiceEndpoint::BatchJobs => {
            format!("{base}/batchPredictionJobs")
        }
        ServiceEndpoint::ListModels => {
            format!("{base}/publishers/google/models")
        }
        endpoint => {
            // Model-scoped endpoints
            let model_id = model
                .map(|m| m.to_string().trim_start_matches("models/").to_string())
                .unwrap_or_default();
            let publisher_model = format!("publishers/google/models/{model_id}");
            if let Some(method) = endpoint.model_method() {
                format!("{base}/{publisher_model}:{method}")
            } else {
                format!("{base}/{publisher_model}")
            }
        }
    }
}

/// Build the REST path segment for Google AI (mldev) endpoints.
fn build_rest_path(endpoint: ServiceEndpoint, model: Option<&GeminiModel>) -> String {
    match endpoint {
        ServiceEndpoint::LiveWs => {
            panic!("Use ws_url() for LiveWs endpoints")
        }
        ServiceEndpoint::Files => "files".to_string(),
        ServiceEndpoint::CachedContents => "cachedContents".to_string(),
        ServiceEndpoint::TuningJobs => "tunedModels".to_string(),
        ServiceEndpoint::BatchJobs => "batchJobs".to_string(),
        ServiceEndpoint::ListModels => "models".to_string(),
        endpoint => {
            let model_str = model.map(|m| m.to_string()).unwrap_or_default();
            if let Some(method) = endpoint.model_method() {
                format!("{model_str}:{method}")
            } else {
                model_str
            }
        }
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

    #[test]
    fn service_endpoint_requires_model() {
        assert!(ServiceEndpoint::GenerateContent.requires_model());
        assert!(ServiceEndpoint::CountTokens.requires_model());
        assert!(!ServiceEndpoint::ListModels.requires_model());
        assert!(!ServiceEndpoint::Files.requires_model());
    }
}
