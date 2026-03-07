//! Vertex AI Bearer token authentication.

use std::sync::Arc;

use async_trait::async_trait;

use crate::protocol::types::GeminiModel;
use crate::session::AuthError;

use super::url_builders::build_vertex_rest_url;
use super::{AuthProvider, ServiceEndpoint};

/// How a [`VertexAIAuth`] resolves its Bearer token.
enum TokenSource {
    /// Fixed token string — used for WebSocket connections where the token
    /// is only needed once at connect time.
    Fixed(parking_lot::Mutex<String>),
    /// Dynamic token refresher — called on every `auth_headers()` invocation.
    /// Used for HTTP REST calls (e.g., generate) where the token must remain
    /// valid across many requests over a long session.
    Refreshable(Arc<dyn Fn() -> String + Send + Sync>),
}

/// Vertex AI Bearer token authentication.
///
/// Uses a project/location pair to construct the Vertex AI WebSocket URL,
/// and a Bearer token for the `Authorization` header.
///
/// Supports two token modes:
/// - **Fixed** ([`new`](Self::new)): token is set once at construction.
///   Best for WebSocket connections where the token is only needed at connect time.
/// - **Refreshable** ([`with_token_refresher`](Self::with_token_refresher)):
///   a closure is called on every `auth_headers()` invocation, ensuring fresh
///   tokens for long-running HTTP clients (e.g., generate API calls).
pub struct VertexAIAuth {
    project: String,
    location: String,
    token_source: TokenSource,
}

impl VertexAIAuth {
    /// Create a new Vertex AI auth provider with a fixed token.
    ///
    /// The token is stored and reused for all requests. Use this for
    /// WebSocket connections where the token is only needed at connect time.
    pub fn new(
        project: impl Into<String>,
        location: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            location: location.into(),
            token_source: TokenSource::Fixed(parking_lot::Mutex::new(token.into())),
        }
    }

    /// Create a Vertex AI auth provider with a dynamic token refresher.
    ///
    /// The `refresher` closure is called on every `auth_headers()` invocation,
    /// allowing token refresh for long-running HTTP clients. The closure
    /// should handle caching internally to avoid unnecessary overhead.
    pub fn with_token_refresher(
        project: impl Into<String>,
        location: impl Into<String>,
        refresher: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            project: project.into(),
            location: location.into(),
            token_source: TokenSource::Refreshable(Arc::new(refresher)),
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
        let model_id = model.to_string().trim_start_matches("models/").to_string();
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
        let token = match &self.token_source {
            TokenSource::Fixed(m) => m.lock().clone(),
            TokenSource::Refreshable(f) => f(),
        };
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {token}"),
        )])
    }
}
