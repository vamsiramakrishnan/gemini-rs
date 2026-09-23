//! LLM abstraction — decouples agents from specific model providers.
//!
//! The `BaseLlm` trait provides a unified interface for generating content
//! from any LLM. The `GeminiLlm` implementation wraps gemini-live's `Client`
//! for Gemini models.

pub mod gemini;
mod mock;
pub mod registry;

pub use gemini::{GeminiLlm, GeminiLlmParams};
pub use mock::MockLlm;
pub use registry::LlmRegistry;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use gemini_genai_rs::prelude::{Content, Part, Tool};

/// Provides access tokens for VertexAI authentication.
///
/// Implement this trait to supply dynamically refreshed tokens.
/// The default implementation reads `GOOGLE_ACCESS_TOKEN` from the environment.
pub trait TokenProvider: Send + Sync {
    /// Return a valid access token. Called before each `generate()` request
    /// when using VertexAI variant.
    fn token(&self) -> String;
}

/// Default token provider — reads `GOOGLE_ACCESS_TOKEN` from environment.
pub struct EnvTokenProvider;

impl TokenProvider for EnvTokenProvider {
    fn token(&self) -> String {
        std::env::var("GOOGLE_ACCESS_TOKEN").unwrap_or_default()
    }
}

/// Token provider that shells out to `gcloud auth print-access-token`,
/// caching the result with a configurable TTL.
pub struct GcloudTokenProvider {
    cache: parking_lot::Mutex<(String, std::time::Instant)>,
    ttl: std::time::Duration,
}

impl GcloudTokenProvider {
    /// Create a new provider with the given cache TTL (recommended: 45 minutes).
    pub fn new(ttl: std::time::Duration) -> Self {
        Self {
            cache: parking_lot::Mutex::new((String::new(), std::time::Instant::now())),
            ttl,
        }
    }
}

impl TokenProvider for GcloudTokenProvider {
    fn token(&self) -> String {
        let mut guard = self.cache.lock();
        let (ref mut cached_token, ref mut fetched_at) = *guard;
        if !cached_token.is_empty() && fetched_at.elapsed() < self.ttl {
            return cached_token.clone();
        }
        // Shell out to gcloud
        match std::process::Command::new("gcloud")
            .args(["auth", "print-access-token"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                *cached_token = token.clone();
                *fetched_at = std::time::Instant::now();
                token
            }
            _ => {
                // Fall back to env var
                std::env::var("GOOGLE_ACCESS_TOKEN").unwrap_or_default()
            }
        }
    }
}

/// Configuration for an LLM generation request.
///
/// Every field a provider can honour is here, and [`GeminiLlm`] sends every
/// one of them. New fields may be added; build requests with
/// `..Default::default()` or [`LlmRequest::from_text`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmRequest {
    /// The messages/contents to send.
    pub contents: Vec<Content>,
    /// The model for this request, overriding the provider's default model
    /// (e.g. `"gemini-2.5-pro"`). `None` uses [`BaseLlm::model_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// System instruction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<String>,
    /// Available tools.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<Tool>,
    /// Temperature for generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Nucleus sampling threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Number of highest-probability tokens to sample from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Strings that end generation when produced.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stop_sequences: Vec<String>,
    /// Token budget for the model's thinking, on models that think.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// MIME type for structured output (e.g., `"application/json"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    /// JSON Schema for structured output. Requires `response_mime_type = "application/json"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_json_schema: Option<serde_json::Value>,
}

impl LlmRequest {
    /// Create a request from a single user message.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            contents: vec![Content {
                role: Some(gemini_genai_rs::prelude::Role::User),
                parts: vec![Part::Text { text: text.into() }],
            }],
            ..Default::default()
        }
    }

    /// Create a request from existing contents.
    pub fn from_contents(contents: Vec<Content>) -> Self {
        Self {
            contents,
            ..Default::default()
        }
    }
}

