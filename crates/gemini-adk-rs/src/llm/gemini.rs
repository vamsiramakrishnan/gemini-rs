//! Concrete Gemini LLM implementation using gemini-live `Client`.
//!
//! The [`GeminiLlm`] struct is always available for type references and registry
//! wiring. Actual HTTP generation requires the `gemini-llm` feature flag, which
//! pulls in `gemini-live/http` and `gemini-live/generate`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use std::sync::LazyLock;

#[cfg(feature = "gemini-llm")]
use crate::llm::TokenUsage;
use crate::llm::{
    BaseLlm, EnvTokenProvider, GcloudTokenProvider, LlmError, LlmRequest, LlmResponse,
    TokenProvider,
};
use crate::utils::variant::{GoogleLlmVariant, get_google_llm_variant};

/// Parameters for constructing a [`GeminiLlm`].
#[derive(Default)]
pub struct GeminiLlmParams {
    /// Model name. Defaults to the `GEMINI_MODEL` env var if set, else
    /// `gemini-flash-latest` on Google AI (a rolling alias the catalog
    /// keeps serving) or `gemini-2.5-flash` on Vertex AI.
    pub model: Option<String>,
    /// API key for Gemini API (non-Vertex).
    pub api_key: Option<String>,
    /// Whether to use Vertex AI backend.
    pub vertexai: Option<bool>,
    /// Google Cloud project ID (Vertex AI only).
    pub project: Option<String>,
    /// Google Cloud region (Vertex AI only, defaults to "us-central1").
    pub location: Option<String>,
    /// Custom HTTP headers for requests.
    pub headers: Option<HashMap<String, String>>,
    /// Custom token provider for VertexAI. Defaults to reading `GOOGLE_ACCESS_TOKEN` env var.
    pub token_provider: Option<Arc<dyn TokenProvider>>,
}

/// Concrete Gemini LLM implementation using gemini-live `Client`.
///
/// The gemini-live `Client` is created once at construction time and reused for
/// all `generate()` calls, matching the JS GenAI SDK pattern where a single
/// `GoogleGenAI` instance is shared across requests.
pub struct GeminiLlm {
    model: String,
    variant: GoogleLlmVariant,
    /// Stored for constructing the gemini-live `Client` when `gemini-llm` is enabled.
    #[allow(dead_code)]
    params: GeminiLlmParams,
    /// Token provider for VertexAI token refresh.
    #[allow(dead_code)]
    token_provider: Arc<dyn TokenProvider>,
    /// Cached gemini-live Client, created once at construction time.
    #[cfg(feature = "gemini-llm")]
    client: gemini_genai_rs::Client,
}

/// The name the API uses for an enum value (`"MAX_TOKENS"`, not `MaxTokens`).
#[cfg(feature = "gemini-llm")]
fn wire_name(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Keep what the transport knew: the status, whether credentials failed,
/// whether the provider was reachable at all.
#[cfg(feature = "gemini-llm")]
fn llm_error(error: gemini_genai_rs::generate::GenerateError) -> LlmError {
    use gemini_genai_rs::client::http::HttpError;
    use gemini_genai_rs::generate::GenerateError;

    match error {
        GenerateError::Http(HttpError::ApiError {
            status, message, ..
        }) => LlmError::Api { status, message },
        GenerateError::Http(HttpError::Request(e)) => LlmError::Transport(e.to_string()),
        GenerateError::Http(e @ HttpError::RetriesExhausted { .. }) => {
            LlmError::Transport(e.to_string())
        }
        GenerateError::Http(HttpError::Auth(e)) | GenerateError::Auth(e) => {
            LlmError::Auth(e.to_string())
        }
        GenerateError::SafetyBlocked { reason } | GenerateError::PromptBlocked { reason } => {
            LlmError::ContentFiltered(wire_name(&reason))
        }
        other => LlmError::RequestFailed(other.to_string()),
    }
}

static SUPPORTED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"^gemini-.*$").unwrap(),
        Regex::new(r"^projects/.*/endpoints/.*$").unwrap(),
        Regex::new(r"^projects/.*/models/gemini.*$").unwrap(),
    ]
});

