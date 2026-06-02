//! Vertex AI RAG memory service — stores and retrieves memories via Vertex AI RAG.
//!
//! Mirrors ADK-Python's `vertex_ai_rag_memory_service`:
//! * `add_session_to_memory` ingests session events by uploading them as a RAG
//!   file (one JSON line per event) into the configured corpus, encoding the
//!   `app_name` / `user_id` / `session_id` into the file's display name.
//! * `search_memory` queries the corpus via the `:retrieveContexts` endpoint,
//!   then filters results by the encoded `app_name` / `user_id`.
//!
//! The generic [`MemoryService`] trait (key-value oriented) is implemented on
//! top of these primitives: `store` uploads a single entry, `search` queries
//! the corpus.

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{MemoryEntry, MemoryError, MemoryService};

/// Display-name prefix used to tag uploaded session files, mirroring ADK.
const SOURCE_DISPLAY_NAME_PREFIX: &str = "adk-memory-v1.";

// ──────────────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────────────

/// Configuration for the Vertex AI RAG memory service.
#[derive(Debug, Clone)]
pub struct VertexAiRagMemoryConfig {
    /// The RAG corpus resource name.
    /// Format: `projects/{project}/locations/{location}/ragCorpora/{corpus_id}`
    pub corpus: String,
    /// Google Cloud project ID.
    pub project: String,
    /// Google Cloud location (e.g. `us-central1`).
    pub location: String,
    /// Number of contexts to retrieve (maps to `similarity_top_k`).
    pub similarity_top_k: Option<u32>,
    /// Only return contexts with a vector distance smaller than this threshold.
    pub vector_distance_threshold: Option<f64>,
}

impl VertexAiRagMemoryConfig {
    /// Create a new config from a corpus resource name, deriving the
    /// project / location from it when fully-qualified.
    pub fn from_corpus(corpus: impl Into<String>) -> Self {
        let corpus = corpus.into();
        let (project, location) = parse_project_location(&corpus);
        Self {
            corpus,
            project,
            location,
            similarity_top_k: None,
            vector_distance_threshold: Some(10.0),
        }
    }

    /// Build the `:retrieveContexts` endpoint URL.
    fn retrieve_contexts_url(&self) -> String {
        format!(
            "https://{location}-aiplatform.googleapis.com/v1beta1/projects/{project}/locations/{location}:retrieveContexts",
            project = self.project,
            location = self.location,
        )
    }

    /// Build the RAG file upload endpoint URL for the configured corpus.
    ///
    /// Format:
    /// `https://{location}-aiplatform.googleapis.com/upload/v1beta1/{corpus}/ragFiles:upload`
    fn upload_rag_file_url(&self) -> String {
        format!(
            "https://{location}-aiplatform.googleapis.com/upload/v1beta1/{corpus}/ragFiles:upload",
            location = self.location,
            corpus = self.corpus,
        )
    }
}

/// Best-effort extraction of `{project}` / `{location}` from a corpus resource
/// name of the form `projects/{project}/locations/{location}/ragCorpora/{id}`.
fn parse_project_location(corpus: &str) -> (String, String) {
    let mut project = String::new();
    let mut location = String::new();
    let mut parts = corpus.split('/');
    while let Some(seg) = parts.next() {
        match seg {
            "projects" => project = parts.next().unwrap_or_default().to_string(),
            "locations" => location = parts.next().unwrap_or_default().to_string(),
            _ => {}
        }
    }
    (project, location)
}

// ──────────────────────────────────────────────────────────────────────────────
// source_display_name encode / decode (mirrors ADK)
// ──────────────────────────────────────────────────────────────────────────────

fn encode_part(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value.as_bytes())
}

fn decode_part(value: &str) -> Option<String> {
    URL_SAFE_NO_PAD
        .decode(value.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Encode `app_name` / `user_id` / `session_id` into a display name, matching
/// ADK's `_build_source_display_name`.
pub(crate) fn build_source_display_name(app_name: &str, user_id: &str, session_id: &str) -> String {
    format!(
        "{prefix}{a}.{u}.{s}",
        prefix = SOURCE_DISPLAY_NAME_PREFIX,
        a = encode_part(app_name),
        u = encode_part(user_id),
        s = encode_part(session_id),
    )
}

/// Decode a display name back into `(app_name, user_id, session_id)`, matching
/// ADK's `_parse_source_display_name`. Returns `None` if it doesn't match the
/// expected three-part encoded form.
pub(crate) fn parse_source_display_name(name: &str) -> Option<(String, String, String)> {
    if let Some(rest) = name.strip_prefix(SOURCE_DISPLAY_NAME_PREFIX) {
        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        return Some((
            decode_part(parts[0])?,
            decode_part(parts[1])?,
            decode_part(parts[2])?,
        ));
    }
    // Legacy dot-delimited (plaintext) form.
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ))
}

