//! Unified Gemini API client — wraps both Live (WebSocket) and REST API access.
//!
//! The [`Client`] struct provides a single entry point for all Gemini APIs.
//! REST API modules are feature-gated behind their respective features
//! (e.g., `generate`, `embed`, `models`) so that live-only users pay zero cost.

#[cfg(feature = "http")]
pub mod http;

use std::sync::Arc;

use crate::protocol::types::{ApiEndpoint, ModelId, SessionConfig};
use crate::transport::ConnectBuilder;
use crate::transport::auth::{
    AuthProvider, GoogleAIAuth, GoogleAITokenAuth, RestAuth, ServiceEndpoint, VertexAIAuth,
};

/// Unified Gemini API client.
///
/// Mirrors the `GoogleGenAI` class from `@google/genai` (js-genai).
/// Provides access to both Live (WebSocket) and REST APIs through a single
/// authenticated entry point.
///
/// # Construction
///
/// ```no_run
/// use gemini_genai_rs::Client;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// // From API key (Google AI)
/// let client = Client::from_api_key("your-api-key");
///
/// // From Vertex AI credentials
/// let client = Client::from_vertex("project-id", "us-central1", "access-token");
///
/// // Live WebSocket session on the platform's default Live model
/// let session = client.live(None).connect().await?;
/// # Ok(())
/// # }
/// ```
pub struct Client {
    endpoint: ApiEndpoint,
    model: ModelId,
    auth: Arc<dyn RestAuth>,
    #[cfg(feature = "http")]
    http: http::HttpClient,
}

impl Client {
    /// Create a client with Google AI API key authentication.
    pub fn from_api_key(api_key: impl Into<String>) -> Self {
        let key: String = api_key.into();
        let endpoint = ApiEndpoint::google_ai(key.clone());
        let auth: Arc<dyn RestAuth> = Arc::new(GoogleAIAuth::new(key));
        Self {
            endpoint,
            model: ModelId::FLASH_LATEST,
            auth,
            #[cfg(feature = "http")]
            http: http::HttpClient::new(http::HttpConfig::default()),
        }
    }

    /// Create a client with Google AI OAuth2 token authentication.
    pub fn from_access_token(access_token: impl Into<String>) -> Self {
        let token: String = access_token.into();
        let endpoint = ApiEndpoint::google_ai_token(token.clone());
        let auth: Arc<dyn RestAuth> = Arc::new(GoogleAITokenAuth::new(token));
        Self {
            endpoint,
            model: ModelId::FLASH_LATEST,
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
        let auth: Arc<dyn RestAuth> = Arc::new(VertexAIAuth::new(proj, loc, tok));
        Self {
            endpoint,
            model: ModelId::FLASH_LATEST,
            auth,
            #[cfg(feature = "http")]
            http: http::HttpClient::new(http::HttpConfig::default()),
        }
    }

    /// Create a client with Vertex AI authentication and dynamic token refresh.
    ///
    /// The `refresher` closure is called on every REST API request and on
    /// every Live connection attempt (including reconnects) to obtain a
    /// fresh Bearer token. It should cache internally (see
    /// `GcloudTokenProvider` in gemini-adk-rs for an example).
    ///
    /// This is the right constructor for anything that outlives a token's
    /// ~1 h lifetime.
    pub fn from_vertex_refreshable(
        project: impl Into<String>,
        location: impl Into<String>,
        refresher: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        let proj: String = project.into();
        let loc: String = location.into();
        // One source feeds both sides: REST requests and every Live
        // (re)connection attempt see a fresh token.
        let refresher = Arc::new(refresher);
        let live_refresher = refresher.clone();
        let endpoint =
            ApiEndpoint::vertex_refreshing(proj.clone(), loc.clone(), move || live_refresher());
        let auth: Arc<dyn RestAuth> =
            Arc::new(VertexAIAuth::with_token_refresher(proj, loc, move || {
                refresher()
            }));
        Self {
            endpoint,
            model: ModelId::FLASH_LATEST,
            auth,
            #[cfg(feature = "http")]
            http: http::HttpClient::new(http::HttpConfig::default()),
        }
    }

    /// Set the default model for all API calls.
    pub fn model(mut self, model: impl Into<ModelId>) -> Self {
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
    pub fn default_model(&self) -> &ModelId {
        &self.model
    }

    /// Build the REST URL for a given service endpoint, using the default model.
    pub fn rest_url(&self, endpoint: ServiceEndpoint) -> String {
        self.auth.rest_url(endpoint, Some(&self.model))
    }

    /// Build the REST URL for a given service endpoint with a specific model.
    pub fn rest_url_for(&self, endpoint: ServiceEndpoint, model: &ModelId) -> String {
        self.auth.rest_url(endpoint, Some(model))
    }

    /// Get auth headers for REST API calls.
    pub async fn auth_headers(&self) -> Result<Vec<(String, String)>, crate::session::AuthError> {
        self.auth.auth_headers().await
    }

    /// A Live session on this client's credentials.
    ///
    /// `None` connects to the platform's default Live model (the REST default
    /// model is a text model and would not do). Tune the session with
    /// [`ConnectBuilder::configure`], then `.connect().await`.
    pub fn live(&self, model: Option<ModelId>) -> ConnectBuilder {
        let mut config = SessionConfig::from_endpoint(self.endpoint.clone());
        config.model = model;
        ConnectBuilder::new(config)
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
        let headers = self.auth.auth_headers().await?;
        self.http.post_json(&url, headers, body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_from_api_key() {
        let client = Client::from_api_key("test-key");
        // The REST client's default is a text model: `generateContent` on a
        // Live-only native-audio model 404s.
        assert_eq!(client.default_model(), &ModelId::FLASH_LATEST);
    }

    #[test]
    fn client_from_vertex() {
        let client = Client::from_vertex("proj", "us-central1", "tok");
        let url = client.auth().ws_url(&ModelId::FLASH_LATEST);
        assert!(url.contains("us-central1-aiplatform.googleapis.com"));
    }

    #[test]
    fn client_model_override() {
        let client = Client::from_api_key("key").model("models/gemini-2.0-flash-live-001");
        assert_eq!(
            client.default_model(),
            &ModelId::from_static("models/gemini-2.0-flash-live-001")
        );
    }

    #[test]
    fn client_rest_url_generate() {
        let client = Client::from_api_key("my-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        let url = client.rest_url(ServiceEndpoint::GenerateContent);
        assert!(url.contains(":generateContent"));
        assert!(!url.contains("my-key"), "the key rides in a header: {url}");
    }

    #[test]
    fn client_rest_url_vertex() {
        let client = Client::from_vertex("proj", "us-east1", "tok")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        let url = client.rest_url(ServiceEndpoint::GenerateContent);
        assert!(url.contains("us-east1-aiplatform.googleapis.com"));
        assert!(url.contains(":generateContent"));
    }

    #[test]
    fn live_session_builder_created() {
        let client = Client::from_api_key("key");
        let _builder = client.live(Some(ModelId::from_static(
            "models/gemini-2.0-flash-live-001",
        )));
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
        // Nothing is fetched eagerly: a token minted at construction would be
        // the stale one by the time a reconnect needs it.
        assert_eq!(call_count.load(Ordering::SeqCst), 0);
        // Every REST request consults the source …
        let headers = client.auth_headers().await.unwrap();
        assert_eq!(headers[0].1, "Bearer refreshed-token");
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        // … and so does every Live connection attempt, through the same source.
        let live_config = SessionConfig::from_endpoint(client.endpoint.clone());
        assert_eq!(
            live_config.bearer_token().as_deref(),
            Some("refreshed-token")
        );
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }
}
