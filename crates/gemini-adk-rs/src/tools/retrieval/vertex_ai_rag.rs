//! Vertex AI RAG retrieval tool — retrieve context via Vertex AI RAG API.
//!
//! Mirrors ADK-Python's `vertex_ai_rag_retrieval` tool. Calls the Vertex AI
//! RAG `:retrieveContexts` endpoint for the configured rag corpora / resources
//! and returns the retrieved contexts as the tool result.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::base::{BaseRetrievalTool, RetrievalResult};
use crate::error::ToolError;

/// Configuration for Vertex AI RAG retrieval.
///
/// Mirrors ADK-Python's `VertexRagStore` retrieval parameters. A retrieval may
/// target one or more rag corpora (resource names) and constrain the result set
/// via `similarity_top_k` and `vector_distance_threshold`.
#[derive(Debug, Clone)]
pub struct VertexAiRagConfig {
    /// Google Cloud project ID.
    pub project: String,
    /// Google Cloud location (e.g. `us-central1`).
    pub location: String,
    /// The RAG corpus resource name(s).
    /// Format: `projects/{project}/locations/{location}/ragCorpora/{corpus_id}`
    pub rag_corpora: Vec<String>,
    /// Number of contexts to retrieve (maps to `similarity_top_k`).
    pub similarity_top_k: Option<u32>,
    /// Only return contexts with a vector distance smaller than this threshold.
    pub vector_distance_threshold: Option<f64>,
}

impl VertexAiRagConfig {
    /// Create a new config from a single corpus resource name.
    ///
    /// The project and location are parsed from the corpus resource name when
    /// it is fully-qualified (`projects/{project}/locations/{location}/...`).
    pub fn from_corpus(corpus: impl Into<String>) -> Self {
        let corpus = corpus.into();
        let (project, location) = parse_project_location(&corpus);
        Self {
            project,
            location,
            rag_corpora: vec![corpus],
            similarity_top_k: None,
            vector_distance_threshold: None,
        }
    }

    /// Set the explicit project / location used to build the endpoint URL.
    pub fn with_project_location(
        mut self,
        project: impl Into<String>,
        location: impl Into<String>,
    ) -> Self {
        self.project = project.into();
        self.location = location.into();
        self
    }

    /// Set the number of contexts to retrieve.
    pub fn with_similarity_top_k(mut self, top_k: u32) -> Self {
        self.similarity_top_k = Some(top_k);
        self
    }

    /// Set the vector distance threshold.
    pub fn with_vector_distance_threshold(mut self, threshold: f64) -> Self {
        self.vector_distance_threshold = Some(threshold);
        self
    }

    /// Build the `:retrieveContexts` endpoint URL.
    ///
    /// Format:
    /// `https://{location}-aiplatform.googleapis.com/v1beta1/projects/{project}/locations/{location}:retrieveContexts`
    fn retrieve_contexts_url(&self) -> String {
        format!(
            "https://{location}-aiplatform.googleapis.com/v1beta1/projects/{project}/locations/{location}:retrieveContexts",
            project = self.project,
            location = self.location,
        )
    }
}

/// Best-effort extraction of `{project}` / `{location}` from a rag corpus
/// resource name of the form
/// `projects/{project}/locations/{location}/ragCorpora/{id}`.
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
// Token provider (mirrors session/vertex_ai.rs)
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
    fn get(&self) -> Result<String, ToolError> {
        match self {
            TokenProvider::None => Err(ToolError::ExecutionFailed(
                "missing auth token: call .with_token() or .with_token_refresher()".into(),
            )),
            TokenProvider::Static(t) => Ok(t.clone()),
            TokenProvider::Refresher(f) => Ok(f()),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Wire DTOs — shapes returned by `:retrieveContexts`
// ──────────────────────────────────────────────────────────────────────────────

/// Top-level response envelope for `:retrieveContexts`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetrieveContextsResponse {
    #[serde(default)]
    pub contexts: RagContexts,
}

/// The `contexts` object inside the response (itself wrapping a list).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RagContexts {
    #[serde(default)]
    pub contexts: Vec<RagContext>,
}

