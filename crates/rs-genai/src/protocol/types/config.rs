//! Session configuration types: SpeechConfig, GenerationConfig, SessionConfig, etc.

use serde::{Deserialize, Serialize};

use super::content::{Content, Part};
use super::enums::{AudioFormat, GeminiModel, Modality, Sensitivity, Voice};
use super::tools::{Tool, ToolConfig};

// ---------------------------------------------------------------------------
// Session configuration (builder pattern)
// ---------------------------------------------------------------------------

/// Speech configuration for audio output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechConfig {
    /// Voice selection configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_config: Option<VoiceConfig>,
}

/// Voice configuration within speech config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConfig {
    /// Prebuilt voice selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prebuilt_voice_config: Option<PrebuiltVoiceConfig>,
}

/// Prebuilt voice selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrebuiltVoiceConfig {
    /// Name of the prebuilt voice (e.g. `"Puck"`, `"Kore"`).
    pub voice_name: String,
}

/// Input audio transcription configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputAudioTranscription {}

/// Output audio transcription configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputAudioTranscription {}

/// Controls how incoming audio interacts with model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityHandling {
    /// User speech interrupts model output (barge-in). Default.
    #[serde(rename = "START_OF_ACTIVITY_INTERRUPTS")]
    StartOfActivityInterrupts,
    /// Model continues speaking even during user speech.
    #[serde(rename = "NO_INTERRUPTION")]
    NoInterruption,
}

/// Controls which input counts toward a user's conversation turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnCoverage {
    /// Only speech/audio included in turn (VAD-filtered). Default.
    #[serde(rename = "TURN_INCLUDES_ONLY_ACTIVITY")]
    TurnIncludesOnlyActivity,
    /// All input including silence included in turn.
    #[serde(rename = "TURN_INCLUDES_ALL_INPUT")]
    TurnIncludesAllInput,
}

/// Server-side VAD configuration for the setup message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeInputConfig {
    /// Server-side VAD settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_activity_detection: Option<AutomaticActivityDetection>,
    /// How user speech interacts with model output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_handling: Option<ActivityHandling>,
    /// Which input counts toward a user turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_coverage: Option<TurnCoverage>,
}

/// Automatic activity detection (VAD) settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticActivityDetection {
    /// Whether automatic activity detection is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// Sensitivity for detecting speech onset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_of_speech_sensitivity: Option<Sensitivity>,
    /// Sensitivity for detecting end of speech.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_of_speech_sensitivity: Option<Sensitivity>,
    /// Milliseconds of audio to include before speech onset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_padding_ms: Option<u32>,
    /// Milliseconds of silence before end-of-speech is triggered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_duration_ms: Option<u32>,
}

/// Session resumption configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionConfig {
    /// Opaque handle from a previous session for transparent resume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

/// Context window compression configuration for long sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWindowCompressionConfig {
    /// Sliding window mechanism for context compression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sliding_window: Option<SlidingWindow>,
    /// Token threshold that triggers context window compression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_tokens: Option<u32>,
}

/// Sliding window configuration for context compression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlidingWindow {
    /// Target number of tokens for the sliding window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_tokens: Option<u32>,
}

/// Proactivity configuration — controls whether the model can initiate responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProactivityConfig {
    /// Whether proactive audio responses are enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proactive_audio: Option<bool>,
}

/// Token count breakdown by modality (text, audio, image, video).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModalityTokenCount {
    /// The modality (e.g., "TEXT", "AUDIO", "IMAGE", "VIDEO").
    #[serde(default)]
    pub modality: Option<String>,
    /// Token count for this modality.
    #[serde(default)]
    pub token_count: Option<u32>,
}