impl GeminiLlm {
    /// Create a new `GeminiLlm` from parameters.
    ///
    /// Resolves defaults for model, variant, API key, project, and location
    /// from parameters first, then falls back to environment variables.
    /// The gemini-live `Client` is created once here and reused for all calls.
    pub fn new(mut params: GeminiLlmParams) -> Self {
        // Resolve variant from params or env
        let variant = if let Some(true) = params.vertexai {
            GoogleLlmVariant::VertexAi
        } else if let Some(false) = params.vertexai {
            GoogleLlmVariant::GeminiApi
        } else {
            get_google_llm_variant()
        };

        // Resolve model: params, then GEMINI_TEXT_MODEL, then the shared
        // GEMINI_MODEL, then a per-variant default. Google AI retires dated
        // names but serves the rolling `gemini-flash-latest` alias; Vertex AI
        // keeps versioned GA names and does not carry the alias.
        let model = params
            .model
            .clone()
            .or_else(|| {
                ["GEMINI_TEXT_MODEL", "GEMINI_MODEL"]
                    .iter()
                    .find_map(|k| std::env::var(k).ok())
                    .filter(|m| !m.trim().is_empty())
            })
            .unwrap_or_else(|| match variant {
                GoogleLlmVariant::GeminiApi => "gemini-flash-latest".to_string(),
                GoogleLlmVariant::VertexAi => "gemini-2.5-flash".to_string(),
            });
        if model.contains("native-audio") || model.contains("-live-") {
            tracing::warn!(
                model = %model,
                "GeminiLlm resolved a Live (bidi) model name for generateContent; set \
                 GEMINI_TEXT_MODEL (or GeminiLlmParams::model) to a text model — a shared \
                 GEMINI_MODEL pointing at the Live model 404s here"
            );
        }

        // Resolve API key from params or env
        if params.api_key.is_none() && variant == GoogleLlmVariant::GeminiApi {
            // Same acceptance chain as the Live connect path, so one exported
            // variable works for both halves of the stack.
            params.api_key = std::env::var("GOOGLE_GENAI_API_KEY")
                .or_else(|_| std::env::var("GEMINI_API_KEY"))
                .or_else(|_| std::env::var("GOOGLE_API_KEY"))
                .ok();
        }

        // Resolve project/location from env for Vertex AI
        if variant == GoogleLlmVariant::VertexAi {
            if params.project.is_none() {
                params.project = std::env::var("GOOGLE_CLOUD_PROJECT").ok();
            }
            if params.location.is_none() {
                params.location = std::env::var("GOOGLE_CLOUD_LOCATION").ok();
            }
        }

        // Resolve token provider for VertexAI.
        // Default to GcloudTokenProvider (env var -> gcloud CLI fallback) for VertexAI,
        // matching the auth resolution in build_session_config(). For GeminiApi, use
        // EnvTokenProvider since API key auth doesn't need token refresh.
        let token_provider: Arc<dyn TokenProvider> =
            params.token_provider.take().unwrap_or_else(|| {
                if variant == GoogleLlmVariant::VertexAi {
                    Arc::new(GcloudTokenProvider::new(std::time::Duration::from_secs(
                        45 * 60,
                    )))
                } else {
                    Arc::new(EnvTokenProvider)
                }
            });

        // Create the gemini-live Client once, reuse across generate() calls.
        // For VertexAI, use from_vertex_refreshable() so the token is dynamically
        // refreshed on every REST API call (via auth_headers()), preventing 401
        // errors from stale tokens during long-running sessions.
        #[cfg(feature = "gemini-llm")]
        let client = {
            use gemini_genai_rs::{Client, prelude::ModelId};
            match variant {
                GoogleLlmVariant::GeminiApi => {
                    let api_key = params.api_key.as_deref().unwrap_or("");
                    Client::from_api_key(api_key).model(ModelId::new(model.clone()))
                }
                GoogleLlmVariant::VertexAi => {
                    let project = params.project.as_deref().unwrap_or("").to_string();
                    let location = params
                        .location
                        .as_deref()
                        .unwrap_or("us-central1")
                        .to_string();
                    let tp = token_provider.clone();
                    Client::from_vertex_refreshable(project, location, move || tp.token())
                        .model(ModelId::new(model.clone()))
                }
            }
        };

        Self {
            model,
            variant,
            params,
            token_provider,
            #[cfg(feature = "gemini-llm")]
            client,
        }
    }

