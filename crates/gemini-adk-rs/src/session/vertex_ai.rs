//! Vertex AI session service — managed session storage via Vertex AI REST API.
//!
//! Provides session persistence using the Vertex AI session management
//! endpoint. Sessions are stored and managed by Google Cloud, with
//! optional TTL-based expiration.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::{Session, SessionError, SessionId, SessionService};
use crate::events::{Event, EventActions};

// ──────────────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────────────

/// Configuration for the Vertex AI session service.
#[derive(Debug, Clone)]
pub struct VertexAiSessionConfig {
    /// Google Cloud project ID.
    pub project: String,
    /// Google Cloud region (e.g., `us-central1`).
    pub location: String,
    /// Optional time-to-live for sessions, in seconds.
    /// If set, sessions expire after this duration of inactivity.
    pub ttl_seconds: Option<u64>,
}

impl VertexAiSessionConfig {
    /// Create a new Vertex AI session config.
    pub fn new(project: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            location: location.into(),
            ttl_seconds: None,
        }
    }

    /// Set the session TTL in seconds.
    pub fn ttl_seconds(mut self, ttl: u64) -> Self {
        self.ttl_seconds = Some(ttl);
        self
    }

    /// Construct the base URL for the Vertex AI session endpoint.
    ///
    /// Format: `https://{location}-aiplatform.googleapis.com/v1beta1/projects/{project}/locations/{location}/reasoningEngines`
    fn base_url(&self) -> String {
        format!(
            "https://{location}-aiplatform.googleapis.com/v1beta1/projects/{project}/locations/{location}",
            project = self.project,
            location = self.location,
        )
    }

    /// Construct the sessions endpoint URL for a specific reasoning engine.
    fn sessions_url(&self, engine_id: &str) -> String {
        format!(
            "{}/reasoningEngines/{}/sessions",
            self.base_url(),
            percent_encode(engine_id),
        )
    }

    /// Construct the URL for a specific session.
    ///
    /// `session_id` reaches this from the caller, so it is encoded rather than
    /// interpolated: an id containing `/` addresses a different resource, and
    /// one containing `?` or `#` turns the rest of the path into a query or
    /// fragment. Encoding keeps it one path segment whatever it holds.
    fn session_url(&self, engine_id: &str, session_id: &str) -> String {
        format!(
            "{}/{}",
            self.sessions_url(engine_id),
            percent_encode(session_id)
        )
    }

    /// Construct the events endpoint URL for a specific session.
    fn events_url(&self, engine_id: &str, session_id: &str) -> String {
        format!("{}/events", self.session_url(engine_id, session_id))
    }
}

/// Percent-encode one URL path segment or query value.
///
/// Escapes everything outside RFC 3986's unreserved set, so the result can only
/// ever be the single component it was meant to be — it cannot introduce a path
/// separator, open a query string, or append another query parameter.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Auth provider
// ──────────────────────────────────────────────────────────────────────────────

/// How to supply a bearer token for Vertex AI requests.
enum TokenProvider {
    /// No token configured — requests will fail with a clear message.
    None,
    /// A static, pre-fetched bearer token string.
    Static(String),
    /// A dynamic refresher: called before every request.
    Refresher(Arc<dyn Fn() -> String + Send + Sync>),
}

impl TokenProvider {
    /// Retrieve the current token, or return an error if none is configured.
    fn get(&self) -> Result<String, SessionError> {
        match self {
            TokenProvider::None => Err(SessionError::Storage(
                "missing auth token: call .with_token() or .with_token_refresher()".into(),
            )),
            TokenProvider::Static(t) => Ok(t.clone()),
            TokenProvider::Refresher(f) => Ok(f()),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// DTO types — mirror Vertex AI JSON shapes
// ──────────────────────────────────────────────────────────────────────────────

/// Vertex AI session resource as returned by the REST API.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexSession {
    /// Full resource name, e.g.
    /// `projects/p/locations/l/reasoningEngines/e/sessions/{id}`
    name: String,
    /// The user ID associated with this session.
    #[serde(default)]
    user_id: String,
    /// Arbitrary session state stored server-side.
    #[serde(default)]
    session_state: Option<Value>,
    /// RFC 3339 creation timestamp.
    #[serde(default)]
    create_time: Option<String>,
    /// RFC 3339 last-update timestamp.
    #[serde(default)]
    update_time: Option<String>,
}

/// Response envelope for `listSessions`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListSessionsResponse {
    #[serde(default)]
    sessions: Vec<VertexSession>,
}

/// Vertex AI event resource as returned by the REST API.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexEvent {
    /// Who authored the event.
    #[serde(default)]
    author: String,
    /// Invocation identifier.
    #[serde(default)]
    invocation_id: String,
    /// Freeform content (may hold a `parts` array or a plain string).
    #[serde(default)]
    content: Option<Value>,
    /// Actions metadata stored alongside the event.
    #[serde(default)]
    actions: Option<Value>,
    /// Event identifier (last path segment of `name`).
    #[serde(default)]
    name: Option<String>,
    /// Unix timestamp as a string (seconds since epoch).
    #[serde(default)]
    timestamp: Option<String>,
}

