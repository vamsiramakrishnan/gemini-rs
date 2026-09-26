//! Session configuration types: SpeechConfig, GenerationConfig, SessionConfig, etc.

use serde::{Deserialize, Serialize};

use super::content::{Content, Part};
use super::enums::{AudioFormat, LiveModelProfile, Modality, ModelId, Sensitivity, Voice};
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

/// Voice configuration within speech config: a prebuilt voice, or a voice
/// replicated from a sample.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConfig {
    /// Prebuilt voice selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prebuilt_voice_config: Option<PrebuiltVoiceConfig>,
    /// A custom voice replicated from a short recording (Gemini 3.8 Live on
    /// Vertex AI; allow-listed customers only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replicated_voice_config: Option<ReplicatedVoiceConfig>,
}

/// Prebuilt voice selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrebuiltVoiceConfig {
    /// Name of the prebuilt voice (e.g. `"Puck"`, `"Kore"`).
    pub voice_name: String,
}

/// A voice the model replicates from a recorded sample, configured per
/// session. Nothing is uploaded or trained in advance.
///
/// You are responsible for the consents and rights needed to process the
/// voice sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicatedVoiceConfig {
    /// MIME type of the sample, e.g. `"audio/pcm;rate=24000"`.
    pub mime_type: String,
    /// The voice sample, base64-encoded.
    pub voice_sample_audio: String,
}

impl ReplicatedVoiceConfig {
    /// A replicated voice from raw sample bytes (base64-encoded here).
    pub fn from_sample(sample: &[u8], mime_type: impl Into<String>) -> Self {
        use base64::Engine as _;
        Self {
            mime_type: mime_type.into(),
            voice_sample_audio: base64::engine::general_purpose::STANDARD.encode(sample),
        }
    }
}

/// Audio transcription settings, for the user's audio
/// (`inputAudioTranscription`) or the model's (`outputAudioTranscription`).
///
/// The default — every field unset — asks for transcription with the
/// server's defaults, which is what an empty `{}` on the wire means.
///
/// ```
/// # use gemini_genai_rs::protocol::types::AudioTranscriptionConfig;
/// let config = AudioTranscriptionConfig::default()
///     .language_codes(["en-US", "es-US"])
///     .custom_vocabulary(["ORD-8472", "QwikPay"]);
/// assert_eq!(
///     serde_json::to_value(&config).unwrap(),
///     serde_json::json!({
///         "languageCodes": ["en-US", "es-US"],
///         "customVocabulary": ["ORD-8472", "QwikPay"],
///     }),
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AudioTranscriptionConfig {
    /// BCP-47 language hints (e.g. `"en-US"`). Hints reduce misdetected
    /// languages, especially on short utterances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_codes: Option<Vec<String>>,
    /// Domain terms the recogniser should prefer — product names, SKUs,
    /// proper nouns (Gemini 3.8 Live).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_vocabulary: Option<Vec<String>>,
}

impl AudioTranscriptionConfig {
    /// Set the BCP-47 language hints.
    pub fn language_codes<S: Into<String>>(mut self, codes: impl IntoIterator<Item = S>) -> Self {
        self.language_codes = Some(codes.into_iter().map(Into::into).collect());
        self
    }

    /// Set the terms transcription is biased toward.
    pub fn custom_vocabulary<S: Into<String>>(
        mut self,
        terms: impl IntoIterator<Item = S>,
    ) -> Self {
        self.custom_vocabulary = Some(terms.into_iter().map(Into::into).collect());
        self
    }
}

/// Input audio transcription configuration.
pub type InputAudioTranscription = AudioTranscriptionConfig;

/// Output audio transcription configuration.
pub type OutputAudioTranscription = AudioTranscriptionConfig;

/// Live Avatar video output (Gemini 3.8 Live): a prebuilt avatar by name, or
/// a custom one generated from a reference image.
///
/// Setting an avatar makes the model answer with synchronized 24 FPS video
/// (`responseModalities: ["VIDEO"]`), delivered as
/// [`SessionEvent::Media`](crate::session::SessionEvent::Media) chunks.
///
/// ```
/// # use gemini_genai_rs::protocol::types::AvatarConfig;
/// let ben = AvatarConfig::prebuilt("Ben");
/// assert_eq!(serde_json::to_value(&ben).unwrap(), serde_json::json!({"avatarName": "Ben"}));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AvatarConfig {
    /// Name of a prebuilt avatar (e.g. `"Ben"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_name: Option<String>,
    /// A custom avatar generated from a reference image (allow-listed
    /// customers only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customized_avatar: Option<CustomizedAvatar>,
    /// Audio bitrate of the avatar stream, in bits per second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_bitrate_bps: Option<u32>,
    /// Video bitrate of the avatar stream, in bits per second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_bitrate_bps: Option<u32>,
}

impl AvatarConfig {
    /// A prebuilt avatar by name.
    pub fn prebuilt(name: impl Into<String>) -> Self {
        Self {
            avatar_name: Some(name.into()),
            ..Self::default()
        }
    }

    /// A custom avatar from a reference image.
    ///
    /// The documented requirements: PNG recommended (alpha avoids a visible
    /// box), RGB, at least 704×1280, under 5 MB, a neutral bust shot facing
    /// the camera. `image_mime_type` is sent as given; the API's own example
    /// uses `"png"`. You are responsible for the consents and rights needed
    /// to process the likeness.
    pub fn custom(image: &[u8], image_mime_type: impl Into<String>) -> Self {
        use base64::Engine as _;
        Self {
            customized_avatar: Some(CustomizedAvatar {
                image_mime_type: Some(image_mime_type.into()),
                image_data: Some(base64::engine::general_purpose::STANDARD.encode(image)),
            }),
            ..Self::default()
        }
    }

    /// Set the audio and video bitrates of the avatar stream.
    pub fn bitrates(mut self, audio_bps: u32, video_bps: u32) -> Self {
        self.audio_bitrate_bps = Some(audio_bps);
        self.video_bitrate_bps = Some(video_bps);
        self
    }
}