    /// A `GeminiLlm` configured from the environment, checked before any
    /// request is made.
    ///
    /// Reads the same variables as [`new`](Self::new) —
    /// `GEMINI_API_KEY` (or `GOOGLE_GENAI_API_KEY`, `GOOGLE_API_KEY`) for Google
    /// AI; `GOOGLE_GENAI_USE_VERTEXAI=true` with `GOOGLE_CLOUD_PROJECT` and
    /// optionally `GOOGLE_CLOUD_LOCATION` for Vertex AI; `GEMINI_TEXT_MODEL` or
    /// `GEMINI_MODEL` for the model — and fails with a message naming the
    /// variable to set when one is missing, or when the model is a Live model
    /// the text API does not serve. [`new`](Self::new) accepts the same
    /// configuration and fails on the first request instead.
    ///
    /// ```no_run
    /// use gemini_adk_rs::llm::GeminiLlm;
    ///
    /// let llm = GeminiLlm::from_env()?;
    /// # Ok::<(), gemini_adk_rs::llm::LlmError>(())
    /// ```
    pub fn from_env() -> Result<Self, LlmError> {
        Self::try_new(GeminiLlmParams::default())
    }

    /// Like [`new`](Self::new), but checks the resolved configuration first;
    /// see [`from_env`](Self::from_env).
    pub fn try_new(params: GeminiLlmParams) -> Result<Self, LlmError> {
        let llm = Self::new(params);
        llm.check()?;
        Ok(llm)
    }

    /// What would make every request fail, found before sending one.
    fn check(&self) -> Result<(), LlmError> {
        if super::ModelCapabilities::infer_from_id(&self.model).live_bidi {
            return Err(LlmError::Config(format!(
                "`{}` is a Live model, which the text API does not serve. Set \
                 GEMINI_TEXT_MODEL (or `GeminiLlmParams::model`) to a text model such as \
                 `gemini-flash-latest`; keep GEMINI_MODEL for the Live session",
                self.model
            )));
        }
        match self.variant {
            GoogleLlmVariant::GeminiApi
                if self
                    .params
                    .api_key
                    .as_deref()
                    .is_none_or(|k| k.trim().is_empty()) =>
            {
                Err(LlmError::Auth(
                    "no Gemini API key. Set GEMINI_API_KEY (create one at \
                     https://aistudio.google.com/apikey), or use Vertex AI with \
                     GOOGLE_GENAI_USE_VERTEXAI=true and GOOGLE_CLOUD_PROJECT"
                        .into(),
                ))
            }
            GoogleLlmVariant::VertexAi
                if self
                    .params
                    .project
                    .as_deref()
                    .is_none_or(|p| p.trim().is_empty()) =>
            {
                Err(LlmError::Config(
                    "Vertex AI needs a project. Set GOOGLE_CLOUD_PROJECT (and optionally \
                     GOOGLE_CLOUD_LOCATION), or unset GOOGLE_GENAI_USE_VERTEXAI to use a \
                     Gemini API key"
                        .into(),
                ))
            }
            _ => Ok(()),
        }
    }

    /// Turn a wire response into an [`LlmResponse`], or the error it carries:
    /// a blocked prompt or a reply withheld by content safety is an error, not
    /// an empty answer.
    #[cfg(feature = "gemini-llm")]
    fn from_generate_response(
        response: gemini_genai_rs::generate::GenerateContentResponse,
    ) -> Result<LlmResponse, LlmError> {
        use gemini_genai_rs::prelude::{Content, FinishReason, Role};

        if let Some(reason) = response
            .prompt_feedback
            .as_ref()
            .and_then(|f| f.block_reason)
        {
            return Err(LlmError::ContentFiltered(wire_name(&reason)));
        }
        let candidate = response.candidates.into_iter().next();
        let finish = candidate.as_ref().and_then(|c| c.finish_reason);
        let content = candidate.and_then(|c| c.content).unwrap_or(Content {
            role: Some(Role::Model),
            parts: vec![],
        });
        if content.parts.is_empty()
            && let Some(
                reason @ (FinishReason::Safety
                | FinishReason::Recitation
                | FinishReason::Blocklist
                | FinishReason::ProhibitedContent
                | FinishReason::Spii),
            ) = finish
        {
            return Err(LlmError::ContentFiltered(wire_name(&reason)));
        }

        // Thinking tokens are billed as output, so they count as completion.
        let usage = response.usage_metadata.map(|u| {
            let prompt = u.prompt_token_count.unwrap_or(0);
            let completion = u
                .response_token_count
                .unwrap_or(0)
                .saturating_add(u.thoughts_token_count.unwrap_or(0));
            TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: u
                    .total_token_count
                    .unwrap_or(prompt.saturating_add(completion)),
            }
        });