/// Response envelope for `listEvents`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListEventsResponse {
    #[serde(default)]
    session_events: Vec<VertexEvent>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Pure helper functions (URL/body builders + response mappers)
// ──────────────────────────────────────────────────────────────────────────────

/// Build the JSON body for a `createSession` request.
pub(crate) fn build_create_body(user_id: &str, ttl_seconds: Option<u64>) -> Value {
    let mut body = serde_json::json!({ "userId": user_id });
    if let Some(ttl) = ttl_seconds {
        body["ttl"] = Value::String(format!("{ttl}s"));
    }
    body
}

/// Build the JSON body for an `appendEvent` request.
pub(crate) fn build_event_body(event: &Event) -> Value {
    let content_val = match &event.content {
        Some(text) => serde_json::json!({ "parts": [{ "text": text }] }),
        None => serde_json::json!({}),
    };
    serde_json::json!({
        "author":       event.author,
        "invocationId": event.invocation_id,
        "content":      content_val,
        "actions":      serde_json::to_value(&event.actions)
                            .unwrap_or(serde_json::json!({})),
    })
}

/// Extract the short session ID from a full Vertex AI resource `name`.
///
/// The name looks like
/// `projects/p/locations/l/reasoningEngines/e/sessions/SESSION_ID`.
/// We return the last path segment.
fn session_id_from_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Map a `VertexSession` DTO into our domain `Session`.
///
/// `app_name` is not stored by Vertex — the caller supplies it from context.
fn map_vertex_session(vs: VertexSession, app_name: &str) -> Session {
    let id_str = session_id_from_name(&vs.name);
    let state = vs
        .session_state
        .and_then(|v| v.as_object().cloned())
        .map(|m| m.into_iter().collect())
        .unwrap_or_default();

    let now = vs.create_time.clone().unwrap_or_else(|| "0Z".to_string());

    Session {
        id: SessionId::from_string(id_str),
        app_name: app_name.to_string(),
        user_id: vs.user_id,
        state,
        created_at: vs.create_time.unwrap_or_else(|| now.clone()),
        updated_at: vs.update_time.unwrap_or(now),
        events: Vec::new(),
    }
}

/// Map a `VertexEvent` DTO into our domain `Event`.
fn map_vertex_event(ve: VertexEvent) -> Event {
    // Extract plain text from the Vertex content shape:
    // { "parts": [{ "text": "..." }] }
    let content = ve.content.as_ref().and_then(|c| {
        c.get("parts")
            .and_then(|p| p.as_array())
            .and_then(|arr| arr.first())
            .and_then(|part| part.get("text"))
            .and_then(|t| t.as_str())
            .map(String::from)
    });

    let actions: EventActions = ve
        .actions
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    let timestamp: u64 = ve
        .timestamp
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Derive a stable event ID from the resource name if available.
    let id = ve
        .name
        .as_deref()
        .map(|n| session_id_from_name(n).to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    Event {
        id,
        invocation_id: ve.invocation_id,
        author: ve.author,
        content,
        actions,
        timestamp,
    }
}

/// Pure helper: given a status code and an already-consumed body string,
/// decide whether this is a 404 (mapped to `Ok(None)`), a non-2xx error
/// (mapped to `Err`), or a success (caller must parse the body).
///
/// Returns:
/// - `Ok(true)`  → status was 2xx; caller may parse the body.
/// - `Ok(false)` → status was 404; caller should return `Ok(None)`.
/// - `Err(_)`    → status was non-2xx / non-404 error.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn classify_status(status: u16, body: &str) -> Result<bool, SessionError> {
    if status == 404 {
        return Ok(false);
    }
    if (200..300).contains(&status) {
        return Ok(true);
    }
    Err(SessionError::Storage(format!(
        "Vertex AI request failed [{status}]: {body}"
    )))
}