/// The reference image of a custom avatar.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomizedAvatar {
    /// Image format, as the API's example gives it (`"png"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_mime_type: Option<String>,
    /// The image, base64-encoded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_data: Option<String>,
}

/// How conversation history seeded with `clientContent` is treated.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryConfig {
    /// Gemini 3.8 Live accepts `clientContent` as initial history only when
    /// this is set: send the turns after `setupComplete`, with
    /// `turnComplete: true` on the last one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_history_in_client_content: Option<bool>,
}

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
    /// Only speech/audio included in turn (VAD-filtered).
    #[serde(rename = "TURN_INCLUDES_ONLY_ACTIVITY")]
    TurnIncludesOnlyActivity,
    /// All input including silence included in turn. This is the Gemini API default
    /// when `turn_coverage` is unspecified.
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
    /// Sensitivity for detecting speech onset. The wire enum has only
    /// `START_SENSITIVITY_HIGH` / `_LOW`; see [`Sensitivity`] for how the
    /// other levels are sent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "sensitivity_wire::start"
    )]
    pub start_of_speech_sensitivity: Option<Sensitivity>,
    /// Sensitivity for detecting end of speech (`END_SENSITIVITY_HIGH` /
    /// `_LOW` on the wire).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "sensitivity_wire::end"
    )]
    pub end_of_speech_sensitivity: Option<Sensitivity>,
    /// Milliseconds of audio to include before speech onset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_padding_ms: Option<u32>,
    /// Milliseconds of silence before end-of-speech is triggered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_duration_ms: Option<u32>,
}

/// Wire encoding of [`Sensitivity`] for the two VAD sensitivity fields.
///
/// The API's enums are per field — `START_SENSITIVITY_{HIGH,LOW}` and
/// `END_SENSITIVITY_{HIGH,LOW}` — so a plain `SENSITIVITY_LOW` is not a value
/// either field accepts. Levels the wire cannot express (`Medium`,
/// `Automatic`, `Disabled`) go out as `*_UNSPECIFIED`, the server default.
mod sensitivity_wire {
    use super::Sensitivity;
    use serde::{Deserialize, Deserializer, Serializer};

    fn encode(prefix: &str, s: Sensitivity) -> String {
        let level = match s {
            Sensitivity::SensitivityHigh => "HIGH",
            Sensitivity::SensitivityLow => "LOW",
            Sensitivity::SensitivityMedium | Sensitivity::Automatic | Sensitivity::Disabled => {
                "UNSPECIFIED"
            }
        };
        format!("{prefix}_SENSITIVITY_{level}")
    }

    fn decode(raw: &str) -> Sensitivity {
        if raw.ends_with("HIGH") {
            Sensitivity::SensitivityHigh
        } else if raw.ends_with("LOW") {
            Sensitivity::SensitivityLow
        } else if raw.ends_with("MEDIUM") {
            Sensitivity::SensitivityMedium
        } else if raw == "DISABLED" {
            Sensitivity::Disabled
        } else {
            Sensitivity::Automatic
        }
    }

    fn ser<S: Serializer>(prefix: &str, v: &Option<Sensitivity>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(level) => s.serialize_str(&encode(prefix, *level)),
            None => s.serialize_none(),
        }
    }

    fn de<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Sensitivity>, D::Error> {
        Ok(Option::<String>::deserialize(d)?.map(|raw| decode(&raw)))
    }

    pub(super) mod start {
        use super::*;
        pub(crate) fn serialize<S: Serializer>(
            v: &Option<Sensitivity>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            ser("START", v, s)
        }
        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<Sensitivity>, D::Error> {
            de(d)
        }
    }

    pub(super) mod end {
        use super::*;
        pub(crate) fn serialize<S: Serializer>(
            v: &Option<Sensitivity>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            ser("END", v, s)
        }
        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<Sensitivity>, D::Error> {
            de(d)
        }
    }
}

/// Session resumption configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionConfig {
    /// Opaque handle from a previous session for transparent resume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Transparent mode: every resumption update also names the last client
    /// message the server consumed
    /// ([`ResumeInfo::last_consumed_index`](crate::session::ResumeInfo)), so a
    /// reconnecting client knows which messages to send again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparent: Option<bool>,
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
    ///
    /// Live reports this as `responseTokenCount`; `generateContent` as
    /// `candidatesTokenCount`. Both land here.
    #[serde(default, alias = "candidatesTokenCount")]
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    /// Token budget for thinking/reasoning steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// Whether to include the model's thought process in responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
    /// How much the model thinks, for models that take a level instead of a
    /// budget (Gemini 3.8 Live Extended Thinking requires one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

/// How much a model thinks before answering, for models that take a level
/// rather than a token budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ThinkingLevel {
    /// As little as the model allows.
    Minimal,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
}

/// Media resolution for image/video inputs: the per-frame token budget.
///
/// Sent as the API's enum names (`MEDIA_RESOLUTION_LOW`, …); the bare
/// `LOW` / `MEDIUM` / `HIGH` earlier releases sent are still read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaResolution {
    /// Low resolution: fewest tokens per frame.
    #[serde(rename = "MEDIA_RESOLUTION_LOW", alias = "LOW")]
    Low,
    /// Medium resolution.
    #[serde(rename = "MEDIA_RESOLUTION_MEDIUM", alias = "MEDIUM")]
    Medium,
    /// High resolution: finest visual detail.
    #[serde(rename = "MEDIA_RESOLUTION_HIGH", alias = "HIGH")]
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
    /// Strings that stop generation when the model produces one (up to five).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
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
/// # use gemini_genai_rs::protocol::types::{ApiEndpoint, VertexConfig};
/// let google_ai = ApiEndpoint::google_ai("MY_API_KEY");
/// let with_token = ApiEndpoint::google_ai_token("ya29.ACCESS_TOKEN");
/// let vertex = ApiEndpoint::vertex("my-project", "us-central1", "ACCESS_TOKEN");
/// ```
#[derive(Clone)]
pub enum ApiEndpoint {
    /// Google AI Studio -- API-key authentication.
    GoogleAI {
        /// The API key.
        api_key: String,
    },
    /// Google AI with OAuth2 access token (e.g. from gcloud).
    GoogleAIToken {
        /// The OAuth2 access token.
        access_token: AccessToken,
    },
    /// Vertex AI — project + location + OAuth2 bearer token.
    VertexAI(VertexConfig),
}