/// Usage metadata returned by the server on messages.
///
/// Contains token counts for the prompt, response, cached content,
/// tool use, and thinking, with optional per-modality breakdowns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    /// Number of tokens in the prompt.
    #[serde(default)]
    pub prompt_token_count: Option<u32>,
    /// Number of tokens in the cached portion of the prompt.
    #[serde(default)]
    pub cached_content_token_count: Option<u32>,
    /// Total number of tokens across all generated response candidates.
    #[serde(default)]
    pub response_token_count: Option<u32>,
    /// Number of tokens present in tool-use prompt(s).
    #[serde(default)]
    pub tool_use_prompt_token_count: Option<u32>,
    /// Number of tokens of thoughts for thinking models.
    #[serde(default)]
    pub thoughts_token_count: Option<u32>,
    /// Total token count for the generation request (prompt + response).
    #[serde(default)]
    pub total_token_count: Option<u32>,
    /// Per-modality breakdown of prompt tokens.
    #[serde(default)]
    pub prompt_tokens_details: Vec<ModalityTokenCount>,
    /// Per-modality breakdown of cached content tokens.
    #[serde(default)]
    pub cache_tokens_details: Vec<ModalityTokenCount>,
    /// Per-modality breakdown of response tokens.
    #[serde(default)]
    pub response_tokens_details: Vec<ModalityTokenCount>,
    /// Per-modality breakdown of tool-use prompt tokens.
    #[serde(default)]
    pub tool_use_prompt_tokens_details: Vec<ModalityTokenCount>,
}

/// Grounding metadata for server content with search results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingMetadata {
    /// Grounding chunks with source information.
    #[serde(default)]
    pub grounding_chunks: Vec<serde_json::Value>,
    /// Grounding supports linking content to sources.
    #[serde(default)]
    pub grounding_supports: Vec<serde_json::Value>,
    /// Web search queries used for grounding.
    #[serde(default)]
    pub web_search_queries: Vec<String>,
    /// Search entry point for rendering.
    #[serde(default)]
    pub search_entry_point: Option<serde_json::Value>,
}

/// URL context metadata for content sourced from URLs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlContextMetadata {
    /// URL-related metadata entries.
    #[serde(default)]
    pub url_metadata: Vec<serde_json::Value>,
}

/// Configuration for model thinking/reasoning (Gemini 2.5+).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    /// Token budget for thinking/reasoning steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// Whether to include the model's thought process in responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
}

/// Media resolution for image/video inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaResolution {
    /// Low resolution.
    Low,
    /// Medium resolution.
    Medium,
    /// High resolution.
    High,
}

/// Generation config sent in the setup message.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    /// Output modalities (Text, Audio, Image).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_modalities: Option<Vec<Modality>>,
    /// Speech/voice configuration for audio output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_config: Option<SpeechConfig>,
    /// Sampling temperature (0.0 = deterministic, 2.0 = max randomness).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Top-p (nucleus) sampling threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Top-k sampling: number of top tokens to consider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Maximum number of output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Thinking/reasoning configuration (Gemini 2.5+).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    /// Enable emotionally expressive dialog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_affective_dialog: Option<bool>,
    /// Resolution for image/video inputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<MediaResolution>,
    /// Random seed for deterministic generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
    /// MIME type for structured output (e.g., `"application/json"`, `"text/x.enum"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    /// JSON Schema for structured output. Requires `response_mime_type = "application/json"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_json_schema: Option<serde_json::Value>,
}

