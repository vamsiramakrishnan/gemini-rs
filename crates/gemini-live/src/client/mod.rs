//! Unified Gemini API client — wraps both Live (WebSocket) and REST API access.
//!
//! The [`Client`] struct provides a single entry point for all Gemini APIs.
//! REST API modules are feature-gated behind their respective features
//! (e.g., `generate`, `embed`, `models`) so that live-only users pay zero cost.

#[cfg(feature = "http")]
pub mod http;

use std::sync::Arc;

use crate::protocol::types::{ApiEndpoint, GeminiModel, SessionConfig};
use crate::session::SessionError;
use crate::session::SessionHandle;
use crate::transport::auth::{
    AuthProvider, GoogleAIAuth, GoogleAITokenAuth, ServiceEndpoint, VertexAIAuth,
};
use crate::transport::{connect, TransportConfig};

/// Unified Gemini API client.
///
/// Mirrors the `GoogleGenAI` class from `@google/genai` (js-genai).
/// Provides access to both Live (WebSocket) and REST APIs through a single
/// authenticated entry point.
///
/// # Construction
///
/// ```ignore
/// // From API key (Google AI)
/// let client = Client::from_api_key("your-api-key");
///
/// // From Vertex AI credentials
/// let client = Client::from_vertex("project-id", "us-central1", "access-token");
///
/// // Live WebSocket session
/// let session = client.live("gemini-2.5-flash").connect().await?;
/// ```
pub struct Client {
    endpoint: ApiEndpoint,
    model: GeminiModel,
    auth: Arc<dyn AuthProvider>,
    #[cfg(feature = "http")]
    http: http::HttpClient,
}

impl Client {
    /// Create a client with Google AI API key authentication.
    pub fn from_api_key(api_key: impl Into<String>) -> Self {
        let key: String = api_key.into();
        let endpoint = ApiEndpoint::google_ai(key.clone());
        let auth: Arc<dyn AuthProvider> = Arc::new(GoogleAIAuth::new(key));
        Self {
            endpoint,
            model: GeminiModel::default(),
            auth,
            #[cfg(feature = "http")]
            http: http::HttpClient::new(http::HttpConfig::default()),
        }
    }

    /// Create a client with Google AI OAuth2 token authentication.
    pub fn from_access_token(access_token: impl Into<String>) -> Self {
        let token: String = access_token.into();
        let endpoint = ApiEndpoint::google_ai_token(token.clone());
        let auth: Arc<dyn AuthProvider> = Arc::new(GoogleAITokenAuth::new(token));
        Self {
            endpoint,
            model: GeminiModel::default(),
            auth,
            #[cfg(feature = "http")]
            http: http::HttpClient::new(http::HttpConfig::default()),
        }
    }

    /// Create a client with Vertex AI authentication.
    pub fn from_vertex(
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        let proj: String = project.into();
        let loc: String = location.into();
        let tok: String = access_token.into();
        let endpoint = ApiEndpoint::vertex(proj.clone(), loc.clone(), tok.clone());
        let auth: Arc<dyn AuthProvider> = Arc::new(VertexAIAuth::new(proj, loc, tok));
        Self {
            endpoint,
            model: GeminiModel::default(),
            auth,
            #[cfg(feature = "http")]
            http: http::HttpClient::new(http::HttpConfig::default()),
        }
    }

    /// Create a client with Vertex AI authentication and dynamic token refresh.
    ///
    /// The `refresher` closure is called on every REST API request to obtain
    /// a fresh Bearer token. It should handle caching internally to avoid
    /// unnecessary overhead (see `GcloudTokenProvider` in gemini-adk for an example).
    ///
    /// This is the recommended constructor for long-running HTTP clients
    /// (e.g., extraction LLMs) where tokens may expire during the session.
    pub fn from_vertex_refreshable(
        project: impl Into<String>,
        location: impl Into<String>,
        refresher: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        let proj: String = project.into();
        let loc: String = location.into();
        // Get initial token for the ApiEndpoint (used if .live() is called)
        let initial_token = refresher();
        let endpoint = ApiEndpoint::vertex(proj.clone(), loc.clone(), initial_token);
        let auth: Arc<dyn AuthProvider> =
            Arc::new(VertexAIAuth::with_token_refresher(proj, loc, refresher));
        Self {
            endpoint,
            model: GeminiModel::default(),
            auth,
            #[cfg(feature = "http")]
            http: http::HttpClient::new(http::HttpConfig::default()),
        }
    }

    /// Set the default model for all API calls.
    pub fn model(mut self, model: impl Into<GeminiModel>) -> Self {
        self.model = model.into();
        self
    }

    /// Configure the HTTP client (timeouts, retries, etc.).
    #[cfg(feature = "http")]
    pub fn http_config(mut self, config: http::HttpConfig) -> Self {
        self.http = http::HttpClient::new(config);
        self
    }

    /// Get a reference to the underlying auth provider.
    pub fn auth(&self) -> &dyn AuthProvider {
        &*self.auth
    }

    /// Get the default model.
    pub fn default_model(&self) -> &GeminiModel {
        &self.model
    }