// ──────────────────────────────────────────────────────────────────────────────
// Token provider (mirrors session/vertex_ai.rs)
// ──────────────────────────────────────────────────────────────────────────────

enum TokenProvider {
    None,
    Static(String),
    Refresher(Arc<dyn Fn() -> String + Send + Sync>),
}

impl TokenProvider {
    fn get(&self) -> Result<String, MemoryError> {
        match self {
            TokenProvider::None => Err(MemoryError::Storage(
                "missing auth token: call .with_token() or .with_token_refresher()".into(),
            )),
            TokenProvider::Static(t) => Ok(t.clone()),
            TokenProvider::Refresher(f) => Ok(f()),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Wire DTOs — `:retrieveContexts` response
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetrieveContextsResponse {
    #[serde(default)]
    contexts: RagContexts,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RagContexts {
    #[serde(default)]
    contexts: Vec<RagContext>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RagContext {
    #[serde(default)]
    text: String,
    #[serde(default)]
    source_display_name: String,
}

/// Build the request body for a `:retrieveContexts` call against this corpus.
fn build_retrieve_body(
    query: &str,
    corpus: &str,
    similarity_top_k: Option<u32>,
    vector_distance_threshold: Option<f64>,
) -> Value {
    let mut vertex_rag_store = json!({
        "ragResources": [ { "ragCorpus": corpus } ],
    });
    if let Some(threshold) = vector_distance_threshold {
        vertex_rag_store["vectorDistanceThreshold"] = json!(threshold);
    }
    let mut query_obj = json!({ "text": query });
    if let Some(top_k) = similarity_top_k {
        query_obj["ragRetrievalConfig"] = json!({ "topK": top_k });
    }
    json!({
        "vertexRagStore": vertex_rag_store,
        "query": query_obj,
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Service struct
// ──────────────────────────────────────────────────────────────────────────────

/// Memory service backed by Vertex AI RAG.
///
/// Stores memory as RAG files in a corpus (via `ragFiles:upload`) and uses
/// semantic search (`:retrieveContexts`) for retrieval.
///
/// # Quick start
///
/// ```rust,no_run
/// # use gemini_adk_rs::memory::{VertexAiRagMemoryConfig, VertexAiRagMemoryService, MemoryService, MemoryEntry};
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let svc = VertexAiRagMemoryService::new(
///     VertexAiRagMemoryConfig::from_corpus(
///         "projects/my-proj/locations/us-central1/ragCorpora/my-corpus",
///     ),
/// )
/// .with_token("ya29.my-access-token");
///
/// let hits = svc.search("session-1", "what did the user order?").await?;
/// # Ok(())
/// # }
/// ```
pub struct VertexAiRagMemoryService {
    config: VertexAiRagMemoryConfig,
    client: reqwest::Client,
    token_provider: TokenProvider,
}

impl VertexAiRagMemoryService {
    /// Create a new Vertex AI RAG memory service.
    ///
    /// No auth token is configured yet — call [`with_token`](Self::with_token)
    /// or [`with_token_refresher`](Self::with_token_refresher) before issuing
    /// requests.
    pub fn new(config: VertexAiRagMemoryConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            token_provider: TokenProvider::None,
        }
    }

    /// Set a static bearer token for all requests.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token_provider = TokenProvider::Static(token.into());
        self
    }

    /// Set a dynamic token refresher closure (invoked before every request).
    pub fn with_token_refresher(mut self, f: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.token_provider = TokenProvider::Refresher(Arc::new(f));
        self
    }

    /// Returns the configured corpus resource name.
    pub fn corpus(&self) -> &str {
        &self.config.corpus
    }

    /// Upload a text document to the RAG corpus with the given display name.
    ///
    /// This mirrors ADK's `rag.upload_file(...)`: the display name carries the
    /// session info (since the upload API doesn't accept arbitrary metadata).
    async fn upload_text(&self, contents: &str, display_name: &str) -> Result<(), MemoryError> {
        let token = self.token_provider.get()?;
        let url = self.config.upload_rag_file_url();

        // Multipart upload: a metadata part (rag file spec) + the file body.
        let metadata = json!({
            "rag_file": { "display_name": display_name },
        });
        let form = reqwest::multipart::Form::new()
            .text("metadata", metadata.to_string())
            .part(
                "file",
                reqwest::multipart::Part::text(contents.to_string())
                    .file_name(format!("{display_name}.txt"))
                    .mime_str("text/plain")
                    .map_err(|e| MemoryError::Storage(format!("invalid mime: {e}")))?,
            );

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Goog-Upload-Protocol", "multipart")
            .multipart(form)
            .send()
            .await
            .map_err(|e| MemoryError::Storage(format!("HTTP upload failed: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
            return Err(MemoryError::Storage(format!(
                "Vertex AI RAG upload failed [{status}]: {body}"
            )));
        }
        Ok(())
    }

    /// Ingest a batch of memory entries for a session, mirroring ADK's
    /// `add_session_to_memory`. Each entry is serialised as one JSON line and
    /// the whole batch is uploaded as a single RAG file tagged with an encoded
    /// display name.
    ///
    /// `app_name` / `user_id` / `session_id` are encoded into the file display
    /// name so that [`search`](Self::search) can later filter by scope.
    pub async fn add_session_to_memory(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: &[MemoryEntry],
    ) -> Result<(), MemoryError> {
        let mut lines = Vec::new();
        for entry in entries {
            let text = match &entry.value {
                Value::String(s) => s.replace('\n', " "),
                other => other.to_string(),
            };
            lines.push(
                json!({
                    "author": entry.key,
                    "timestamp": entry.updated_at,
                    "text": text,
                })
                .to_string(),
            );
        }
        let contents = lines.join("\n");
        let display_name = build_source_display_name(app_name, user_id, session_id);
        self.upload_text(&contents, &display_name).await
    }

    /// Query the corpus and return the raw retrieved contexts (text +
    /// source_display_name), mirroring ADK's `search_memory` retrieval step.
    async fn retrieve_contexts(&self, query: &str) -> Result<Vec<RagContext>, MemoryError> {
        let token = self.token_provider.get()?;
        let url = self.config.retrieve_contexts_url();
        let body = build_retrieve_body(
            query,
            &self.config.corpus,
            self.config.similarity_top_k,
            self.config.vector_distance_threshold,
        );

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::Storage(format!("HTTP request failed: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let err_body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
            return Err(MemoryError::Storage(format!(
                "Vertex AI RAG retrieveContexts failed [{status}]: {err_body}"
            )));
        }

        let parsed: RetrieveContextsResponse = resp.json().await.map_err(|e| {
            MemoryError::Storage(format!("failed to parse retrieveContexts response: {e}"))
        })?;
        Ok(parsed.contexts.contexts)
    }

    /// Search memory scoped to a specific `app_name` / `user_id`, mirroring
    /// ADK's `search_memory`. Filters retrieved contexts by the encoded display
    /// name and parses the per-event JSON lines back into [`MemoryEntry`]s.
    pub async fn search_memory(
        &self,
        app_name: &str,
        user_id: &str,
        query: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let contexts = self.retrieve_contexts(query).await?;
        Ok(parse_scoped_contexts(contexts, Some((app_name, user_id))))
    }
}

/// Parse retrieved contexts into memory entries, optionally filtering by
/// `(app_name, user_id)` encoded in each context's `source_display_name`.
fn parse_scoped_contexts(
    contexts: Vec<RagContext>,
    scope: Option<(&str, &str)>,
) -> Vec<MemoryEntry> {
    let mut out = Vec::new();
    for ctx in contexts {
        if let Some((app_name, user_id)) = scope {
            match parse_source_display_name(&ctx.source_display_name) {
                Some((src_app, src_user, _session)) => {
                    if src_app != app_name || src_user != user_id {
                        continue;
                    }
                }
                None => continue,
            }
        }
        for line in ctx.text.split('\n') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<Value>(line) {
                let author = event
                    .get("author")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let timestamp = event
                    .get("timestamp")
                    .and_then(|t| t.as_u64().or_else(|| t.as_f64().map(|f| f as u64)))
                    .unwrap_or(0);
                let text = event
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                out.push(MemoryEntry {
                    key: author,
                    value: Value::String(text),
                    created_at: timestamp,
                    updated_at: timestamp,
                });
            }
        }
    }
    out
}

#[async_trait]
impl MemoryService for VertexAiRagMemoryService {
    /// Ingest a single entry into the corpus. The `session_id` scopes the
    /// uploaded file's display name (app/user default to the entry key /
    /// session id when not separately tracked).
    async fn store(&self, session_id: &str, entry: MemoryEntry) -> Result<(), MemoryError> {
        // Treat the session_id as both app_name proxy and session id; callers
        // that need full app/user scoping should use `add_session_to_memory`.
        self.add_session_to_memory(session_id, session_id, session_id, &[entry])
            .await
    }

    /// Vertex AI RAG doesn't support direct key-based retrieval — use
    /// [`search`](Self::search) instead.
    async fn get(&self, _session_id: &str, _key: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        Ok(None)
    }

    /// Listing is not supported by the RAG retrieval API (semantic only).
    async fn list(&self, _session_id: &str) -> Result<Vec<MemoryEntry>, MemoryError> {
        Ok(vec![])
    }

    /// Semantic search against the corpus via `:retrieveContexts`. Results are
    /// not scope-filtered here (use [`search_memory`](Self::search_memory) for
    /// app/user scoping).
    async fn search(
        &self,
        _session_id: &str,
        query: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let contexts = self.retrieve_contexts(query).await?;
        Ok(parse_scoped_contexts(contexts, None))
    }

    /// Deletion of individual RAG files by key is not supported.
    async fn delete(&self, _session_id: &str, _key: &str) -> Result<(), MemoryError> {
        Ok(())
    }

    /// Clearing a corpus is a destructive admin operation — not performed here.
    async fn clear(&self, _session_id: &str) -> Result<(), MemoryError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> VertexAiRagMemoryConfig {
        VertexAiRagMemoryConfig::from_corpus(
            "projects/test/locations/us-central1/ragCorpora/test-corpus",
        )
    }

    #[test]
    fn service_metadata() {
        let svc = VertexAiRagMemoryService::new(test_config());
        assert!(svc.corpus().contains("test-corpus"));
    }

    #[test]
    fn config_parses_project_location() {
        let cfg = test_config();
        assert_eq!(cfg.project, "test");
        assert_eq!(cfg.location, "us-central1");
        assert!(cfg
            .retrieve_contexts_url()
            .ends_with(":retrieveContexts"));
        assert!(cfg.upload_rag_file_url().ends_with("/ragFiles:upload"));
    }

    #[test]
    fn display_name_roundtrip() {
        let dn = build_source_display_name("my-app", "user-1", "sess-42");
        assert!(dn.starts_with(SOURCE_DISPLAY_NAME_PREFIX));
        let (a, u, s) = parse_source_display_name(&dn).expect("should parse");
        assert_eq!(a, "my-app");
        assert_eq!(u, "user-1");
        assert_eq!(s, "sess-42");
    }

    #[test]
    fn display_name_handles_dots_in_ids() {
        // Encoded form is dot-safe even when IDs contain dots.
        let dn = build_source_display_name("a.b", "c.d", "e.f");
        let (a, u, s) = parse_source_display_name(&dn).expect("should parse");
        assert_eq!((a.as_str(), u.as_str(), s.as_str()), ("a.b", "c.d", "e.f"));
    }

    #[test]
    fn parse_legacy_plain_display_name() {
        let (a, u, s) = parse_source_display_name("app.user.session").expect("legacy ok");
        assert_eq!((a.as_str(), u.as_str(), s.as_str()), ("app", "user", "session"));
    }

    #[test]
    fn parse_rejects_malformed_display_name() {
        assert!(parse_source_display_name("not-a-valid-name").is_none());
        assert!(parse_source_display_name("adk-memory-v1.onlyonepart").is_none());
    }

    #[test]
    fn builds_retrieve_body() {
        let body = build_retrieve_body("q", "corpus-x", Some(3), Some(10.0));
        assert_eq!(body["query"]["text"], "q");
        assert_eq!(body["query"]["ragRetrievalConfig"]["topK"], 3);
        assert_eq!(
            body["vertexRagStore"]["ragResources"][0]["ragCorpus"],
            "corpus-x"
        );
        assert_eq!(body["vertexRagStore"]["vectorDistanceThreshold"], 10.0);
    }

    #[test]
    fn parse_scoped_contexts_filters_by_scope() {
        let dn_match = build_source_display_name("app", "alice", "s1");
        let dn_other = build_source_display_name("app", "bob", "s2");
        let contexts = vec![
            RagContext {
                text: json!({"author": "user", "timestamp": 100, "text": "hello"}).to_string(),
                source_display_name: dn_match,
            },
            RagContext {
                text: json!({"author": "user", "timestamp": 200, "text": "nope"}).to_string(),
                source_display_name: dn_other,
            },
        ];
        let entries = parse_scoped_contexts(contexts, Some(("app", "alice")));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "user");
        assert_eq!(entries[0].value, json!("hello"));
        assert_eq!(entries[0].created_at, 100);
    }

    #[test]
    fn parse_scoped_contexts_no_scope_keeps_all() {
        let contexts = vec![RagContext {
            text: json!({"author": "model", "timestamp": 5, "text": "hi"}).to_string(),
            source_display_name: String::new(),
        }];
        let entries = parse_scoped_contexts(contexts, None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "model");
    }

    #[tokio::test]
    async fn search_without_token_errors() {
        let svc = VertexAiRagMemoryService::new(test_config());
        let result = svc.search("s1", "test").await;
        assert!(result.is_err());
    }
}