/// A single retrieved context chunk.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RagContext {
    #[serde(default)]
    pub text: String,
    /// Source resource URI (e.g. the GCS path of the ingested file).
    #[serde(default)]
    pub source_uri: String,
    /// Display name supplied at ingest time (carries session info for memory).
    #[serde(default)]
    pub source_display_name: String,
    /// Vector distance / relevance score (lower distance = more relevant).
    #[serde(default)]
    pub distance: Option<f64>,
    /// Some API revisions return a `score` field instead of `distance`.
    #[serde(default)]
    pub score: Option<f64>,
}

/// Build the JSON request body for a `:retrieveContexts` call.
pub(crate) fn build_retrieve_body(
    query: &str,
    rag_corpora: &[String],
    similarity_top_k: Option<u32>,
    vector_distance_threshold: Option<f64>,
) -> Value {
    let resources: Vec<Value> = rag_corpora
        .iter()
        .map(|c| json!({ "ragCorpus": c }))
        .collect();

    let mut vertex_rag_store = json!({ "ragResources": resources });
    if let Some(threshold) = vector_distance_threshold {
        vertex_rag_store["vectorDistanceThreshold"] = json!(threshold);
    }

    let mut retrieval_config = json!({});
    if let Some(top_k) = similarity_top_k {
        retrieval_config["topK"] = json!(top_k);
    }
    let mut query_obj = json!({ "text": query });
    if retrieval_config.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        query_obj["ragRetrievalConfig"] = retrieval_config;
    }

    json!({
        "vertexRagStore": vertex_rag_store,
        "query": query_obj,
    })
}

/// Map a wire response into the crate's `RetrievalResult` list.
pub(crate) fn map_contexts(resp: RetrieveContextsResponse) -> Vec<RetrievalResult> {
    resp.contexts
        .contexts
        .into_iter()
        .map(|c| {
            // Prefer an explicit score; otherwise derive one from distance
            // (distance is "smaller is better", so invert into a 0..1 score).
            let score = c.score.unwrap_or_else(|| match c.distance {
                Some(d) => 1.0 / (1.0 + d.max(0.0)),
                None => 0.0,
            });
            let source = if !c.source_uri.is_empty() {
                c.source_uri
            } else {
                c.source_display_name.clone()
            };
            RetrievalResult {
                content: c.text,
                source,
                score,
                metadata: json!({ "sourceDisplayName": c.source_display_name }),
            }
        })
        .collect()
}