impl std::fmt::Debug for ApiEndpoint {
    /// Credentials never appear in `Debug` output: a config logged at
    /// `debug!` level must not leak the key that authenticates it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GoogleAI { .. } => f
                .debug_struct("GoogleAI")
                .field("api_key", &"<redacted>")
                .finish(),
            Self::GoogleAIToken { access_token } => f
                .debug_struct("GoogleAIToken")
                .field("access_token", access_token)
                .finish(),
            Self::VertexAI(v) => f.debug_tuple("VertexAI").field(v).finish(),
        }
    }
}

/// An OAuth2 access token source.
///
/// Tokens from `gcloud auth print-access-token` and service-account exchanges
/// live for about an hour. A session that reconnects after that with the
/// string it was built with is refused; [`AccessToken::Dynamic`] is consulted
/// on every connection attempt instead, so a refreshing source keeps
/// reconnects authenticated. `Debug` output never shows the token.
///
/// ```
/// # use gemini_genai_rs::protocol::types::AccessToken;
/// let fixed: AccessToken = "ya29.TOKEN".into();
/// let fresh = AccessToken::from_fn(|| std::env::var("GOOGLE_ACCESS_TOKEN").unwrap_or_default());
/// assert_eq!(fixed.get(), "ya29.TOKEN");
/// assert_eq!(format!("{fresh:?}"), "AccessToken(<redacted>)");
/// ```
#[derive(Clone)]
pub enum AccessToken {
    /// A fixed token string.
    Static(String),
    /// A source consulted on every connection attempt.
    Dynamic(std::sync::Arc<dyn Fn() -> String + Send + Sync>),
}

impl AccessToken {
    /// A token source that is called on every connection attempt.
    pub fn from_fn(f: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self::Dynamic(std::sync::Arc::new(f))
    }