    /// Build the REST URL for a given service endpoint, using the default model.
    pub fn rest_url(&self, endpoint: ServiceEndpoint) -> String {
        self.auth.rest_url(endpoint, Some(&self.model))
    }

    /// Build the REST URL for a given service endpoint with a specific model.
    pub fn rest_url_for(&self, endpoint: ServiceEndpoint, model: &GeminiModel) -> String {
        self.auth.rest_url(endpoint, Some(model))
    }

    /// Get auth headers for REST API calls.
    pub async fn auth_headers(&self) -> Result<Vec<(String, String)>, crate::session::AuthError> {
        self.auth.auth_headers().await
    }

    /// Start a Live WebSocket session builder.
    ///
    /// Returns a [`LiveSessionBuilder`] that can be customized before connecting.
    pub fn live(&self, model: GeminiModel) -> LiveSessionBuilder {
        LiveSessionBuilder {
            endpoint: self.endpoint.clone(),
            model,
            transport_config: TransportConfig::default(),
            config_fn: None,
        }
    }

    /// Get a reference to the HTTP client for making REST API calls.
    #[cfg(feature = "http")]
    pub fn http_client(&self) -> &http::HttpClient {
        &self.http
    }

    /// Make a raw REST API request (low-level).
    ///
    /// Higher-level module methods (e.g., `generate_content()`) should be preferred.
    #[cfg(feature = "http")]
    pub async fn rest_request(
        &self,
        endpoint: ServiceEndpoint,
        body: &impl serde::Serialize,
    ) -> Result<serde_json::Value, http::HttpError> {
        let url = self.rest_url(endpoint);
        let headers = self
            .auth
            .auth_headers()
            .await
            .map_err(|e| http::HttpError::Auth(e.to_string()))?;
        self.http.post_json(&url, headers, body).await
    }
}

/// Builder for Live WebSocket sessions initiated from a [`Client`].
pub struct LiveSessionBuilder {
    endpoint: ApiEndpoint,
    model: GeminiModel,
    transport_config: TransportConfig,
    config_fn: Option<Box<dyn FnOnce(SessionConfig) -> SessionConfig>>,
}

impl LiveSessionBuilder {
    /// Set transport configuration (timeouts, reconnection, etc.).
    pub fn transport_config(mut self, config: TransportConfig) -> Self {
        self.transport_config = config;
        self
    }

    /// Apply a customization function to the session config before connecting.
    pub fn configure(mut self, f: impl FnOnce(SessionConfig) -> SessionConfig + 'static) -> Self {
        self.config_fn = Some(Box::new(f));
        self
    }

    /// Connect and return a [`SessionHandle`].
    pub async fn connect(self) -> Result<SessionHandle, SessionError> {
        let mut config = SessionConfig::from_endpoint(self.endpoint).model(self.model);

        if let Some(f) = self.config_fn {
            config = f(config);
        }

        connect(config, self.transport_config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_from_api_key() {
        let client = Client::from_api_key("test-key");
        assert!(matches!(
            client.default_model(),
            GeminiModel::GeminiLive2_5FlashNativeAudio
        ));
    }

    #[test]
    fn client_from_vertex() {
        let client = Client::from_vertex("proj", "us-central1", "tok");
        let url = client.auth().ws_url(&GeminiModel::default());
        assert!(url.contains("us-central1-aiplatform.googleapis.com"));
    }

    #[test]
    fn client_model_override() {
        let client = Client::from_api_key("key").model(GeminiModel::Gemini2_0FlashLive);
        assert!(matches!(
            client.default_model(),
            GeminiModel::Gemini2_0FlashLive
        ));
    }

    #[test]
    fn client_rest_url_generate() {
        let client = Client::from_api_key("my-key").model(GeminiModel::Gemini2_0FlashLive);
        let url = client.rest_url(ServiceEndpoint::GenerateContent);
        assert!(url.contains(":generateContent"));
        assert!(url.contains("key=my-key"));
    }

    #[test]
    fn client_rest_url_vertex() {
        let client =
            Client::from_vertex("proj", "us-east1", "tok").model(GeminiModel::Gemini2_0FlashLive);
        let url = client.rest_url(ServiceEndpoint::GenerateContent);
        assert!(url.contains("us-east1-aiplatform.googleapis.com"));
        assert!(url.contains(":generateContent"));
    }

    #[test]
    fn live_session_builder_created() {
        let client = Client::from_api_key("key");
        let _builder = client.live(GeminiModel::Gemini2_0FlashLive);
    }

    #[tokio::test]
    async fn client_from_vertex_refreshable() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();
        let client = Client::from_vertex_refreshable("proj", "us-central1", move || {
            cc.fetch_add(1, Ordering::SeqCst);
            "refreshed-token".to_string()
        });
        // Initial token fetch happens at construction
        assert!(call_count.load(Ordering::SeqCst) >= 1);
        // auth_headers should call the refresher again
        let headers = client.auth_headers().await.unwrap();
        assert_eq!(headers[0].1, "Bearer refreshed-token");
        assert!(call_count.load(Ordering::SeqCst) >= 2);
    }
}