/// Retrieval tool that searches via the Vertex AI RAG API.
///
/// Calls the Vertex AI RAG `:retrieveContexts` endpoint to retrieve relevant
/// document chunks from the configured corpora.
///
/// # Quick start
///
/// ```rust,no_run
/// # use gemini_adk_rs::tools::retrieval::{VertexAiRagConfig, VertexAiRagRetrievalTool, BaseRetrievalTool};
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let tool = VertexAiRagRetrievalTool::new(
///     VertexAiRagConfig::from_corpus(
///         "projects/my-proj/locations/us-central1/ragCorpora/my-corpus",
///     )
///     .with_similarity_top_k(5),
/// )
/// .with_token("ya29.my-access-token");
///
/// let results = tool.retrieve("what is the refund policy?", 5).await?;
/// # Ok(())
/// # }
/// ```
pub struct VertexAiRagRetrievalTool {
    config: VertexAiRagConfig,
    client: reqwest::Client,
    token_provider: TokenProvider,
}

impl VertexAiRagRetrievalTool {
    /// Create a new Vertex AI RAG retrieval tool.
    ///
    /// No auth token is configured yet — call [`with_token`](Self::with_token)
    /// or [`with_token_refresher`](Self::with_token_refresher) before issuing
    /// requests.
    pub fn new(config: VertexAiRagConfig) -> Self {
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

    /// Returns the configured corpora resource names.
    pub fn corpora(&self) -> &[String] {
        &self.config.rag_corpora
    }

    /// Returns the first configured corpus (convenience accessor).
    pub fn corpus(&self) -> &str {
        self.config
            .rag_corpora
            .first()
            .map(String::as_str)
            .unwrap_or("")
    }
}

#[async_trait]
impl BaseRetrievalTool for VertexAiRagRetrievalTool {
    fn name(&self) -> &str {
        "vertex_ai_rag_retrieval"
    }

    async fn retrieve(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<RetrievalResult>, ToolError> {
        // Per-call top_k overrides the configured default when set.
        let top_k = self
            .config
            .similarity_top_k
            .or_else(|| (top_k > 0).then_some(top_k as u32));

        let body = build_retrieve_body(
            query,
            &self.config.rag_corpora,
            top_k,
            self.config.vector_distance_threshold,
        );

        let token = self.token_provider.get()?;
        let url = self.config.retrieve_contexts_url();

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("HTTP request failed: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let err_body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
            return Err(ToolError::ExecutionFailed(format!(
                "Vertex AI RAG retrieveContexts failed [{status}]: {err_body}"
            )));
        }

        let parsed: RetrieveContextsResponse = resp.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to parse retrieveContexts response: {e}"))
        })?;

        Ok(map_contexts(parsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> VertexAiRagConfig {
        VertexAiRagConfig::from_corpus(
            "projects/my-proj/locations/us-central1/ragCorpora/my-corpus",
        )
    }

    #[test]
    fn tool_metadata() {
        let tool = VertexAiRagRetrievalTool::new(test_config());
        assert_eq!(tool.name(), "vertex_ai_rag_retrieval");
        assert!(tool.corpus().contains("my-corpus"));
    }

    #[test]
    fn parses_project_location_from_corpus() {
        let cfg = test_config();
        assert_eq!(cfg.project, "my-proj");
        assert_eq!(cfg.location, "us-central1");
        assert!(cfg
            .retrieve_contexts_url()
            .contains("us-central1-aiplatform.googleapis.com"));
        assert!(cfg.retrieve_contexts_url().ends_with(":retrieveContexts"));
    }

    #[test]
    fn builds_request_body() {
        let body = build_retrieve_body(
            "hello",
            &["projects/p/locations/l/ragCorpora/c".into()],
            Some(7),
            Some(0.5),
        );
        assert_eq!(body["query"]["text"], "hello");
        assert_eq!(body["query"]["ragRetrievalConfig"]["topK"], 7);
        assert_eq!(
            body["vertexRagStore"]["ragResources"][0]["ragCorpus"],
            "projects/p/locations/l/ragCorpora/c"
        );
        assert_eq!(body["vertexRagStore"]["vectorDistanceThreshold"], 0.5);
    }

    #[test]
    fn builds_request_body_minimal() {
        let body = build_retrieve_body("q", &["c".into()], None, None);
        assert_eq!(body["query"]["text"], "q");
        // No top_k → no ragRetrievalConfig.
        assert!(body["query"].get("ragRetrievalConfig").is_none());
        assert!(body["vertexRagStore"].get("vectorDistanceThreshold").is_none());
    }

    #[test]
    fn maps_contexts_with_distance() {
        let resp = RetrieveContextsResponse {
            contexts: RagContexts {
                contexts: vec![RagContext {
                    text: "chunk text".into(),
                    source_uri: "gs://bucket/file.txt".into(),
                    source_display_name: "adk-memory-v1.abc".into(),
                    distance: Some(0.0),
                    score: None,
                }],
            },
        };
        let results = map_contexts(resp);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "chunk text");
        assert_eq!(results[0].source, "gs://bucket/file.txt");
        assert!((results[0].score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn missing_token_errors() {
        let tool = VertexAiRagRetrievalTool::new(test_config());
        let err = tool.token_provider.get().unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
        assert!(err.to_string().contains("missing auth token"));
    }

    #[tokio::test]
    async fn retrieve_without_token_errors() {
        let tool = VertexAiRagRetrievalTool::new(test_config());
        let result = tool.retrieve("test query", 5).await;
        assert!(result.is_err());
    }
}