/// The response from an LLM generation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    /// The generated content.
    pub content: Content,
    /// Finish reason (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Token usage (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

impl LlmResponse {
    /// A model turn that says `text` and finishes.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self::from_parts(vec![Part::Text { text: text.into() }], Some("STOP"))
    }

    /// A model turn that calls one tool.
    pub fn tool_call(name: impl Into<String>, args: serde_json::Value) -> Self {
        Self::tool_calls([(name, args)])
    }

    /// A model turn that calls several tools at once, in order.
    pub fn tool_calls<N: Into<String>>(
        calls: impl IntoIterator<Item = (N, serde_json::Value)>,
    ) -> Self {
        let parts = calls
            .into_iter()
            .map(|(name, args)| Part::FunctionCall {
                function_call: gemini_genai_rs::prelude::FunctionCall {
                    name: name.into(),
                    args,
                    id: None,
                },
            })
            .collect();
        Self::from_parts(parts, None)
    }

    fn from_parts(parts: Vec<Part>, finish_reason: Option<&str>) -> Self {
        Self {
            content: Content {
                role: Some(gemini_genai_rs::prelude::Role::Model),
                parts,
            },
            finish_reason: finish_reason.map(str::to_owned),
            usage: None,
        }
    }

    /// Add a streamed chunk to this response: its parts are appended (adjacent
    /// text merged), and its finish reason and usage, when present, replace
    /// these — a provider's streamed usage is cumulative.
    pub fn append(&mut self, chunk: LlmResponse) {
        for part in chunk.content.parts {
            match (self.content.parts.last_mut(), part) {
                (Some(Part::Text { text }), Part::Text { text: more }) => text.push_str(&more),
                (_, part) => self.content.parts.push(part),
            }
        }
        if chunk.finish_reason.is_some() {
            self.finish_reason = chunk.finish_reason;
        }
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }
    }

    /// Attach token usage, as a provider reports it.
    pub fn with_usage(mut self, prompt_tokens: u32, completion_tokens: u32) -> Self {
        self.usage = Some(TokenUsage::new(prompt_tokens, completion_tokens));
        self
    }

    /// Extract text from the response, concatenating all text parts.
    pub fn text(&self) -> String {
        self.content
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract function calls from the response.
    pub fn function_calls(&self) -> Vec<&gemini_genai_rs::prelude::FunctionCall> {
        self.content
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::FunctionCall { function_call } => Some(function_call),
                _ => None,
            })
            .collect()
    }
}

/// Token usage statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input/prompt tokens.
    pub prompt_tokens: u32,
    /// Output/completion tokens.
    pub completion_tokens: u32,
    /// Total tokens.
    pub total_tokens: u32,
}

impl TokenUsage {
    /// Usage for one call; the total is the sum of the two.
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    }
}

impl std::ops::Add for TokenUsage {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            prompt_tokens: self.prompt_tokens.saturating_add(other.prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_add(other.completion_tokens),
            total_tokens: self.total_tokens.saturating_add(other.total_tokens),
        }
    }
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

/// Errors from LLM operations.
///
/// Errors keep what the provider said — the HTTP status, the reason content
/// was blocked — so a caller can decide what to do without parsing a message:
///
/// ```
/// use gemini_adk_rs::llm::LlmError;
///
/// let err = LlmError::Api { status: 429, message: "quota exceeded".into() };
/// assert!(err.is_rate_limited() && err.is_retryable());
/// assert_eq!(err.status(), Some(429));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LlmError {
    /// The provider answered with an error status.
    #[error("the model API returned HTTP {status}: {message}")]
    Api {
        /// The HTTP status code.
        status: u16,
        /// The provider's error message.
        message: String,
    },
    /// Credentials are missing or were rejected. The message says how to fix it.
    #[error("{0}")]
    Auth(String),
    /// The provider could not be reached: connection, TLS or timeout.
    #[error("could not reach the model API: {0}")]
    Transport(String),
    /// The client is configured in a way no request can succeed with. The
    /// message says what to change.
    #[error("{0}")]
    Config(String),
    /// The HTTP request to the LLM API failed for another reason.
    #[error("LLM request failed: {0}")]
    RequestFailed(String),
    /// The requested model is not available.
    #[error("Model not available: {0}")]
    ModelNotAvailable(String),
    /// The request was rate-limited by the provider.
    #[error("Rate limited")]
    RateLimited,
    /// The prompt or the reply was blocked by content safety; carries the
    /// provider's reason (e.g. `"SAFETY"`, `"PROHIBITED_CONTENT"`).
    #[error("blocked by content safety: {0}")]
    ContentFiltered(String),
    /// A catch-all for other LLM errors.
    #[error("{0}")]
    Other(String),
}