/// API endpoint selector — Google AI (direct), Google AI with OAuth token, or Vertex AI.
///
/// # Google AI with API Key (default)
///
/// Uses an API key passed as a query parameter. The WebSocket URL is
/// `wss://generativelanguage.googleapis.com/ws/...?key={api_key}` and model
/// URIs are `models/{model}`.
///
/// # Google AI with Access Token
///
/// Uses an OAuth2 access token (e.g. from `gcloud auth print-access-token`)
/// passed as a query parameter. Same endpoint as Google AI but with
/// `access_token` instead of `key`. This is the recommended approach when
/// using gcloud credentials without an API key.
///
/// # Vertex AI
///
/// Uses a regional endpoint with OAuth2 bearer-token authentication. The
/// WebSocket URL is
/// `wss://{location}-aiplatform.googleapis.com/ws/google.cloud.aiplatform.v1.LlmBidiService/BidiGenerateContent`
/// and model URIs are
/// `projects/{project}/locations/{location}/publishers/google/models/{model}`.
///
/// ```
/// # use rs_genai::protocol::types::{ApiEndpoint, VertexConfig};
/// let google_ai = ApiEndpoint::google_ai("MY_API_KEY");
/// let with_token = ApiEndpoint::google_ai_token("ya29.ACCESS_TOKEN");
/// let vertex = ApiEndpoint::vertex("my-project", "us-central1", "ACCESS_TOKEN");
/// ```
#[derive(Debug, Clone)]
pub enum ApiEndpoint {
    /// Google AI Studio -- API-key authentication.
    GoogleAI {
        /// The API key.
        api_key: String,
    },
    /// Google AI with OAuth2 access token (e.g. from gcloud).
    GoogleAIToken {
        /// The OAuth2 access token.
        access_token: String,
    },
    /// Vertex AI — project + location + OAuth2 bearer token.
    VertexAI(VertexConfig),
}

/// Configuration for connecting through Vertex AI.
#[derive(Debug, Clone)]
pub struct VertexConfig {
    /// Google Cloud project ID (e.g. `"my-project-123"`).
    pub project: String,
    /// Regional location (e.g. `"us-central1"`).
    pub location: String,
    /// OAuth2 access token obtained from `gcloud auth print-access-token`
    /// or a service-account token exchange.
    pub access_token: String,
    /// Optional API host override. Defaults to
    /// `{location}-aiplatform.googleapis.com`.
    pub api_host: Option<String>,
}

impl ApiEndpoint {
    /// Shorthand for Google AI endpoint with API key.
    pub fn google_ai(api_key: impl Into<String>) -> Self {
        Self::GoogleAI {
            api_key: api_key.into(),
        }
    }

    /// Google AI endpoint with an OAuth2 access token.
    ///
    /// Use this when authenticating with `gcloud auth print-access-token`
    /// or any other OAuth2 flow instead of an API key.
    pub fn google_ai_token(access_token: impl Into<String>) -> Self {
        Self::GoogleAIToken {
            access_token: access_token.into(),
        }
    }

    /// Shorthand for Vertex AI endpoint.
    pub fn vertex(
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self::VertexAI(VertexConfig {
            project: project.into(),
            location: location.into(),
            access_token: access_token.into(),
            api_host: None,
        })
    }

    /// Vertex AI endpoint with a custom API host (for private endpoints,
    /// VPC-SC, or testing).
    pub fn vertex_with_host(
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
        api_host: impl Into<String>,
    ) -> Self {
        Self::VertexAI(VertexConfig {
            project: project.into(),
            location: location.into(),
            access_token: access_token.into(),
            api_host: Some(api_host.into()),
        })
    }
}

/// Complete session configuration — the builder entrypoint.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// API endpoint and credentials (Google AI key or Vertex AI project/token).
    pub endpoint: ApiEndpoint,
    /// Which Gemini model to use.
    pub model: GeminiModel,
    /// Generation parameters (modalities, temperature, etc.).
    pub generation_config: GenerationConfig,
    /// System instruction content.
    pub system_instruction: Option<Content>,
    /// Tool declarations for function calling, search, etc.
    pub tools: Vec<Tool>,
    /// Tool usage configuration.
    pub tool_config: Option<ToolConfig>,
    /// Input audio transcription configuration.
    pub input_audio_transcription: Option<InputAudioTranscription>,
    /// Output audio transcription configuration.
    pub output_audio_transcription: Option<OutputAudioTranscription>,
    /// Realtime input configuration (VAD, activity handling).
    pub realtime_input_config: Option<RealtimeInputConfig>,
    /// Session resumption configuration.
    pub session_resumption: Option<SessionResumptionConfig>,
    /// Context window compression configuration.
    pub context_window_compression: Option<ContextWindowCompressionConfig>,
    /// Proactivity configuration.
    pub proactivity: Option<ProactivityConfig>,
    /// Audio format for input (default: PCM16).
    pub input_audio_format: AudioFormat,
    /// Audio format for output (default: PCM16).
    pub output_audio_format: AudioFormat,
    /// Input audio sample rate in Hz (default: 16000).
    pub input_sample_rate: u32,
    /// Output audio sample rate in Hz (default: 24000).
    pub output_sample_rate: u32,
}