        Ok(LlmResponse {
            content,
            finish_reason: finish.map(|r| wire_name(&r)),
            usage,
        })
    }

    /// Check if a model name is supported by `GeminiLlm`.
    pub fn is_supported(model: &str) -> bool {
        SUPPORTED_PATTERNS.iter().any(|re| re.is_match(model))
    }

    /// Get the variant (VertexAI vs GeminiApi).
    pub fn variant(&self) -> GoogleLlmVariant {
        self.variant
    }

    /// Map every field of an [`LlmRequest`] onto the wire request, and the
    /// per-request model override onto the model to call.
    #[cfg(feature = "gemini-llm")]
    fn to_generate_config(
        mut request: LlmRequest,
    ) -> (
        gemini_genai_rs::generate::GenerateContentConfig,
        Option<gemini_genai_rs::prelude::ModelId>,
    ) {
        use gemini_genai_rs::generate::GenerateContentConfig;
        use gemini_genai_rs::prelude::{GenerationConfig, ModelId, ThinkingConfig};

        let mut config = if request.contents.is_empty() {
            GenerateContentConfig::from_text("")
        } else {
            GenerateContentConfig::from_contents(std::mem::take(&mut request.contents))
        };
        if let Some(sys) = request.system_instruction.take() {
            config = config.system_instruction(&sys);
        }
        config.tools = std::mem::take(&mut request.tools);

        let generation = GenerationConfig {
            temperature: request.temperature,
            max_output_tokens: request.max_output_tokens,
            top_p: request.top_p,
            top_k: request.top_k,
            stop_sequences: (!request.stop_sequences.is_empty())
                .then(|| std::mem::take(&mut request.stop_sequences)),
            thinking_config: request.thinking_budget.map(|budget| ThinkingConfig {
                thinking_budget: Some(budget),
                include_thoughts: None,
            }),
            response_mime_type: request.response_mime_type.take(),
            response_json_schema: request.response_json_schema.take(),
            ..GenerationConfig::default()
        };
        config.generation_config =
            (generation != GenerationConfig::default()).then_some(generation);

        (config, request.model.take().map(ModelId::new))
    }
}

#[async_trait]
impl BaseLlm for GeminiLlm {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        // Feature-gate the actual HTTP call behind gemini-live's generate + http features.
        #[cfg(feature = "gemini-llm")]
        {
            let (config, model) = Self::to_generate_config(request);

            let response = self
                .client
                .generate_content_with(config, model.as_ref())
                .await
                .map_err(llm_error)?;
            Self::from_generate_response(response)
        }