impl LlmError {
    /// The HTTP status the provider answered with, when there was one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            Self::RateLimited => Some(429),
            _ => None,
        }
    }

    /// The provider is rate-limiting or out of quota (HTTP 429).
    pub fn is_rate_limited(&self) -> bool {
        self.status() == Some(429)
    }

    /// Credentials are missing, invalid or lack permission (HTTP 401/403).
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Auth(_)) || matches!(self.status(), Some(401 | 403))
    }

    /// The prompt or reply was blocked by content safety.
    pub fn is_content_filtered(&self) -> bool {
        matches!(self, Self::ContentFiltered(_))
    }

    /// Trying the same request again later may succeed: rate limits, server
    /// errors (5xx) and transport failures. Auth, configuration, content
    /// safety and other client errors will fail the same way again.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_)) || matches!(self.status(), Some(429 | 500..=599))
    }
}

/// Capability declaration for a model — what callers may rely on without
/// probing. See [`BaseLlm::capabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelCapabilities {
    /// Accepts a thinking budget / emits thought summaries.
    pub thinking: bool,
    /// Serves bidiGenerateContent (Live) sessions.
    pub live_bidi: bool,
    /// Produces native audio output.
    pub audio_output: bool,
    /// Accepts inline image/media parts in requests (vision).
    pub vision_input: bool,
    /// Honors cached-content references.
    pub context_caching: bool,
}

impl ModelCapabilities {
    /// Conservative inference from a model id string. Known family
    /// substrings light up capabilities; unknown ids report text-only.
    pub fn infer_from_id(id: &str) -> Self {
        let id = id.to_ascii_lowercase();
        let gemini = id.contains("gemini");
        let live = id.contains("live") || id.contains("native-audio");
        Self {
            thinking: gemini && (id.contains("2.5") || id.contains("thinking")),
            live_bidi: live,
            audio_output: live || id.contains("tts"),
            vision_input: gemini,
            context_caching: gemini,
        }
    }
}

/// A reply streamed in chunks; see [`BaseLlm::generate_stream`].
pub type LlmStream = futures_util::stream::BoxStream<'static, Result<LlmResponse, LlmError>>;

/// Trait for LLM providers — decouples agents from specific models.
///
/// Implementations must be `Send + Sync` for use across async tasks.
#[async_trait]
pub trait BaseLlm: Send + Sync {
    /// The model identifier (e.g., "gemini-2.5-flash").
    fn model_id(&self) -> &str;