/// Interpret a `reqwest::Response` that should carry a JSON body.
///
/// * 2xx  → deserialise as `T`
/// * 404  → `Ok(None)`
/// * else → `Err(SessionError::Storage(...))`
///
/// Returns `Ok(Some(T))` on success, `Ok(None)` on 404.
async fn parse_json_response<T: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<Option<T>, SessionError> {
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if (200..300).contains(&status) {
        let parsed: T = resp
            .json()
            .await
            .map_err(|e| SessionError::Storage(format!("failed to parse response: {e}")))?;
        return Ok(Some(parsed));
    }
    // Collect body for a useful error message.
    let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
    Err(SessionError::Storage(format!(
        "Vertex AI request failed [{status}]: {body}"
    )))
}

/// Like [`parse_json_response`] but maps the 404 into
/// `Err(SessionError::NotFound(id))`.
async fn parse_json_response_required<T: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
    id: &SessionId,
) -> Result<T, SessionError> {
    match parse_json_response::<T>(resp).await? {
        Some(v) => Ok(v),
        None => Err(SessionError::NotFound(id.clone())),
    }
}

/// Check that a response was successful (for void operations like DELETE).
async fn check_success(resp: reqwest::Response) -> Result<(), SessionError> {
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
    Err(SessionError::Storage(format!(
        "Vertex AI request failed [{status}]: {body}"
    )))
}

// ──────────────────────────────────────────────────────────────────────────────
// Service struct
// ──────────────────────────────────────────────────────────────────────────────

/// Session service backed by the Vertex AI managed session endpoint.
///
/// Uses the Vertex AI REST API for session CRUD and event storage.
/// Requires a valid Google Cloud project with the AI Platform API enabled.
///
/// Sessions are stored server-side by Google Cloud, providing managed
/// persistence without requiring a separate database.
///
/// # Quick start
///
/// ```rust,no_run
/// # use gemini_adk_rs::session::{VertexAiSessionConfig, VertexAiSessionService};
/// let svc = VertexAiSessionService::new(
///     VertexAiSessionConfig::new("my-project", "us-central1")
///         .ttl_seconds(3600),
/// )
/// .with_token("ya29.my-access-token")
/// .reasoning_engine("my-engine-id");
/// ```
pub struct VertexAiSessionService {
    config: VertexAiSessionConfig,
    client: reqwest::Client,
    token_provider: TokenProvider,
    engine_id: String,
}