    /// The current token.
    pub fn get(&self) -> String {
        match self {
            Self::Static(t) => t.clone(),
            Self::Dynamic(f) => f(),
        }
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

impl From<String> for AccessToken {
    fn from(t: String) -> Self {
        Self::Static(t)
    }
}

impl From<&str> for AccessToken {
    fn from(t: &str) -> Self {
        Self::Static(t.to_string())
    }
}

impl From<&String> for AccessToken {
    fn from(t: &String) -> Self {
        Self::Static(t.clone())
    }
}

/// Configuration for connecting through Vertex AI.
#[derive(Debug, Clone)]
pub struct VertexConfig {
    /// Google Cloud project ID (e.g. `"my-project-123"`).
    pub project: String,
    /// Regional location (e.g. `"us-central1"`).
    pub location: String,
    /// OAuth2 bearer token source — a fixed string, or a closure consulted
    /// on every connection attempt (see [`AccessToken`]).
    pub access_token: AccessToken,
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
    pub fn google_ai_token(access_token: impl Into<AccessToken>) -> Self {
        Self::GoogleAIToken {
            access_token: access_token.into(),
        }
    }

    /// Shorthand for Vertex AI endpoint.
    ///
    /// `access_token` is a fixed string or an [`AccessToken`]; for a token
    /// that refreshes across reconnects see [`ApiEndpoint::vertex_refreshing`].
    pub fn vertex(
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<AccessToken>,
    ) -> Self {
        Self::VertexAI(VertexConfig {
            project: project.into(),
            location: location.into(),
            access_token: access_token.into(),
            api_host: None,
        })
    }

    /// Vertex AI endpoint whose bearer token is fetched on every connection
    /// attempt, so a reconnect after the token's ~1 h lifetime carries a
    /// live credential.
    ///
    /// ```
    /// # use gemini_genai_rs::protocol::types::ApiEndpoint;
    /// let endpoint = ApiEndpoint::vertex_refreshing("my-project", "us-central1", || {
    ///     std::env::var("GOOGLE_ACCESS_TOKEN").unwrap_or_default()
    /// });
    /// ```
    pub fn vertex_refreshing(
        project: impl Into<String>,
        location: impl Into<String>,
        token: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        Self::vertex(project, location, AccessToken::from_fn(token))
    }

    /// Vertex AI endpoint with a custom API host (for private endpoints,
    /// VPC-SC, or testing).
    pub fn vertex_with_host(
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<AccessToken>,
        api_host: impl Into<String>,
    ) -> Self {
        Self::VertexAI(VertexConfig {
            project: project.into(),
            location: location.into(),
            access_token: access_token.into(),
            api_host: Some(api_host.into()),
        })
    }

    /// Resolve an endpoint from standard environment variables.
    ///
    /// Selects the platform from `GOOGLE_GENAI_USE_VERTEXAI` (`true`/`1`):
    ///
    /// - **Vertex AI** (`GOOGLE_GENAI_USE_VERTEXAI=true`): reads
    ///   `GOOGLE_CLOUD_PROJECT` (required), `GOOGLE_CLOUD_LOCATION`
    ///   (default `us-central1`), and `GOOGLE_ACCESS_TOKEN` (required —
    ///   higher layers may fall back to `gcloud auth print-access-token`).
    /// - **Google AI** (default): reads the first set of `GEMINI_API_KEY`,
    ///   `GOOGLE_GENAI_API_KEY`, or `GOOGLE_API_KEY`.
    ///
    /// Returns a descriptive error naming the missing variable so the
    /// failure is actionable.
    pub fn from_env() -> Result<Self, EndpointEnvError> {
        let use_vertex = std::env::var("GOOGLE_GENAI_USE_VERTEXAI")
            .map(|v| {
                let v = v.trim();
                v.eq_ignore_ascii_case("true") || v == "1"
            })
            .unwrap_or(false);

        if use_vertex {
            let project = non_empty_env("GOOGLE_CLOUD_PROJECT")
                .ok_or(EndpointEnvError::Missing("GOOGLE_CLOUD_PROJECT"))?;
            let location =
                non_empty_env("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|| "us-central1".to_string());
            let token = non_empty_env("GOOGLE_ACCESS_TOKEN")
                .ok_or(EndpointEnvError::Missing("GOOGLE_ACCESS_TOKEN"))?;
            Ok(Self::vertex(project, location, token))
        } else {
            let api_key = non_empty_env("GEMINI_API_KEY")
                .or_else(|| non_empty_env("GOOGLE_GENAI_API_KEY"))
                .or_else(|| non_empty_env("GOOGLE_API_KEY"))
                .ok_or(EndpointEnvError::Missing(
                    "GEMINI_API_KEY (or GOOGLE_GENAI_API_KEY / GOOGLE_API_KEY)",
                ))?;
            Ok(Self::google_ai(api_key))
        }
    }
}

/// Read an environment variable, treating empty/whitespace as unset.
fn non_empty_env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

/// Error resolving an [`ApiEndpoint`] from the environment.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointEnvError {
    /// A required environment variable is unset or empty.
    #[error("missing required environment variable: {0}")]
    Missing(&'static str),
}

/// Complete session configuration — the builder entrypoint.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// API endpoint and credentials (Google AI key or Vertex AI project/token).
    pub endpoint: ApiEndpoint,
    /// Which Gemini model to use.
    /// The Live model. `None` means "the platform's current default", resolved
    /// at connect time by [`SessionConfig::resolved_model`]; set it only when a
    /// specific model is required.
    pub model: Option<ModelId>,
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
    /// Live Avatar video output (Gemini 3.8 Live).
    pub avatar_config: Option<AvatarConfig>,
    /// How `clientContent` history is treated (Gemini 3.8 Live).
    pub history_config: Option<HistoryConfig>,
    /// Ask the server to send `voiceActivity` events at speech boundaries
    /// (Vertex AI; stripped on Google AI, which does not accept it).
    pub explicit_vad_signal: Option<bool>,
    /// Encoding of the audio sent with `send_audio` (default: PCM16 at
    /// [`LIVE_INPUT_SAMPLE_RATE`](super::enums::LIVE_INPUT_SAMPLE_RATE)).
    /// The output format is not configurable: the API returns 24 kHz PCM16.
    pub input_audio_format: AudioFormat,
    /// Optional send pacing for outbound audio (token-bucket backpressure).
    ///
    /// `None` (default) sends audio as fast as the command queue accepts it.
    /// When set, [`SessionHandle::send_audio`](crate::session::SessionHandle::send_audio)
    /// paces the *producer*: a caller pushing audio faster than
    /// `refill_rate_bps` waits, instead of overflowing the send queue.
    pub audio_pacing: Option<crate::transport::BackpressureConfig>,
    /// Optional wire recorder. When set, [`connect`](crate::transport::connect)
    /// and [`ConnectBuilder`](crate::transport::ConnectBuilder) wrap the codec in a
    /// [`RecordingCodec`](crate::transport::RecordingCodec) so every wire byte
    /// (both directions) is delivered to the recorder. See
    /// [`SessionConfig::record_wire`].
    pub wire_recorder: Option<crate::transport::WireRecorderHandle>,
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
    /// # use gemini_genai_rs::protocol::types::SessionConfig;
    /// let config = SessionConfig::from_access_token("ya29.ACCESS_TOKEN");
    /// ```
    pub fn from_access_token(access_token: impl Into<AccessToken>) -> Self {
        Self::from_endpoint(ApiEndpoint::google_ai_token(access_token))
    }

    /// Create a session configuration for Vertex AI.
    ///
    /// Uses the regional Vertex AI endpoint (`{location}-aiplatform.googleapis.com`).
    /// For the global endpoint, consider using [`SessionConfig::from_access_token`]
    /// instead.
    ///
    /// ```rust
    /// # use gemini_genai_rs::protocol::types::SessionConfig;
    /// let config = SessionConfig::from_vertex(
    ///     "my-project-123",
    ///     "us-central1",
    ///     "ya29.ACCESS_TOKEN",
    /// );
    /// ```
    pub fn from_vertex(
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<AccessToken>,
    ) -> Self {
        Self::from_endpoint(ApiEndpoint::vertex(project, location, access_token))
    }

    /// Create a session configuration from an explicit [`ApiEndpoint`].
    pub fn from_endpoint(endpoint: ApiEndpoint) -> Self {
        Self {
            endpoint,
            model: None,
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
            avatar_config: None,
            history_config: None,
            explicit_vad_signal: None,
            input_audio_format: AudioFormat::Pcm16,
            audio_pacing: None,
            wire_recorder: None,
        }
    }

    /// Set the Gemini model.
    pub fn model(mut self, model: impl Into<ModelId>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// The model this session will ask for: the configured one, else the
    /// platform default (see [`ModelId::live_default`]).
    pub fn resolved_model(&self) -> ModelId {
        self.model
            .clone()
            .unwrap_or_else(|| ModelId::live_default(self.is_vertex()))
    }

    /// Record every wire byte (both directions) to the given recorder.
    ///
    /// Both connect paths ([`connect`](crate::transport::connect) and
    /// [`ConnectBuilder`](crate::transport::ConnectBuilder)) honor this by
    /// wrapping the codec in a [`RecordingCodec`](crate::transport::RecordingCodec).
    /// Use [`FileWireRecorder`](crate::transport::FileWireRecorder) for a
    /// durable JSONL log that can be replayed offline with
    /// [`ReplayTransport`](crate::transport::ReplayTransport).
    pub fn record_wire(
        mut self,
        recorder: std::sync::Arc<dyn crate::transport::WireRecorder>,
    ) -> Self {
        self.wire_recorder = Some(crate::transport::WireRecorderHandle::new(recorder));
        self
    }

    /// Set the output voice.
    pub fn voice(mut self, voice: Voice) -> Self {
        self.generation_config.speech_config = Some(SpeechConfig {
            voice_config: Some(VoiceConfig {
                prebuilt_voice_config: Some(PrebuiltVoiceConfig {
                    voice_name: voice.to_string(),
                }),
                replicated_voice_config: None,
            }),
        });
        self
    }

    /// Speak in a voice replicated from a recorded sample instead of a
    /// prebuilt one (Gemini 3.8 Live on Vertex AI; allow-listed customers).
    pub fn replicated_voice(mut self, voice: ReplicatedVoiceConfig) -> Self {
        self.generation_config.speech_config = Some(SpeechConfig {
            voice_config: Some(VoiceConfig {
                prebuilt_voice_config: None,
                replicated_voice_config: Some(voice),
            }),
        });
        self
    }

    /// Answer with Live Avatar video (Gemini 3.8 Live on Vertex AI).
    ///
    /// Also sets `responseModalities` to `["VIDEO"]`, which the API requires
    /// for avatar output; the synchronized speech rides in the same stream.
    /// Chunks arrive as [`SessionEvent::Media`](crate::session::SessionEvent::Media).
    ///
    /// Vertex AI only. Google AI's avatar config has no `avatarName` or
    /// `customizedAvatar` (those are left off the wire there), and its
    /// `gemini-3.8-live` refuses the `VIDEO` modality.
    pub fn avatar(mut self, avatar: AvatarConfig) -> Self {
        self.avatar_config = Some(avatar);
        self.generation_config.response_modalities = Some(vec![Modality::Video]);
        self
    }

    /// Accept conversation history sent with `clientContent` before the
    /// first turn (required by Gemini 3.8 Live to seed history). Send the
    /// turns after `setupComplete`, with `turnComplete: true` on the last.
    pub fn initial_history_in_client_content(mut self, enabled: bool) -> Self {
        self.history_config = Some(HistoryConfig {
            initial_history_in_client_content: Some(enabled),
        });
        self
    }

    /// Ask the server for explicit `voiceActivity` events at the start and
    /// end of user speech (Vertex AI only; stripped on Google AI).
    pub fn explicit_vad_signal(mut self, enabled: bool) -> Self {
        self.explicit_vad_signal = Some(enabled);
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

    /// Whether the server transcribes the user's audio
    /// (`SessionEvent::InputTranscription`).
    ///
    /// Enabling keeps settings already given through
    /// [`input_transcription_config`](Self::input_transcription_config).
    pub fn input_transcription(mut self, enabled: bool) -> Self {
        let current = self.input_audio_transcription.take();
        self.input_audio_transcription = enabled.then(|| current.unwrap_or_default());
        self
    }

    /// Whether the server transcribes the model's audio
    /// (`SessionEvent::OutputTranscription`).
    ///
    /// Enabling keeps settings already given through
    /// [`output_transcription_config`](Self::output_transcription_config).
    pub fn output_transcription(mut self, enabled: bool) -> Self {
        let current = self.output_audio_transcription.take();
        self.output_audio_transcription = enabled.then(|| current.unwrap_or_default());
        self
    }

    /// Transcribe the user's audio with these settings — language hints,
    /// custom vocabulary.
    pub fn input_transcription_config(mut self, config: AudioTranscriptionConfig) -> Self {
        self.input_audio_transcription = Some(config);
        self
    }

    /// Transcribe the model's audio with these settings.
    pub fn output_transcription_config(mut self, config: AudioTranscriptionConfig) -> Self {
        self.output_audio_transcription = Some(config);
        self
    }

    /// Bias transcription of the user's audio toward these terms — product
    /// names, SKUs, proper nouns (Gemini 3.8 Live). Enables input
    /// transcription, keeping any language hints already set.
    pub fn custom_vocabulary<S: Into<String>>(
        mut self,
        terms: impl IntoIterator<Item = S>,
    ) -> Self {
        let config = self.input_audio_transcription.take().unwrap_or_default();
        self.input_audio_transcription = Some(config.custom_vocabulary(terms));
        self
    }

    /// Set the temperature for generation.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.generation_config.temperature = Some(temp);
        self
    }

    /// Whether the *server* is detecting speech boundaries.
    ///
    /// True unless the caller explicitly disabled automatic detection, because
    /// that is the API's own default: omitting `realtimeInputConfig` entirely
    /// leaves server VAD on.
    ///
    /// This gates explicit `activityStart` / `activityEnd` signalling. The two
    /// are mutually exclusive on the wire — sending an activity signal while
    /// automatic detection is on draws a close frame, code 1007, *"Explicit
    /// activity control is not supported when automatic activity detection is
    /// enabled"*, and the session dies mid-utterance.
    pub fn automatic_activity_detection_enabled(&self) -> bool {
        self.realtime_input_config
            .as_ref()
            .and_then(|c| c.automatic_activity_detection.as_ref())
            .and_then(|d| d.disabled)
            != Some(true)
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

    /// Pace outbound audio with a token bucket (producer-side backpressure).
    ///
    /// See [`SessionConfig::audio_pacing`]. Use
    /// [`BackpressureConfig::default`](crate::transport::BackpressureConfig::default)
    /// for 16 kHz PCM16 rates with a ~250 ms burst allowance.
    pub fn audio_pacing(mut self, config: crate::transport::BackpressureConfig) -> Self {
        self.audio_pacing = Some(config);
        self
    }

    /// Apply recommended realtime input defaults for voice conversations.
    ///
    /// This preserves any values the caller already set. In particular, it sets
    /// `TURN_INCLUDES_ONLY_ACTIVITY` so long pauses/silence in a continuous mic
    /// stream are not included in the user's semantic turn.
    pub fn voice_realtime_defaults(mut self) -> Self {
        let mut ric = self.realtime_input_config.unwrap_or(RealtimeInputConfig {
            automatic_activity_detection: None,
            activity_handling: None,
            turn_coverage: None,
        });
        ric.activity_handling
            .get_or_insert(ActivityHandling::StartOfActivityInterrupts);
        ric.turn_coverage
            .get_or_insert(TurnCoverage::TurnIncludesOnlyActivity);
        self.realtime_input_config = Some(ric);
        self
    }

    /// Enable session resumption: the server issues resumption handles
    /// (`SessionEvent::SessionResumeUpdate`) that a later session can pass to
    /// [`resume_from`](Self::resume_from).
    pub fn session_resumption(mut self) -> Self {
        self.session_resumption
            .get_or_insert_with(SessionResumptionConfig::default);
        self
    }

    /// Enable session resumption in transparent mode (Vertex AI only; left
    /// off the wire on Google AI): each resumption update
    /// also names the last client message the server consumed
    /// (`ResumeInfo::last_consumed_index`), so a client resuming from the
    /// handle knows which messages to send again.
    pub fn transparent_resumption(mut self) -> Self {
        self.session_resumption
            .get_or_insert_with(SessionResumptionConfig::default)
            .transparent = Some(true);
        self
    }

    /// Resume an earlier session from a handle the server issued for it.
    /// Implies [`session_resumption`](Self::session_resumption).
    pub fn resume_from(mut self, handle: impl Into<String>) -> Self {
        self.session_resumption
            .get_or_insert_with(SessionResumptionConfig::default)
            .handle = Some(handle.into());
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
    ///
    /// Vertex AI only: Google AI's setup has no `proactivity` field and
    /// refuses the session over it, so it is left off the wire there (and
    /// listed by [`ignored_settings`](Self::ignored_settings)).
    pub fn proactive_audio(mut self, enabled: bool) -> Self {
        self.proactivity = Some(ProactivityConfig {
            proactive_audio: Some(enabled),
        });
        self
    }

    /// Enable thinking/reasoning with a token budget (Gemini 2.5+).
    pub fn thinking(mut self, budget: u32) -> Self {
        let mut tc = self.generation_config.thinking_config.unwrap_or_default();
        tc.thinking_budget = Some(budget);
        self.generation_config.thinking_config = Some(tc);
        self
    }

    /// Set the thinking level, for models that take one instead of a budget.
    /// Gemini 3.8 Live Extended Thinking refuses a setup without it; plain
    /// Gemini 3.8 Live refuses one with it.
    pub fn thinking_level(mut self, level: ThinkingLevel) -> Self {
        self.generation_config
            .thinking_config
            .get_or_insert_with(ThinkingConfig::default)
            .thinking_level = Some(level);
        self
    }

    /// Whether thought summaries are delivered (`SessionEvent::Thought`).
    /// Google AI only; see [`supports_thinking`](Self::supports_thinking).
    pub fn include_thoughts(mut self, enabled: bool) -> Self {
        let mut tc = self.generation_config.thinking_config.unwrap_or_default();
        tc.include_thoughts = Some(enabled);
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

    /// Set the encoding of the audio sent with `send_audio`.
    pub fn input_audio_format(mut self, format: AudioFormat) -> Self {
        self.input_audio_format = format;
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
                 ?key={api_key}"
            ),
            ApiEndpoint::GoogleAIToken { access_token } => format!(
                "wss://generativelanguage.googleapis.com/ws/\
                 google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent\
                 ?access_token={}",
                access_token.get()
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
                self.resolved_model().to_string()
            }
            ApiEndpoint::VertexAI(v) => format!(
                "projects/{}/locations/{}/publishers/google/models/{}",
                v.project,
                v.location,
                self.resolved_model().bare_name()
            ),
        }
    }

    /// The current bearer token when using Vertex AI, `None` for Google AI.
    ///
    /// The transport calls this on every connection attempt to set the
    /// `Authorization` header for the WebSocket upgrade, so a refreshing
    /// [`AccessToken`] is honoured on reconnect. Google AI endpoints pass the
    /// credential as a query parameter instead.
    pub fn bearer_token(&self) -> Option<String> {
        match &self.endpoint {
            ApiEndpoint::GoogleAI { .. } | ApiEndpoint::GoogleAIToken { .. } => None,
            ApiEndpoint::VertexAI(v) => Some(v.access_token.get()),
        }
    }

    /// Returns `true` if this config targets Vertex AI.
    pub fn is_vertex(&self) -> bool {
        matches!(self.endpoint, ApiEndpoint::VertexAI(_))
    }

    /// What the configured model accepts in its setup; see
    /// [`LiveModelProfile`].
    pub fn model_profile(&self) -> LiveModelProfile {
        LiveModelProfile::of(&self.resolved_model())
    }

    /// Whether the configured model can answer in text.
    ///
    /// Native-audio Live models, which are every Live model today including
    /// Gemini 3.8 Live, close the session (1007, "The requested combination
    /// of response modalities (TEXT) is not supported") when a setup asks
    /// for `TEXT`. A model this crate does not recognize is assumed to
    /// answer in text.
    pub fn supports_text_output(&self) -> bool {
        let model = self.resolved_model();
        let name = model.as_str().rsplit('/').next().unwrap_or_default();
        !(name.contains("native-audio") || name.starts_with("gemini-3.8-live"))
    }

    /// A [`text_only`](Self::text_only) session on a model that can only
    /// speak. The setup then asks for audio and its output transcription,
    /// and the transcription is delivered as the session's text
    /// (`SessionEvent::TextDelta`, `TextComplete`) with no audio, so a text
    /// session works on every model.
    pub fn text_via_transcription(&self) -> bool {
        self.generation_config.response_modalities.as_deref() == Some(&[Modality::Text])
            && !self.supports_text_output()
    }

    /// Returns `true` if the target accepts async tool calling fields:
    /// `behavior` on declarations and `scheduling` on responses.
    ///
    /// Google AI does, and so does Gemini 3.8 Live on Vertex AI. Earlier
    /// Vertex AI Live models do not; there the fields are stripped from the
    /// wire so callers can set them unconditionally.
    pub fn supports_async_tools(&self) -> bool {
        !self.is_vertex() || self.model_profile().vertex_async_tools
    }

    /// Returns `true` if the target accepts `thinkingConfig` in the setup
    /// message. Vertex AI does not, and Gemini 3.8 Live does not support
    /// thinking on either platform: the config is stripped there, so
    /// `thinking(..)` and `include_thoughts(..)` are no-ops (listed by
    /// [`ignored_settings`](Self::ignored_settings)).
    pub fn supports_thinking(&self) -> bool {
        let profile = self.model_profile();
        profile.thinking_level_required || (!self.is_vertex() && profile.thinking)
    }

    /// Whether a mid-session system-instruction update can go out as a
    /// `system`-role client content turn, as Vertex AI documents.
    ///
    /// Google AI closes the session (1007, "Request contains an invalid
    /// argument") on a `system` role — measured on Gemini 2.5, 3.1 and 3.8
    /// Live — so there the update is sent as a user-role turn that says it
    /// replaces the instructions, with `turnComplete: false`. Gemini 3.x
    /// follows it; Gemini 2.5 accepts it without following it, so on 2.5
    /// prefer context injection or a new session for a persona change.
    pub fn supports_system_role_updates(&self) -> bool {
        self.is_vertex()
    }

    /// Settings in this config that the target does not accept and that
    /// [`to_setup_message`](Self::to_setup_message) therefore leaves off the
    /// wire, by their wire names. Empty when everything configured is sent.
    ///
    /// Connect logs these as a warning, so a setting that has no effect is
    /// visible rather than silent.
    ///
    /// ```
    /// # use gemini_genai_rs::protocol::types::{ModelId, SessionConfig};
    /// let config = SessionConfig::from_vertex("p", "us-central1", "t")
    ///     .model(ModelId::LIVE_3_8)
    ///     .affective_dialog(true)
    ///     .thinking(512);
    /// assert_eq!(config.ignored_settings(), ["thinkingConfig", "enableAffectiveDialog"]);
    /// ```
    pub fn ignored_settings(&self) -> Vec<&'static str> {
        let profile = self.model_profile();
        let mut ignored = Vec::new();
        if self.generation_config.thinking_config.is_some() && !self.supports_thinking() {
            ignored.push("thinkingConfig");
        }
        if self.generation_config.enable_affective_dialog.is_some()
            && !profile.affective_dialog_flag
        {
            ignored.push("enableAffectiveDialog");
        }
        if self.proactivity.is_some() && (!self.is_vertex() || !profile.proactivity_flag) {
            ignored.push("proactivity");
        }
        if !self.is_vertex()
            && self
                .session_resumption
                .as_ref()
                .is_some_and(|r| r.transparent.is_some())
        {
            ignored.push("sessionResumption.transparent");
        }
        if !self.is_vertex()
            && let Some(avatar) = &self.avatar_config
        {
            if avatar.avatar_name.is_some() {
                ignored.push("avatarConfig.avatarName");
            }
            if avatar.customized_avatar.is_some() {
                ignored.push("avatarConfig.customizedAvatar");
            }
        }
        if self.explicit_vad_signal.is_some() && !self.is_vertex() {
            ignored.push("explicitVadSignal");
        }
        if !self.supports_async_tools()
            && self.tools.iter().any(|t| {
                t.function_declarations
                    .iter()
                    .flatten()
                    .any(|d| d.behavior.is_some())
            })
        {
            ignored.push("functionDeclarations[].behavior");
        }
        ignored
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
#[allow(
    unsafe_code,
    reason = "tests exercise from_env() and must mutate the process environment; each test restores what it touched"
)]
mod tests {
    use super::*;

    /// Drives `ApiEndpoint::from_env` through its branches. Kept in one test
    /// because process environment is global; vars are cleaned up at the end.
    #[test]
    fn api_endpoint_from_env() {
        let vars = [
            "GOOGLE_GENAI_USE_VERTEXAI",
            "GOOGLE_CLOUD_PROJECT",
            "GOOGLE_CLOUD_LOCATION",
            "GOOGLE_ACCESS_TOKEN",
            "GEMINI_API_KEY",
            "GOOGLE_GENAI_API_KEY",
            "GOOGLE_API_KEY",
        ];
        for v in vars {
            unsafe { std::env::remove_var(v) };
        }

        // Google AI via GEMINI_API_KEY (default platform).
        unsafe { std::env::set_var("GEMINI_API_KEY", "k-123") };
        assert!(matches!(
            ApiEndpoint::from_env(),
            Ok(ApiEndpoint::GoogleAI { ref api_key }) if api_key == "k-123"
        ));

        // Empty values are treated as unset → fall through to next candidate.
        unsafe { std::env::set_var("GEMINI_API_KEY", "   ") };
        unsafe { std::env::set_var("GOOGLE_API_KEY", "k-fallback") };
        assert!(matches!(
            ApiEndpoint::from_env(),
            Ok(ApiEndpoint::GoogleAI { ref api_key }) if api_key == "k-fallback"
        ));

        // No key anywhere → actionable error.
        unsafe { std::env::remove_var("GEMINI_API_KEY") };
        unsafe { std::env::remove_var("GOOGLE_API_KEY") };
        assert!(matches!(
            ApiEndpoint::from_env(),
            Err(EndpointEnvError::Missing(_))
        ));

        // Vertex AI with explicit token + default location.
        unsafe { std::env::set_var("GOOGLE_GENAI_USE_VERTEXAI", "TRUE") };
        unsafe { std::env::set_var("GOOGLE_CLOUD_PROJECT", "proj-1") };
        unsafe { std::env::set_var("GOOGLE_ACCESS_TOKEN", "tok-abc") };
        match ApiEndpoint::from_env() {
            Ok(ApiEndpoint::VertexAI(cfg)) => {
                assert_eq!(cfg.project, "proj-1");
                assert_eq!(cfg.location, "us-central1");
                assert_eq!(cfg.access_token.get(), "tok-abc");
            }
            other => panic!("expected Vertex endpoint, got {other:?}"),
        }

        // Vertex selected but token missing → distinguishable error so higher
        // layers can fall back to gcloud.
        unsafe { std::env::remove_var("GOOGLE_ACCESS_TOKEN") };
        assert!(matches!(
            ApiEndpoint::from_env(),
            Err(EndpointEnvError::Missing("GOOGLE_ACCESS_TOKEN"))
        ));

        for v in vars {
            unsafe { std::env::remove_var(v) };
        }
    }

    #[test]
    fn session_config_builder() {
        let config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"))
            .voice(Voice::Kore)
            .system_instruction("Be helpful.")
            .temperature(0.7);

        assert!(
            matches!(config.endpoint, ApiEndpoint::GoogleAI { ref api_key } if api_key == "test-key")
        );
        assert_eq!(
            config.model,
            Some(ModelId::from_static("models/gemini-2.0-flash-live-001"))
        );
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
    fn a_text_session_on_a_speech_only_model_asks_for_the_transcript() {
        for model in [
            ModelId::LIVE_3_8,
            ModelId::FLASH_2_5_NATIVE_AUDIO_LATEST,
            ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO,
        ] {
            let config = SessionConfig::new("key").model(model.clone()).text_only();
            assert!(config.text_via_transcription(), "{model:?}");
            let setup = serde_json::to_value(config.to_setup_message()).unwrap();
            assert_eq!(
                setup["setup"]["generationConfig"]["responseModalities"],
                serde_json::json!(["AUDIO"]),
                "{model:?}"
            );
            assert!(setup["setup"]["outputAudioTranscription"].is_object());
        }
        // A model that can answer in text keeps TEXT, and a voice session is
        // unaffected.
        let text_model = SessionConfig::new("key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"))
            .text_only();
        assert!(!text_model.text_via_transcription());
        let setup = serde_json::to_value(text_model.to_setup_message()).unwrap();
        assert_eq!(
            setup["setup"]["generationConfig"]["responseModalities"],
            serde_json::json!(["TEXT"])
        );
        assert!(
            !SessionConfig::new("key")
                .model(ModelId::LIVE_3_8)
                .text_via_transcription()
        );
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
            .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);
        assert!(config.is_vertex());
        assert_eq!(config.bearer_token().as_deref(), Some("token123"));
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
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        assert_eq!(
            config.model_uri(),
            "projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.0-flash-live-001"
        );
    }

    #[test]
    fn vertex_model_uri_custom_model() {
        let config = SessionConfig::from_vertex("proj", "asia-southeast1", "tok").model(
            ModelId::new("gemini-live-2.5-flash-native-audio".to_string()),
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
        let config = SessionConfig::new("key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
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
        assert!(json.contains("\"MEDIA_RESOLUTION_HIGH\""));
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
    fn voice_realtime_defaults_sets_turn_coverage_without_overriding() {
        let config = SessionConfig::new("key").voice_realtime_defaults();
        let realtime = config.realtime_input_config.expect("realtime config");
        assert_eq!(
            realtime.activity_handling,
            Some(ActivityHandling::StartOfActivityInterrupts)
        );
        assert_eq!(
            realtime.turn_coverage,
            Some(TurnCoverage::TurnIncludesOnlyActivity)
        );

        let config = SessionConfig::new("key")
            .activity_handling(ActivityHandling::NoInterruption)
            .turn_coverage(TurnCoverage::TurnIncludesAllInput)
            .voice_realtime_defaults();
        let realtime = config.realtime_input_config.expect("realtime config");
        assert_eq!(
            realtime.activity_handling,
            Some(ActivityHandling::NoInterruption)
        );
        assert_eq!(
            realtime.turn_coverage,
            Some(TurnCoverage::TurnIncludesAllInput)
        );
    }

    #[test]
    fn thinking_config_with_include_thoughts() {
        let config = SessionConfig::new("key")
            .thinking(2048)
            .include_thoughts(true);
        let json = config.to_setup_json();
        assert!(json.contains("\"thinkingBudget\""));
        assert!(json.contains("2048"));
        assert!(json.contains("\"includeThoughts\""));
        assert!(json.contains("true"));
    }

    #[test]
    fn google_ai_supports_async_tools() {
        let config = SessionConfig::new("key");
        assert!(config.supports_async_tools());
    }

    #[test]
    fn vertex_ai_does_not_support_async_tools() {
        let config = SessionConfig::from_vertex("proj", "us-central1", "token");
        assert!(!config.supports_async_tools());
    }

    #[test]
    fn vertex_ai_strips_behavior_from_setup() {
        use crate::protocol::types::{FunctionCallingBehavior, FunctionDeclaration, Tool};
        let config = SessionConfig::from_vertex("proj", "us-central1", "token").add_tool(
            Tool::functions(vec![FunctionDeclaration {
                name: "test".into(),
                description: "test".into(),
                parameters: None,
                behavior: Some(FunctionCallingBehavior::NonBlocking),
            }]),
        );
        let setup = config.to_setup_message();
        let decl = &setup.setup.tools[0].function_declarations.as_ref().unwrap()[0];
        assert!(decl.behavior.is_none(), "Vertex AI should strip behavior");
    }

    #[test]
    fn google_ai_preserves_behavior_in_setup() {
        use crate::protocol::types::{FunctionCallingBehavior, FunctionDeclaration, Tool};
        let config =
            SessionConfig::new("key").add_tool(Tool::functions(vec![FunctionDeclaration {
                name: "test".into(),
                description: "test".into(),
                parameters: None,
                behavior: Some(FunctionCallingBehavior::NonBlocking),
            }]));
        let setup = config.to_setup_message();
        let decl = &setup.setup.tools[0].function_declarations.as_ref().unwrap()[0];
        assert_eq!(
            decl.behavior,
            Some(FunctionCallingBehavior::NonBlocking),
            "Google AI should preserve behavior"
        );
    }
}