    /// What this model supports — the ADK model-capability-declaration
    /// pattern: the model states its capabilities instead of every caller
    /// re-inferring them from the id string. The default derives a
    /// conservative estimate from [`model_id`](Self::model_id); back-ends
    /// with authoritative knowledge should override.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::infer_from_id(self.model_id())
    }

    /// Generate content from the LLM.
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError>;

    /// Generate content as a stream of chunks, in the order the model
    /// produced them. [`LlmResponse::append`] folds them into the whole reply;
    /// usage, when a chunk carries it, is cumulative.
    ///
    /// The default yields [`generate`](Self::generate)'s reply as one chunk,
    /// so every provider supports it; `GeminiLlm` streams for real.
    async fn generate_stream(&self, request: LlmRequest) -> Result<LlmStream, LlmError> {
        let response = self.generate(request).await?;
        Ok(futures_util::stream::once(async move { Ok(response) }).boxed())
    }

    /// Pre-warm the HTTP connection pool to avoid cold-start latency.
    ///
    /// The default implementation is a no-op. `GeminiLlm` overrides this to
    /// establish the TCP+TLS connection so the first real `generate()` call
    /// doesn't pay the ~100-300ms handshake penalty.
    async fn warm_up(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

/// A shared model is a model, so `Arc<GeminiLlm>`, `Arc<dyn BaseLlm>` and a
/// bare `GeminiLlm` are interchangeable wherever a [`BaseLlm`] is accepted.
#[async_trait]
impl<L: BaseLlm + ?Sized> BaseLlm for std::sync::Arc<L> {
    fn model_id(&self) -> &str {
        (**self).model_id()
    }

    fn capabilities(&self) -> ModelCapabilities {
        (**self).capabilities()
    }

    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        (**self).generate(request).await
    }

    async fn generate_stream(&self, request: LlmRequest) -> Result<LlmStream, LlmError> {
        (**self).generate_stream(request).await
    }

    async fn warm_up(&self) -> Result<(), LlmError> {
        (**self).warm_up().await
    }
}

/// A boxed model is a model; see the `Arc` implementation.
#[async_trait]
impl<L: BaseLlm + ?Sized> BaseLlm for Box<L> {
    fn model_id(&self) -> &str {
        (**self).model_id()
    }