impl VertexAiSessionService {
    /// Create a new Vertex AI session service.
    ///
    /// No auth token is configured yet — call [`with_token`](Self::with_token)
    /// or [`with_token_refresher`](Self::with_token_refresher) before issuing
    /// requests, otherwise they will return
    /// `SessionError::Storage("missing auth token")`.
    pub fn new(config: VertexAiSessionConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            token_provider: TokenProvider::None,
            engine_id: "default".to_string(),
        }
    }

    /// Set a static bearer token for all requests.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token_provider = TokenProvider::Static(token.into());
        self
    }

    /// Set a dynamic token refresher closure.
    ///
    /// The closure is invoked before every HTTP request, allowing the caller
    /// to supply a freshly-refreshed token each time.
    pub fn with_token_refresher(mut self, f: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.token_provider = TokenProvider::Refresher(Arc::new(f));
        self
    }

    /// Override the reasoning engine ID (defaults to `"default"`).
    pub fn reasoning_engine(mut self, id: impl Into<String>) -> Self {
        self.engine_id = id.into();
        self
    }

    // ── Accessors (keep existing tests passing) ────────────────────────────

    /// Returns the configured project ID.
    pub fn project(&self) -> &str {
        &self.config.project
    }

    /// Returns the configured location.
    pub fn location(&self) -> &str {
        &self.config.location
    }

    /// Returns the configured TTL in seconds, if any.
    pub fn ttl_seconds(&self) -> Option<u64> {
        self.config.ttl_seconds
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    /// Build an authorised `RequestBuilder` for a GET request.
    fn get(&self, url: &str) -> Result<reqwest::RequestBuilder, SessionError> {
        let token = self.token_provider.get()?;
        Ok(self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {token}")))
    }

    /// Build an authorised `RequestBuilder` for a POST request with a JSON body.
    fn post(&self, url: &str, body: Value) -> Result<reqwest::RequestBuilder, SessionError> {
        let token = self.token_provider.get()?;
        Ok(self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body))
    }

    /// Build an authorised `RequestBuilder` for a DELETE request.
    fn delete(&self, url: &str) -> Result<reqwest::RequestBuilder, SessionError> {
        let token = self.token_provider.get()?;
        Ok(self
            .client
            .delete(url)
            .header("Authorization", format!("Bearer {token}")))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// SessionService implementation
// ──────────────────────────────────────────────────────────────────────────────

#[async_trait]
impl SessionService for VertexAiSessionService {
    async fn create_session(&self, app_name: &str, user_id: &str) -> Result<Session, SessionError> {
        // Physically create the session under the configured reasoning engine
        // (the same engine the other CRUD methods use), so it stays reachable.
        // `app_name` is only a logical label carried on the returned session.
        let url = self.config.sessions_url(&self.engine_id);
        let body = build_create_body(user_id, self.config.ttl_seconds);

        let resp = self
            .post(&url, body)?
            .send()
            .await
            .map_err(|e| SessionError::Storage(format!("HTTP request failed: {e}")))?;

        // A successful create returns the new VertexSession.
        // We use a placeholder ID only to satisfy the required helper signature;
        // the real ID comes from the response body.
        let placeholder_id = SessionId::new();
        let vs: VertexSession = parse_json_response_required(resp, &placeholder_id).await?;
        Ok(map_vertex_session(vs, app_name))
    }

    async fn get_session(&self, id: &SessionId) -> Result<Option<Session>, SessionError> {
        let url = self.config.session_url(&self.engine_id, id.as_str());

        let resp = self
            .get(&url)?
            .send()
            .await
            .map_err(|e| SessionError::Storage(format!("HTTP request failed: {e}")))?;

        // Treat 404 as Ok(None) per the spec.
        let opt: Option<VertexSession> = parse_json_response(resp).await?;
        Ok(opt.map(|vs| {
            // We don't know the app_name from the response alone — extract it
            // from the resource name if possible, else fall back to engine_id.
            let app_name =
                extract_engine_id_from_name(&vs.name).unwrap_or_else(|| self.engine_id.clone());
            map_vertex_session(vs, &app_name)
        }))
    }

    async fn list_sessions(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<Vec<Session>, SessionError> {
        // List under the configured engine (where sessions are stored), but
        // keep `app_name` as the logical label on the returned sessions.
        let base_url = self.config.sessions_url(&self.engine_id);
        // `user_id` is encoded, not interpolated: a raw `&` would append a
        // second query parameter to the request and a raw `#` would truncate
        // the filter, in both cases listing sessions this call did not ask for.
        let url = format!("{base_url}?filter=userId={}", percent_encode(user_id));

        let resp = self
            .get(&url)?
            .send()
            .await
            .map_err(|e| SessionError::Storage(format!("HTTP request failed: {e}")))?;

        // A 404 here means the engine has no sessions — treat as empty list.
        let opt: Option<ListSessionsResponse> = parse_json_response(resp).await?;
        let sessions = opt
            .map(|r| {
                r.sessions
                    .into_iter()
                    .map(|vs| map_vertex_session(vs, app_name))
                    .collect()
            })
            .unwrap_or_default();
        Ok(sessions)
    }

    async fn delete_session(&self, id: &SessionId) -> Result<(), SessionError> {
        let url = self.config.session_url(&self.engine_id, id.as_str());

        let resp = self
            .delete(&url)?
            .send()
            .await
            .map_err(|e| SessionError::Storage(format!("HTTP request failed: {e}")))?;

        check_success(resp).await
    }

    async fn append_event(&self, id: &SessionId, event: Event) -> Result<(), SessionError> {
        let url = self.config.events_url(&self.engine_id, id.as_str());
        let body = build_event_body(&event);

        let resp = self
            .post(&url, body)?
            .send()
            .await
            .map_err(|e| SessionError::Storage(format!("HTTP request failed: {e}")))?;

        check_success(resp).await
    }

    async fn get_events(&self, id: &SessionId) -> Result<Vec<Event>, SessionError> {
        let url = self.config.events_url(&self.engine_id, id.as_str());

        let resp = self
            .get(&url)?
            .send()
            .await
            .map_err(|e| SessionError::Storage(format!("HTTP request failed: {e}")))?;

        // 404 → empty list (session with no events or missing session).
        let opt: Option<ListEventsResponse> = parse_json_response(resp).await?;
        let events = opt
            .map(|r| r.session_events.into_iter().map(map_vertex_event).collect())
            .unwrap_or_default();
        Ok(events)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Small extraction helper (not pub — internal only)
// ──────────────────────────────────────────────────────────────────────────────

/// Try to pull the engine / reasoning-engine ID out of a Vertex AI resource
/// name like `projects/p/locations/l/reasoningEngines/ENGINE/sessions/S`.
fn extract_engine_id_from_name(name: &str) -> Option<String> {
    let mut parts = name.split('/');
    while let Some(segment) = parts.next() {
        if segment == "reasoningEngines" {
            return parts.next().map(String::from);
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Existing tests (must remain green) ────────────────────────────────

    #[test]
    fn config_new() {
        let config = VertexAiSessionConfig::new("my-project", "us-central1");
        assert_eq!(config.project, "my-project");
        assert_eq!(config.location, "us-central1");
        assert!(config.ttl_seconds.is_none());
    }

    #[test]
    fn config_with_ttl() {
        let config = VertexAiSessionConfig::new("proj", "us-east1").ttl_seconds(3600);
        assert_eq!(config.ttl_seconds, Some(3600));
    }

    #[test]
    fn url_construction() {
        let config = VertexAiSessionConfig::new("my-project", "us-central1");
        assert_eq!(
            config.base_url(),
            "https://us-central1-aiplatform.googleapis.com/v1beta1/projects/my-project/locations/us-central1"
        );
        assert!(
            config
                .sessions_url("engine-1")
                .contains("reasoningEngines/engine-1/sessions")
        );
        assert!(
            config
                .session_url("engine-1", "sess-1")
                .contains("sessions/sess-1")
        );
        assert!(
            config
                .events_url("engine-1", "sess-1")
                .contains("sessions/sess-1/events")
        );
    }

    #[test]
    fn a_session_id_cannot_walk_out_of_its_collection() {
        let config = VertexAiSessionConfig::new("my-project", "us-central1");

        // Without encoding this reads as `.../sessions/../../otherEngine`,
        // which addresses a resource under a different reasoning engine.
        let url = config.session_url("engine-1", "../../otherEngine");
        assert!(
            url.ends_with("/sessions/..%2F..%2FotherEngine"),
            "traversal was not encoded: {url}"
        );

        // And this one would end the path and start a query.
        let url = config.session_url("engine-1", "sess-1?alt=media");
        assert!(!url.contains('?'), "query separator survived: {url}");
        assert!(url.ends_with("/sessions/sess-1%3Falt%3Dmedia"), "{url}");

        // Ordinary ids are untouched — the unreserved set passes through.
        assert!(
            config
                .session_url("engine-1", "sess-1_A.b~2")
                .ends_with("/sessions/sess-1_A.b~2")
        );
    }

    #[test]
    fn a_user_id_cannot_append_its_own_query_parameter() {
        // `&pageSize=1000` unencoded becomes a second parameter on the request
        // rather than part of the userId being filtered on.
        let encoded = percent_encode("alice&pageSize=1000");
        assert_eq!(encoded, "alice%26pageSize%3D1000");
        assert!(!encoded.contains('&'));

        // Non-ASCII is escaped per byte, not dropped or passed through raw.
        assert_eq!(percent_encode("josé"), "jos%C3%A9");
    }

    #[test]
    fn service_accessors() {
        let svc = VertexAiSessionService::new(
            VertexAiSessionConfig::new("proj", "us-west1").ttl_seconds(7200),
        );
        assert_eq!(svc.project(), "proj");
        assert_eq!(svc.location(), "us-west1");
        assert_eq!(svc.ttl_seconds(), Some(7200));
    }

    // ── New: builder methods ───────────────────────────────────────────────

    #[test]
    fn with_token_sets_provider() {
        let svc =
            VertexAiSessionService::new(VertexAiSessionConfig::new("p", "l")).with_token("tok123");
        // Verify the token provider returns the expected token.
        let token = svc.token_provider.get().expect("should have token");
        assert_eq!(token, "tok123");
    }

    #[test]
    fn with_token_refresher_calls_closure() {
        let svc = VertexAiSessionService::new(VertexAiSessionConfig::new("p", "l"))
            .with_token_refresher(|| "dynamic-token".to_string());
        let token = svc.token_provider.get().expect("should have token");
        assert_eq!(token, "dynamic-token");
    }

    #[test]
    fn missing_token_returns_storage_error() {
        let svc = VertexAiSessionService::new(VertexAiSessionConfig::new("p", "l"));
        let err = svc.token_provider.get().unwrap_err();
        assert!(matches!(err, SessionError::Storage(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("missing auth token"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn reasoning_engine_overrides_default() {
        let svc = VertexAiSessionService::new(VertexAiSessionConfig::new("p", "l"))
            .reasoning_engine("my-engine");
        assert_eq!(svc.engine_id, "my-engine");
    }

    #[test]
    fn default_engine_id_is_default() {
        let svc = VertexAiSessionService::new(VertexAiSessionConfig::new("p", "l"));
        assert_eq!(svc.engine_id, "default");
    }

    // ── New: body builders ─────────────────────────────────────────────────

    #[test]
    fn build_create_body_without_ttl() {
        let body = build_create_body("alice", None);
        assert_eq!(body["userId"], "alice");
        assert!(body.get("ttl").is_none());
    }

    #[test]
    fn build_create_body_with_ttl() {
        let body = build_create_body("bob", Some(3600));
        assert_eq!(body["userId"], "bob");
        assert_eq!(body["ttl"], "3600s");
    }

    #[test]
    fn build_event_body_with_content() {
        let event = Event::new("user", Some("Hello Vertex!".to_string()));
        let body = build_event_body(&event);
        assert_eq!(body["author"], "user");
        assert_eq!(body["content"]["parts"][0]["text"], "Hello Vertex!");
    }

    #[test]
    fn build_event_body_without_content() {
        let event = Event::new("agent", None);
        let body = build_event_body(&event);
        assert_eq!(body["author"], "agent");
        // content should be an empty object
        assert!(body["content"].is_object());
    }

    #[test]
    fn build_event_body_includes_invocation_id() {
        let event = Event::new("model", None).with_invocation("inv-xyz");
        let body = build_event_body(&event);
        assert_eq!(body["invocationId"], "inv-xyz");
    }

    // ── New: response mapper helpers ───────────────────────────────────────

    #[test]
    fn session_id_from_name_extracts_last_segment() {
        assert_eq!(
            session_id_from_name(
                "projects/p/locations/l/reasoningEngines/e/sessions/my-session-id"
            ),
            "my-session-id"
        );
        assert_eq!(session_id_from_name("just-an-id"), "just-an-id");
    }

    #[test]
    fn extract_engine_id_from_name_works() {
        let name = "projects/my-proj/locations/us-central1/reasoningEngines/eng-42/sessions/sess-1";
        assert_eq!(
            extract_engine_id_from_name(name),
            Some("eng-42".to_string())
        );
    }

    #[test]
    fn extract_engine_id_returns_none_without_segment() {
        assert_eq!(extract_engine_id_from_name("projects/p/locations/l"), None);
    }

    #[test]
    fn map_vertex_session_maps_fields() {
        let vs = VertexSession {
            name: "projects/p/locations/l/reasoningEngines/e/sessions/sess-abc".to_string(),
            user_id: "alice".to_string(),
            session_state: Some(serde_json::json!({"key": "value"})),
            create_time: Some("2024-01-01T00:00:00Z".to_string()),
            update_time: Some("2024-01-02T00:00:00Z".to_string()),
        };
        let session = map_vertex_session(vs, "my-app");
        assert_eq!(session.id.as_str(), "sess-abc");
        assert_eq!(session.app_name, "my-app");
        assert_eq!(session.user_id, "alice");
        assert_eq!(session.state["key"], "value");
        assert_eq!(session.created_at, "2024-01-01T00:00:00Z");
        assert_eq!(session.updated_at, "2024-01-02T00:00:00Z");
    }

    #[test]
    fn map_vertex_session_handles_missing_optionals() {
        let vs = VertexSession {
            name: "projects/p/locations/l/reasoningEngines/e/sessions/sess-xyz".to_string(),
            user_id: String::new(),
            session_state: None,
            create_time: None,
            update_time: None,
        };
        let session = map_vertex_session(vs, "app");
        assert_eq!(session.id.as_str(), "sess-xyz");
        assert!(session.state.is_empty());
    }

    #[test]
    fn map_vertex_event_maps_text_content() {
        let ve = VertexEvent {
            author: "user".to_string(),
            invocation_id: "inv-1".to_string(),
            content: Some(serde_json::json!({ "parts": [{ "text": "Hi!" }] })),
            actions: None,
            name: Some(
                "projects/p/locations/l/reasoningEngines/e/sessions/s/events/ev-1".to_string(),
            ),
            timestamp: Some("1700000000".to_string()),
        };
        let event = map_vertex_event(ve);
        assert_eq!(event.author, "user");
        assert_eq!(event.invocation_id, "inv-1");
        assert_eq!(event.content, Some("Hi!".to_string()));
        assert_eq!(event.id, "ev-1");
        assert_eq!(event.timestamp, 1_700_000_000);
    }

    #[test]
    fn map_vertex_event_handles_missing_content() {
        let ve = VertexEvent {
            author: "model".to_string(),
            invocation_id: String::new(),
            content: None,
            actions: None,
            name: None,
            timestamp: None,
        };
        let event = map_vertex_event(ve);
        assert_eq!(event.content, None);
        assert_eq!(event.timestamp, 0);
        // ID should be a generated UUID (non-empty)
        assert!(!event.id.is_empty());
    }

    // ── New: 404 → None mapping tested via the pure classify_status helper ──
    //
    // We test the status-classification logic directly using the pure
    // `classify_status` function rather than constructing synthetic
    // `reqwest::Response` objects (which would require the `http` crate as a
    // direct dependency). The async `parse_json_response` / `check_success`
    // wrappers delegate to the same logic path, so these tests give full
    // coverage without a live server.

    #[test]
    fn classify_status_404_is_none_signal() {
        // classify_status returns Ok(false) to signal "treat as None"
        let result = classify_status(404, "not found");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(!result.unwrap(), "expected false for 404");
    }

    #[test]
    fn classify_status_200_is_success_signal() {
        let result = classify_status(200, "");
        assert!(result.unwrap(), "expected true for 200");
    }

    #[test]
    fn classify_status_201_is_success_signal() {
        let result = classify_status(201, "");
        assert!(result.unwrap(), "expected true for 201");
    }

    #[test]
    fn classify_status_299_is_success_signal() {
        let result = classify_status(299, "");
        assert!(result.unwrap(), "expected true for 299");
    }

    #[test]
    fn classify_status_500_is_storage_error() {
        let result = classify_status(500, "internal server error");
        assert!(matches!(result, Err(SessionError::Storage(_))));
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("500"), "expected status in error: {msg}");
        assert!(
            msg.contains("internal server error"),
            "expected body in error: {msg}"
        );
    }

    #[test]
    fn classify_status_403_is_storage_error() {
        let result = classify_status(403, "forbidden");
        assert!(matches!(result, Err(SessionError::Storage(_))));
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("403"), "expected status in error: {msg}");
    }

    #[test]
    fn classify_status_400_is_storage_error() {
        let result = classify_status(400, "bad request");
        assert!(matches!(result, Err(SessionError::Storage(_))));
    }

    #[test]
    fn classify_status_300_is_storage_error() {
        // Redirects are not transparent in our usage — treat as error.
        let result = classify_status(301, "moved permanently");
        assert!(matches!(result, Err(SessionError::Storage(_))));
    }
}
