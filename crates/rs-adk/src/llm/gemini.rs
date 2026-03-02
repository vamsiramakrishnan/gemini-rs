//! Concrete Gemini LLM implementation using rs-genai `Client`.
//!
//! The [`GeminiLlm`] struct is always available for type references and registry
//! wiring. Actual HTTP generation requires the `gemini-llm` feature flag, which
//! pulls in `rs-genai/http` and `rs-genai/generate`.

use std::collections::HashMap;

use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
#[cfg(feature = "gemini-llm")]
use crate::llm::TokenUsage;
use crate::utils::variant::{get_google_llm_variant, GoogleLlmVariant};

/// Parameters for constructing a [`GeminiLlm`].
#[derive(Debug, Clone, Default)]
pub struct GeminiLlmParams {
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub vertexai: Option<bool>,
    pub project: Option<String>,
    pub location: Option<String>,
    pub headers: Option<HashMap<String, String>>,
}

/// Concrete Gemini LLM implementation using rs-genai `Client`.
pub struct GeminiLlm {
    model: String,
    variant: GoogleLlmVariant,
    /// Stored for constructing the rs-genai `Client` when `gemini-llm` is enabled.
    #[allow(dead_code)]
    params: GeminiLlmParams,
}

static SUPPORTED_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
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
    pub fn new(mut params: GeminiLlmParams) -> Self {
        // Resolve model (default to "gemini-2.5-flash")
        let model = params
            .model
            .clone()
            .unwrap_or_else(|| "gemini-2.5-flash".to_string());

        // Resolve variant from params or env
        let variant = if let Some(true) = params.vertexai {
            GoogleLlmVariant::VertexAi
        } else if let Some(false) = params.vertexai {
            GoogleLlmVariant::GeminiApi
        } else {
            get_google_llm_variant()
        };

        // Resolve API key from params or env
        if params.api_key.is_none() && variant == GoogleLlmVariant::GeminiApi {
            params.api_key = std::env::var("GOOGLE_GENAI_API_KEY")
                .or_else(|_| std::env::var("GEMINI_API_KEY"))
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

        Self {
            model,
            variant,
            params,
        }
    }

    /// Check if a model name is supported by `GeminiLlm`.
    pub fn is_supported(model: &str) -> bool {
        SUPPORTED_PATTERNS.iter().any(|re| re.is_match(model))
    }

    /// Get the variant (VertexAI vs GeminiApi).
    pub fn variant(&self) -> GoogleLlmVariant {
        self.variant
    }

    /// Preprocess request: remove labels and displayName for non-Vertex (Gemini API).
    fn preprocess_request(&self, _request: &mut LlmRequest) {
        // For Gemini API backend: remove labels and displayName from tools.
        // This is a no-op for now since LlmRequest doesn't have those fields yet.
        // In a full implementation, this would strip Vertex-only fields.
    }
}

#[async_trait]
impl BaseLlm for GeminiLlm {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn generate(&self, mut request: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.preprocess_request(&mut request);

        // Feature-gate the actual HTTP call behind rs-genai's generate + http features.
        #[cfg(feature = "gemini-llm")]
        {
            use rs_genai::generate::GenerateContentConfig;
            use rs_genai::prelude::*;

            let client = match self.variant {
                GoogleLlmVariant::GeminiApi => {
                    let api_key = self.params.api_key.as_deref().ok_or_else(|| {
                        LlmError::RequestFailed("No API key configured".into())
                    })?;
                    Client::from_api_key(api_key)
                        .model(GeminiModel::Custom(self.model.clone()))
                }
                GoogleLlmVariant::VertexAi => {
                    let project = self.params.project.as_deref().ok_or_else(|| {
                        LlmError::RequestFailed("No project configured".into())
                    })?;
                    let location = self
                        .params
                        .location
                        .as_deref()
                        .unwrap_or("us-central1");
                    // VertexAI requires an access token, typically obtained via
                    // application default credentials. For now, check env.
                    let token = std::env::var("GOOGLE_ACCESS_TOKEN").map_err(|_| {
                        LlmError::RequestFailed("No access token for VertexAI".into())
                    })?;
                    Client::from_vertex(project, location, token)
                        .model(GeminiModel::Custom(self.model.clone()))
                }
            };

            // Build GenerateContentConfig from LlmRequest
            let mut config = if request.contents.is_empty() {
                GenerateContentConfig::from_text("")
            } else {
                GenerateContentConfig::from_contents(request.contents.clone())
            };

            if let Some(sys) = &request.system_instruction {
                config = config.system_instruction(sys);
            }
            if !request.tools.is_empty() {
                config.tools = request.tools.clone();
            }
            if let Some(temp) = request.temperature {
                config = config.temperature(temp);
            }
            if let Some(max) = request.max_output_tokens {
                config = config.max_output_tokens(max);
            }

            let response = client
                .generate_content_with(config, None)
                .await
                .map_err(|e| LlmError::RequestFailed(e.to_string()))?;

            let content = response
                .candidates
                .first()
                .and_then(|c| c.content.clone())
                .unwrap_or_else(|| Content {
                    role: Some(Role::Model),
                    parts: vec![],
                });

            let finish_reason = response
                .candidates
                .first()
                .and_then(|c| c.finish_reason)
                .map(|r| format!("{:?}", r));

            let usage = response.usage_metadata.map(|u| TokenUsage {
                prompt_tokens: u.prompt_token_count.unwrap_or(0),
                completion_tokens: u.response_token_count.unwrap_or(0),
                total_tokens: u.total_token_count.unwrap_or(0),
            });

            Ok(LlmResponse {
                content,
                finish_reason,
                usage,
            })
        }

        #[cfg(not(feature = "gemini-llm"))]
        {
            // Suppress unused-variable warnings when the feature is disabled.
            let _ = request;
            Err(LlmError::RequestFailed(
                "GeminiLlm requires the 'gemini-llm' feature flag \
                 (depends on rs-genai HTTP client)"
                    .into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_is_gemini_2_5_flash() {
        let llm = GeminiLlm::new(GeminiLlmParams::default());
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