    fn capabilities(&self) -> ModelCapabilities {
        (**self).capabilities()
    }

    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        (**self).generate(request).await
    }

    async fn generate_stream(&self, request: LlmRequest) -> Result<LlmStream, LlmError> {
        (**self).generate_stream(request).await
    }

    async fn warm_up(&self) -> Result<(), LlmError> {
        (**self).warm_up().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_request_from_text() {
        let req = LlmRequest::from_text("Hello!");
        assert_eq!(req.contents.len(), 1);
        assert!(req.system_instruction.is_none());
        assert!(req.tools.is_empty());
    }

    #[test]
    fn llm_request_from_contents() {
        let contents = vec![Content {
            role: Some(gemini_genai_rs::prelude::Role::User),
            parts: vec![Part::Text {
                text: "Hello".into(),
            }],
        }];
        let req = LlmRequest::from_contents(contents);
        assert_eq!(req.contents.len(), 1);
    }

    #[test]
    fn llm_response_text() {
        let resp = LlmResponse {
            content: Content {
                role: Some(gemini_genai_rs::prelude::Role::Model),
                parts: vec![
                    Part::Text {
                        text: "Hello ".into(),
                    },
                    Part::Text {
                        text: "world!".into(),
                    },
                ],
            },
            finish_reason: Some("STOP".into()),
            usage: None,
        };
        assert_eq!(resp.text(), "Hello world!");
    }

    #[test]
    fn llm_response_function_calls() {
        let resp = LlmResponse {
            content: Content {
                role: Some(gemini_genai_rs::prelude::Role::Model),
                parts: vec![Part::FunctionCall {
                    function_call: gemini_genai_rs::prelude::FunctionCall {
                        name: "get_weather".into(),
                        args: serde_json::json!({"city": "London"}),
                        id: None,
                    },
                }],
            },
            finish_reason: None,
            usage: None,
        };
        let calls = resp.function_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
    }

    #[test]
    fn errors_classify_by_status_not_by_message() {
        let api = |status| LlmError::Api {
            status,
            message: String::new(),
        };
        assert!(api(429).is_rate_limited() && api(429).is_retryable());
        assert!(api(503).is_retryable() && !api(503).is_rate_limited());
        assert!(api(401).is_auth() && api(403).is_auth() && !api(401).is_retryable());
        assert!(!api(400).is_retryable() && !api(404).is_auth());
        assert!(LlmError::Transport("reset".into()).is_retryable());
        assert!(LlmError::RateLimited.is_rate_limited());
        assert!(LlmError::Auth("no key".into()).is_auth());
        assert!(LlmError::ContentFiltered("SAFETY".into()).is_content_filtered());
        assert_eq!(api(418).status(), Some(418));
        assert_eq!(LlmError::Other("x".into()).status(), None);
    }

    #[test]
    fn append_folds_streamed_chunks_into_one_reply() {
        let mut whole = LlmResponse::from_parts(vec![Part::Text { text: "Hel".into() }], None);
        whole.append(LlmResponse::from_parts(
            vec![Part::Text { text: "lo".into() }],
            None,
        ));
        whole.append(LlmResponse::tool_call("f", serde_json::json!({})).with_usage(3, 1));
        whole.append(LlmResponse::from_text("!").with_usage(3, 2));
        assert_eq!(whole.text(), "Hello!");
        assert_eq!(whole.function_calls().len(), 1);
        assert_eq!(whole.finish_reason.as_deref(), Some("STOP"));
        assert_eq!(
            whole.usage,
            Some(TokenUsage::new(3, 2)),
            "usage is cumulative, not summed"
        );
    }

    #[tokio::test]
    async fn every_model_can_stream() {
        let chunks: Vec<_> = MockLlm::text("one two three")
            .generate_stream(LlmRequest::from_text("x"))
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(chunks.len(), 3, "the mock streams one word at a time");
        let mut whole = chunks[0].clone();
        for chunk in chunks.into_iter().skip(1) {
            whole.append(chunk);
        }
        assert_eq!(whole.text(), "one two three");
    }

    #[test]
    fn base_llm_is_object_safe() {
        fn _assert(_: &dyn BaseLlm) {}
    }

    #[test]
    fn token_usage() {
        let usage = TokenUsage::new(10, 20);
        assert_eq!(usage.total_tokens, 30);
        assert_eq!((usage + TokenUsage::new(1, 2)).total_tokens, 33);
    }

    #[test]
    fn response_constructors_build_model_turns() {
        let said = LlmResponse::from_text("hi").with_usage(3, 4);
        assert_eq!(said.text(), "hi");
        assert_eq!(
            said.content.role,
            Some(gemini_genai_rs::prelude::Role::Model)
        );
        assert_eq!(said.finish_reason.as_deref(), Some("STOP"));
        assert_eq!(said.usage, Some(TokenUsage::new(3, 4)));

        let called = LlmResponse::tool_calls([
            ("a", serde_json::json!({})),
            ("b", serde_json::json!({ "x": 1 })),
        ]);
        let names: Vec<_> = called
            .function_calls()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["a", "b"]);
        assert!(called.text().is_empty());
    }

    /// `Arc<L>`, `Arc<dyn BaseLlm>` and `Box<L>` all satisfy a generic bound,
    /// so no call site needs to know which one it was handed.
    #[tokio::test]
    async fn shared_and_boxed_models_are_models() {
        async fn ask(llm: impl BaseLlm) -> String {
            llm.generate(LlmRequest::from_text("q"))
                .await
                .unwrap()
                .text()
        }
        let mock = MockLlm::text("a").with_model_id("m");
        let shared: std::sync::Arc<dyn BaseLlm> = std::sync::Arc::new(mock.clone());
        assert_eq!(shared.model_id(), "m");
        assert_eq!(ask(shared).await, "a");
        assert_eq!(ask(std::sync::Arc::new(mock.clone())).await, "a");
        assert_eq!(ask(Box::new(mock.clone()) as Box<dyn BaseLlm>).await, "a");
        assert_eq!(mock.call_count(), 3);
    }
}