        #[cfg(not(feature = "gemini-llm"))]
        {
            // Suppress unused-variable warnings when the feature is disabled.
            let _ = request;
            Err(LlmError::RequestFailed(
                "GeminiLlm requires the 'gemini-llm' feature flag \
                 (depends on gemini-live HTTP client)"
                    .into(),
            ))
        }
    }

    /// Pre-warm the HTTP connection pool by making a lightweight request.
    ///
    /// Establishes the TCP+TLS connection so the first real `generate()`
    /// call doesn't pay the ~100-300ms handshake penalty. reqwest's
    /// connection pool keeps it alive for subsequent calls.
    async fn warm_up(&self) -> Result<(), LlmError> {
        #[cfg(feature = "gemini-llm")]
        {
            use gemini_genai_rs::generate::GenerateContentConfig;
            let config = GenerateContentConfig::from_text(".").max_output_tokens(1);
            let _ = self.client.generate_content_with(config, None).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every setting an agent can make must reach the wire body; a field that
    /// is accepted and dropped is the bug this guards against.
    #[cfg(feature = "gemini-llm")]
    #[test]
    fn every_request_field_reaches_the_wire() {
        let request = LlmRequest {
            model: Some("gemini-2.5-pro".into()),
            system_instruction: Some("Be brief.".into()),
            tools: vec![gemini_genai_rs::prelude::Tool::google_search()],
            temperature: Some(0.2),
            max_output_tokens: Some(64),
            top_p: Some(0.9),
            top_k: Some(20),
            stop_sequences: vec!["END".into()],
            thinking_budget: Some(512),
            response_mime_type: Some("application/json".into()),
            response_json_schema: Some(serde_json::json!({ "type": "object" })),
            ..LlmRequest::from_text("hi")
        };
        let (config, model) = GeminiLlm::to_generate_config(request);
        assert_eq!(
            model
                .as_ref()
                .map(gemini_genai_rs::prelude::ModelId::as_str),
            Some("gemini-2.5-pro")
        );

        let body = config.to_request_body();
        let gc = &body["generationConfig"];
        assert_eq!(gc["temperature"], 0.2_f32 as f64);
        assert_eq!(gc["maxOutputTokens"], 64);
        assert_eq!(gc["topP"], 0.9_f32 as f64);
        assert_eq!(gc["topK"], 20);
        assert_eq!(gc["stopSequences"], serde_json::json!(["END"]));
        assert_eq!(gc["thinkingConfig"]["thinkingBudget"], 512);
        assert_eq!(gc["responseMimeType"], "application/json");
        assert_eq!(gc["responseJsonSchema"]["type"], "object");
        assert!(body["tools"][0].get("googleSearch").is_some(), "{body}");
        assert!(body["systemInstruction"].is_object(), "{body}");
    }

    #[cfg(feature = "gemini-llm")]
    fn wire(json: serde_json::Value) -> Result<LlmResponse, LlmError> {
        GeminiLlm::from_generate_response(serde_json::from_value(json).unwrap())
    }

    /// A blocked prompt used to come back as an empty answer.
    #[cfg(feature = "gemini-llm")]
    #[test]
    fn a_blocked_prompt_is_an_error_with_its_reason() {
        let err =
            wire(serde_json::json!({ "promptFeedback": { "blockReason": "SAFETY" } })).unwrap_err();
        assert!(
            matches!(&err, LlmError::ContentFiltered(r) if r == "SAFETY"),
            "{err}"
        );
    }

    #[cfg(feature = "gemini-llm")]
    #[test]
    fn a_withheld_reply_is_an_error_but_a_truncated_one_is_not() {
        let withheld = wire(serde_json::json!({
            "candidates": [{ "finishReason": "PROHIBITED_CONTENT" }]
        }));
        assert!(matches!(withheld, Err(LlmError::ContentFiltered(r)) if r == "PROHIBITED_CONTENT"));

        let truncated = wire(serde_json::json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "Once upon" }] },
                "finishReason": "MAX_TOKENS"
            }]
        }))
        .unwrap();
        assert_eq!(truncated.text(), "Once upon");
        assert_eq!(truncated.finish_reason.as_deref(), Some("MAX_TOKENS"));
    }

    /// `generateContent` names the output count `candidatesTokenCount`; it was
    /// read as zero. Thinking tokens are billed as output.
    #[cfg(feature = "gemini-llm")]
    #[test]
    fn usage_reads_the_rest_field_names() {
        let response = wire(serde_json::json!({
            "candidates": [{ "content": { "role": "model", "parts": [{ "text": "hi" }] } }],
            "usageMetadata": {
                "promptTokenCount": 7,
                "candidatesTokenCount": 3,
                "thoughtsTokenCount": 5,
                "totalTokenCount": 15
            }
        }))
        .unwrap();
        assert_eq!(
            response.usage,
            Some(TokenUsage {
                prompt_tokens: 7,
                completion_tokens: 8,
                total_tokens: 15,
            })
        );
    }

    #[cfg(feature = "gemini-llm")]
    #[test]
    fn transport_errors_keep_their_status() {
        use gemini_genai_rs::client::http::HttpError;
        let err = llm_error(gemini_genai_rs::generate::GenerateError::Http(
            HttpError::ApiError {
                status: 429,
                message: "Resource exhausted".into(),
                body: None,
            },
        ));
        assert!(err.is_rate_limited(), "{err}");
        assert!(err.to_string().contains("Resource exhausted"), "{err}");
    }

    #[test]
    fn check_names_the_missing_key() {
        let err = GeminiLlm::try_new(GeminiLlmParams {
            vertexai: Some(false),
            api_key: Some("  ".into()),
            model: Some("gemini-flash-latest".into()),
            ..Default::default()
        })
        .err()
        .expect("a blank key cannot work");
        assert!(err.is_auth(), "{err}");
        assert!(err.to_string().contains("GEMINI_API_KEY"), "{err}");
    }

    #[test]
    fn check_rejects_a_live_model_for_text() {
        let err = GeminiLlm::try_new(GeminiLlmParams {
            vertexai: Some(false),
            api_key: Some("key".into()),
            model: Some("gemini-2.5-flash-native-audio-preview-12-2025".into()),
            ..Default::default()
        })
        .err()
        .expect("a Live model cannot serve generateContent");
        assert!(err.to_string().contains("GEMINI_TEXT_MODEL"), "{err}");
    }

    #[test]
    fn check_needs_a_vertex_project() {
        let err = GeminiLlm::try_new(GeminiLlmParams {
            vertexai: Some(true),
            project: Some(String::new()),
            model: Some("gemini-2.5-flash".into()),
            ..Default::default()
        })
        .err()
        .expect("Vertex AI without a project cannot work");
        assert!(err.to_string().contains("GOOGLE_CLOUD_PROJECT"), "{err}");
    }

    #[test]
    fn check_accepts_a_complete_configuration() {
        assert!(
            GeminiLlm::try_new(GeminiLlmParams {
                vertexai: Some(false),
                api_key: Some("key".into()),
                model: Some("gemini-flash-latest".into()),
                ..Default::default()
            })
            .is_ok()
        );
    }

    /// A request with no settings sends no `generationConfig` at all.
    #[cfg(feature = "gemini-llm")]
    #[test]
    fn an_unconfigured_request_sends_no_generation_config() {
        let (config, model) = GeminiLlm::to_generate_config(LlmRequest::from_text("hi"));
        assert!(model.is_none());
        assert!(config.to_request_body().get("generationConfig").is_none());
    }

    #[test]
    fn default_model_is_the_rolling_flash_alias() {
        let llm = GeminiLlm::new(GeminiLlmParams {
            vertexai: Some(false),
            ..Default::default()
        });
        let expected = std::env::var("GEMINI_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "gemini-flash-latest".to_string());
        assert_eq!(llm.model_id(), expected);
    }

    #[test]
    fn default_model_on_vertex_is_versioned() {
        if std::env::var("GEMINI_MODEL").is_ok_and(|m| !m.trim().is_empty()) {
            return; // env override wins by design; nothing to assert here
        }
        let llm = GeminiLlm::new(GeminiLlmParams {
            vertexai: Some(true),
            ..Default::default()
        });
        assert_eq!(llm.model_id(), "gemini-2.5-flash");
    }

    #[test]
    fn explicit_model() {
        let llm = GeminiLlm::new(GeminiLlmParams {
            model: Some("gemini-2.0-pro".into()),
            ..Default::default()
        });
        assert_eq!(llm.model_id(), "gemini-2.0-pro");
    }

    #[test]
    fn variant_from_params_vertex() {
        let llm = GeminiLlm::new(GeminiLlmParams {
            vertexai: Some(true),
            ..Default::default()
        });
        assert_eq!(llm.variant(), GoogleLlmVariant::VertexAi);
    }

    #[test]
    fn variant_from_params_gemini_api() {
        let llm = GeminiLlm::new(GeminiLlmParams {
            vertexai: Some(false),
            ..Default::default()
        });
        assert_eq!(llm.variant(), GoogleLlmVariant::GeminiApi);
    }

    #[test]
    fn is_supported_gemini_models() {
        assert!(GeminiLlm::is_supported("gemini-2.5-flash"));
        assert!(GeminiLlm::is_supported("gemini-2.0-pro"));
        assert!(GeminiLlm::is_supported("gemini-1.5-pro-001"));
    }

    #[test]
    fn is_supported_non_gemini_models() {
        assert!(!GeminiLlm::is_supported("gpt-4"));
        assert!(!GeminiLlm::is_supported("claude-3-opus"));
        assert!(!GeminiLlm::is_supported("llama-3"));
    }

    #[test]
    fn is_supported_vertex_ai_resource_paths() {
        assert!(GeminiLlm::is_supported(
            "projects/my-project/endpoints/12345"
        ));
        assert!(GeminiLlm::is_supported(
            "projects/my-project/models/gemini-2.5-flash"
        ));
    }

    #[test]
    fn model_id_returns_correct_string() {
        let llm = GeminiLlm::new(GeminiLlmParams {
            model: Some("gemini-2.5-flash-preview-04-17".into()),
            ..Default::default()
        });
        assert_eq!(llm.model_id(), "gemini-2.5-flash-preview-04-17");
    }

    #[test]
    fn base_llm_is_object_safe() {
        fn _assert_object_safe(_: &dyn BaseLlm) {}
    }

    #[test]
    fn gemini_llm_is_send_sync() {
        fn _assert_send_sync<T: Send + Sync>() {}
        _assert_send_sync::<GeminiLlm>();
    }
}