impl SessionConfig {
    /// Create a new session configuration with a Google AI API key.
    ///
    /// This is the simplest way to get started. For Vertex AI, use
    /// [`SessionConfig::from_vertex`] or [`SessionConfig::from_endpoint`].
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::from_endpoint(ApiEndpoint::google_ai(api_key))
    }

    /// Create a session configuration with an OAuth2 access token.
    ///
    /// Uses the Google AI endpoint (`generativelanguage.googleapis.com`) with
    /// an access token instead of an API key. This is the recommended approach
    /// when using `gcloud auth print-access-token` credentials.
    ///
    /// ```rust
    /// # use rs_genai::protocol::types::SessionConfig;
    /// let config = SessionConfig::from_access_token("ya29.ACCESS_TOKEN");
    /// ```
    pub fn from_access_token(access_token: impl Into<String>) -> Self {
        Self::from_endpoint(ApiEndpoint::google_ai_token(access_token))
    }

    /// Create a session configuration for Vertex AI.
    ///
    /// Uses the regional Vertex AI endpoint (`{location}-aiplatform.googleapis.com`).
    /// For the global endpoint, consider using [`SessionConfig::from_access_token`]
    /// instead.
    ///
    /// ```rust
    /// # use rs_genai::protocol::types::SessionConfig;
    /// let config = SessionConfig::from_vertex(
    ///     "my-project-123",
    ///     "us-central1",
    ///     "ya29.ACCESS_TOKEN",
    /// );
    /// ```
    pub fn from_vertex(
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self::from_endpoint(ApiEndpoint::vertex(project, location, access_token))
    }

    /// Create a session configuration from an explicit [`ApiEndpoint`].
    pub fn from_endpoint(endpoint: ApiEndpoint) -> Self {
        Self {
            endpoint,
            model: GeminiModel::default(),
            generation_config: GenerationConfig {
                response_modalities: Some(vec![Modality::Audio]),
                ..Default::default()
            },
            system_instruction: None,
            tools: Vec::new(),
            tool_config: None,
            input_audio_transcription: None,
            output_audio_transcription: None,
            realtime_input_config: None,
            session_resumption: None,
            context_window_compression: None,
            proactivity: None,
            input_audio_format: AudioFormat::Pcm16,
            output_audio_format: AudioFormat::Pcm16,
            input_sample_rate: 16000,
            output_sample_rate: 24000,
        }
    }

    /// Set the Gemini model.
    pub fn model(mut self, model: GeminiModel) -> Self {
        self.model = model;
        self
    }

    /// Set the output voice.
    pub fn voice(mut self, voice: Voice) -> Self {
        let voice_name = match &voice {
            Voice::Aoede => "Aoede".to_string(),
            Voice::Charon => "Charon".to_string(),
            Voice::Fenrir => "Fenrir".to_string(),
            Voice::Kore => "Kore".to_string(),
            Voice::Puck => "Puck".to_string(),
            Voice::Custom(name) => name.clone(),
        };
        self.generation_config.speech_config = Some(SpeechConfig {
            voice_config: Some(VoiceConfig {
                prebuilt_voice_config: Some(PrebuiltVoiceConfig { voice_name }),
            }),
        });
        self
    }

    /// Set the system instruction.
    pub fn system_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.system_instruction = Some(Content {
            role: None,
            parts: vec![Part::text(instruction)],
        });
        self
    }

    /// Set response modalities.
    pub fn response_modalities(mut self, modalities: Vec<Modality>) -> Self {
        self.generation_config.response_modalities = Some(modalities);
        self
    }

    /// Configure for text-only mode (no audio output).
    pub fn text_only(mut self) -> Self {
        self.generation_config.response_modalities = Some(vec![Modality::Text]);
        self.generation_config.speech_config = None;
        self
    }

    /// Add a tool declaration.
    pub fn add_tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Enable URL context tool.
    pub fn with_url_context(mut self) -> Self {
        self.tools.push(Tool::url_context());
        self
    }

    /// Enable Google Search grounding.
    pub fn with_google_search(mut self) -> Self {
        self.tools.push(Tool::google_search());
        self
    }

    /// Enable code execution.
    pub fn with_code_execution(mut self) -> Self {
        self.tools.push(Tool::code_execution());
        self
    }

    /// Set tool configuration.
    pub fn tool_config(mut self, config: ToolConfig) -> Self {
        self.tool_config = Some(config);
        self
    }

    /// Enable input audio transcription.
    pub fn enable_input_transcription(mut self) -> Self {
        self.input_audio_transcription = Some(InputAudioTranscription {});
        self
    }

    /// Enable output audio transcription.
    pub fn enable_output_transcription(mut self) -> Self {
        self.output_audio_transcription = Some(OutputAudioTranscription {});
        self
    }

    /// Set the temperature for generation.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.generation_config.temperature = Some(temp);
        self
    }

    /// Configure server-side VAD.
    pub fn server_vad(mut self, detection: AutomaticActivityDetection) -> Self {
        let mut ric = self.realtime_input_config.unwrap_or(RealtimeInputConfig {
            automatic_activity_detection: None,
            activity_handling: None,
            turn_coverage: None,
        });
        ric.automatic_activity_detection = Some(detection);
        self.realtime_input_config = Some(ric);
        self
    }

    /// Set how incoming audio interacts with model output (barge-in behavior).
    pub fn activity_handling(mut self, handling: ActivityHandling) -> Self {
        let mut ric = self.realtime_input_config.unwrap_or(RealtimeInputConfig {
            automatic_activity_detection: None,
            activity_handling: None,
            turn_coverage: None,
        });
        ric.activity_handling = Some(handling);
        self.realtime_input_config = Some(ric);
        self
    }

    /// Set which input counts toward a user's conversation turn.
    pub fn turn_coverage(mut self, coverage: TurnCoverage) -> Self {
        let mut ric = self.realtime_input_config.unwrap_or(RealtimeInputConfig {
            automatic_activity_detection: None,
            activity_handling: None,
            turn_coverage: None,
        });
        ric.turn_coverage = Some(coverage);
        self.realtime_input_config = Some(ric);
        self
    }

    /// Enable session resumption.
    pub fn session_resumption(mut self, handle: Option<String>) -> Self {
        self.session_resumption = Some(SessionResumptionConfig { handle });
        self
    }

    /// Configure context window compression for long sessions.
    pub fn context_window_compression(mut self, target_tokens: u32) -> Self {
        let mut cwc = self
            .context_window_compression
            .unwrap_or(ContextWindowCompressionConfig {
                sliding_window: None,
                trigger_tokens: None,
            });
        cwc.sliding_window = Some(SlidingWindow {
            target_tokens: Some(target_tokens),
        });
        self.context_window_compression = Some(cwc);
        self
    }

    /// Set the token threshold that triggers context window compression.
    pub fn context_window_trigger_tokens(mut self, tokens: u32) -> Self {
        let mut cwc = self
            .context_window_compression
            .unwrap_or(ContextWindowCompressionConfig {
                sliding_window: None,
                trigger_tokens: None,
            });
        cwc.trigger_tokens = Some(tokens);
        self.context_window_compression = Some(cwc);
        self
    }

    /// Enable proactive model responses.
    pub fn proactive_audio(mut self, enabled: bool) -> Self {
        self.proactivity = Some(ProactivityConfig {
            proactive_audio: Some(enabled),
        });
        self
    }

    /// Enable thinking/reasoning with a token budget (Gemini 2.5+).
    pub fn thinking(mut self, budget: u32) -> Self {
        let mut tc = self
            .generation_config
            .thinking_config
            .unwrap_or(ThinkingConfig {
                thinking_budget: None,
                include_thoughts: None,
            });
        tc.thinking_budget = Some(budget);
        self.generation_config.thinking_config = Some(tc);
        self
    }

    /// Include the model's thought process in responses.
    pub fn include_thoughts(mut self) -> Self {
        let mut tc = self
            .generation_config
            .thinking_config
            .unwrap_or(ThinkingConfig {
                thinking_budget: None,
                include_thoughts: None,
            });
        tc.include_thoughts = Some(true);
        self.generation_config.thinking_config = Some(tc);
        self
    }

    /// Enable affective dialog (emotionally expressive responses).
    pub fn affective_dialog(mut self, enabled: bool) -> Self {
        self.generation_config.enable_affective_dialog = Some(enabled);
        self
    }

    /// Set the media resolution for image/video inputs.
    pub fn media_resolution(mut self, res: MediaResolution) -> Self {
        self.generation_config.media_resolution = Some(res);
        self
    }

    /// Set the random seed for deterministic generation.
    pub fn seed(mut self, seed: u32) -> Self {
        self.generation_config.seed = Some(seed);
        self
    }

    /// Set input audio format and sample rate.
    pub fn input_audio(mut self, format: AudioFormat, sample_rate: u32) -> Self {
        self.input_audio_format = format;
        self.input_sample_rate = sample_rate;
        self
    }

    /// Set output audio format and sample rate.
    pub fn output_audio(mut self, format: AudioFormat, sample_rate: u32) -> Self {
        self.output_audio_format = format;
        self.output_sample_rate = sample_rate;
        self
    }

    /// Build the WebSocket URL for connecting to the Gemini Live API.
    ///
    /// - **Google AI (key)**: `wss://generativelanguage.googleapis.com/ws/...?key={key}`
    /// - **Google AI (token)**: `wss://generativelanguage.googleapis.com/ws/...?access_token={token}`
    /// - **Vertex AI**: `wss://{location}-aiplatform.googleapis.com/ws/...` or
    ///   `wss://aiplatform.googleapis.com/ws/...` for global
    pub fn ws_url(&self) -> String {
        match &self.endpoint {
            ApiEndpoint::GoogleAI { api_key } => format!(
                "wss://generativelanguage.googleapis.com/ws/\
                 google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent\
                 ?key={}",
                api_key
            ),
            ApiEndpoint::GoogleAIToken { access_token } => format!(
                "wss://generativelanguage.googleapis.com/ws/\
                 google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent\
                 ?access_token={}",
                access_token
            ),
            ApiEndpoint::VertexAI(v) => {
                let host = v.api_host.as_deref().unwrap_or("");
                let host = if host.is_empty() {
                    // "global" uses `aiplatform.googleapis.com` (no prefix),
                    // regional uses `{location}-aiplatform.googleapis.com`.
                    if v.location == "global" {
                        "aiplatform.googleapis.com".to_string()
                    } else {
                        format!("{}-aiplatform.googleapis.com", v.location)
                    }
                } else {
                    host.to_string()
                };
                format!(
                    "wss://{host}/ws/\
                     google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
                )
            }
        }
    }

    /// Build the model URI used in the setup message.
    ///
    /// - **Google AI / Google AI Token**: `models/{model}`
    /// - **Vertex AI**: `projects/{project}/locations/{location}/publishers/google/models/{model}`
    pub fn model_uri(&self) -> String {
        match &self.endpoint {
            ApiEndpoint::GoogleAI { .. } | ApiEndpoint::GoogleAIToken { .. } => {
                self.model.to_string()
            }
            ApiEndpoint::VertexAI(v) => {
                // Strip the `models/` prefix from the Display representation
                let model_name = self.model.to_string();
                let bare = model_name.strip_prefix("models/").unwrap_or(&model_name);
                format!(
                    "projects/{}/locations/{}/publishers/google/models/{}",
                    v.project, v.location, bare
                )
            }
        }
    }

    /// Returns the bearer token when using Vertex AI, `None` for Google AI.
    ///
    /// Used by the transport layer to set the `Authorization` HTTP header
    /// during the WebSocket upgrade handshake. Google AI endpoints pass the
    /// token as a query parameter instead.
    pub fn bearer_token(&self) -> Option<&str> {
        match &self.endpoint {
            ApiEndpoint::GoogleAI { .. } | ApiEndpoint::GoogleAIToken { .. } => None,
            ApiEndpoint::VertexAI(v) => Some(&v.access_token),
        }
    }

    /// Returns `true` if this config targets Vertex AI.
    pub fn is_vertex(&self) -> bool {
        matches!(self.endpoint, ApiEndpoint::VertexAI(_))
    }

    /// Returns `true` if this config uses an access token (either GoogleAIToken or VertexAI).
    pub fn uses_access_token(&self) -> bool {
        matches!(
            self.endpoint,
            ApiEndpoint::GoogleAIToken { .. } | ApiEndpoint::VertexAI(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_config_builder() {
        let config = SessionConfig::new("test-key")
            .model(GeminiModel::Gemini2_0FlashLive)
            .voice(Voice::Kore)
            .system_instruction("Be helpful.")
            .temperature(0.7);

        assert!(
            matches!(config.endpoint, ApiEndpoint::GoogleAI { ref api_key } if api_key == "test-key")
        );
        assert_eq!(config.model, GeminiModel::Gemini2_0FlashLive);
        assert!(config.system_instruction.is_some());
        assert_eq!(config.generation_config.temperature, Some(0.7));
    }

    #[test]
    fn text_only_mode() {
        let config = SessionConfig::new("key").text_only();
        assert_eq!(
            config.generation_config.response_modalities,
            Some(vec![Modality::Text])
        );
        assert!(config.generation_config.speech_config.is_none());
    }

    #[test]
    fn ws_url_contains_key() {
        let config = SessionConfig::new("my-secret-key");
        let url = config.ws_url();
        assert!(url.starts_with("wss://"));
        assert!(url.contains("key=my-secret-key"));
    }

    // --- Vertex AI tests ---

    #[test]
    fn vertex_session_config() {
        let config = SessionConfig::from_vertex("my-project", "us-central1", "token123")
            .model(GeminiModel::GeminiLive2_5FlashNativeAudio);
        assert!(config.is_vertex());
        assert!(config.bearer_token() == Some("token123"));
    }

    #[test]
    fn vertex_ws_url_regional() {
        let config = SessionConfig::from_vertex("proj", "us-central1", "tok");
        let url = config.ws_url();
        assert_eq!(
            url,
            "wss://us-central1-aiplatform.googleapis.com/ws/\
             google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
        );
        assert!(!url.contains("key="));
    }

    #[test]
    fn vertex_ws_url_global() {
        let config = SessionConfig::from_vertex("proj", "global", "tok");
        let url = config.ws_url();
        assert_eq!(
            url,
            "wss://aiplatform.googleapis.com/ws/\
             google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
        );
    }

    #[test]
    fn vertex_ws_url_custom_host() {
        let config = SessionConfig::from_endpoint(ApiEndpoint::vertex_with_host(
            "proj",
            "europe-west4",
            "tok",
            "custom-endpoint.example.com",
        ));
        let url = config.ws_url();
        assert!(url.starts_with("wss://custom-endpoint.example.com/ws/"));
    }

    #[test]
    fn vertex_model_uri() {
        let config = SessionConfig::from_vertex("my-proj", "us-central1", "tok")
            .model(GeminiModel::Gemini2_0FlashLive);
        assert_eq!(
            config.model_uri(),
            "projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.0-flash-live-001"
        );
    }

    #[test]
    fn vertex_model_uri_custom_model() {
        let config = SessionConfig::from_vertex("proj", "asia-southeast1", "tok").model(
            GeminiModel::Custom("gemini-live-2.5-flash-native-audio".to_string()),
        );
        assert_eq!(
            config.model_uri(),
            "projects/proj/locations/asia-southeast1/publishers/google/models/gemini-live-2.5-flash-native-audio"
        );
    }

    #[test]
    fn google_ai_is_not_vertex() {
        let config = SessionConfig::new("key");
        assert!(!config.is_vertex());
        assert!(config.bearer_token().is_none());
    }

    #[test]
    fn google_ai_model_uri_unchanged() {
        let config = SessionConfig::new("key").model(GeminiModel::Gemini2_0FlashLive);
        assert_eq!(config.model_uri(), "models/gemini-2.0-flash-live-001");
    }

    #[test]
    fn session_config_convenience_tools() {
        let config = SessionConfig::new("key")
            .with_url_context()
            .with_google_search()
            .with_code_execution();
        assert_eq!(config.tools.len(), 3);
        let json = config.to_setup_json();
        assert!(json.contains("\"urlContext\""));
        assert!(json.contains("\"googleSearch\""));
        assert!(json.contains("\"codeExecution\""));
    }

    // ── GenerationConfig new fields tests ──

    #[test]
    fn thinking_config_serialization() {
        let config = SessionConfig::new("key").thinking(1024);
        let json = config.to_setup_json();
        assert!(json.contains("\"thinkingConfig\""));
        assert!(json.contains("\"thinkingBudget\""));
        assert!(json.contains("1024"));
    }

    #[test]
    fn affective_dialog_serialization() {
        let config = SessionConfig::new("key").affective_dialog(true);
        let json = config.to_setup_json();
        assert!(json.contains("\"enableAffectiveDialog\""));
        assert!(json.contains("true"));
    }

    #[test]
    fn seed_serialization() {
        let config = SessionConfig::new("key").seed(42);
        let json = config.to_setup_json();
        assert!(json.contains("\"seed\""));
        assert!(json.contains("42"));
    }

    #[test]
    fn media_resolution_serialization() {
        let config = SessionConfig::new("key").media_resolution(MediaResolution::High);
        let json = config.to_setup_json();
        assert!(json.contains("\"mediaResolution\""));
        assert!(json.contains("\"HIGH\""));
    }

    #[test]
    fn combined_new_generation_fields() {
        let config = SessionConfig::new("key")
            .thinking(2048)
            .affective_dialog(true)
            .seed(123)
            .media_resolution(MediaResolution::Medium);
        let json = config.to_setup_json();
        assert!(json.contains("\"thinkingConfig\""));
        assert!(json.contains("\"enableAffectiveDialog\""));
        assert!(json.contains("\"seed\""));
        assert!(json.contains("\"mediaResolution\""));
    }

    // ── ActivityHandling / TurnCoverage serialization tests ──

    #[test]
    fn activity_handling_serialization() {
        let interrupts = ActivityHandling::StartOfActivityInterrupts;
        let json = serde_json::to_string(&interrupts).unwrap();
        assert_eq!(json, "\"START_OF_ACTIVITY_INTERRUPTS\"");
        let parsed: ActivityHandling = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, interrupts);

        let no_int = ActivityHandling::NoInterruption;
        let json = serde_json::to_string(&no_int).unwrap();
        assert_eq!(json, "\"NO_INTERRUPTION\"");
        let parsed: ActivityHandling = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, no_int);
    }

    #[test]
    fn turn_coverage_serialization() {
        let only = TurnCoverage::TurnIncludesOnlyActivity;
        let json = serde_json::to_string(&only).unwrap();
        assert_eq!(json, "\"TURN_INCLUDES_ONLY_ACTIVITY\"");
        let parsed: TurnCoverage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, only);

        let all = TurnCoverage::TurnIncludesAllInput;
        let json = serde_json::to_string(&all).unwrap();
        assert_eq!(json, "\"TURN_INCLUDES_ALL_INPUT\"");
        let parsed: TurnCoverage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, all);
    }

    #[test]
    fn thinking_config_with_include_thoughts() {
        let config = SessionConfig::new("key").thinking(2048).include_thoughts();
        let json = config.to_setup_json();
        assert!(json.contains("\"thinkingBudget\""));
        assert!(json.contains("2048"));
        assert!(json.contains("\"includeThoughts\""));
        assert!(json.contains("true"));
    }
}
