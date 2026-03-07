//! Safety types shared by Generate, Live, and other APIs.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Safety types (shared by Generate, Live, and other APIs)
// ---------------------------------------------------------------------------

/// Categories of potential harm in model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HarmCategory {
    /// Unspecified harm category.
    HarmCategoryUnspecified,
    /// Harassment content.
    HarmCategoryHarassment,
    /// Hate speech content.
    HarmCategoryHateSpeech,
    /// Sexually explicit content.
    HarmCategorySexuallyExplicit,
    /// Dangerous content.
    HarmCategoryDangerousContent,
    /// Civic integrity violations.
    HarmCategoryCivicIntegrity,
}

/// Blocking threshold for safety settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HarmBlockThreshold {
    /// Do not block any content.
    BlockNone,
    /// Block only high-probability harmful content.
    BlockOnlyHigh,
    /// Block medium and above probability harmful content.
    BlockMediumAndAbove,
    /// Block low and above probability harmful content.
    BlockLowAndAbove,
}

/// Probability that content is harmful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HarmProbability {
    /// Negligible probability of harm.
    Negligible,
    /// Low probability of harm.
    Low,
    /// Medium probability of harm.
    Medium,
    /// High probability of harm.
    High,
}

/// Per-category safety configuration for content generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySetting {
    /// Which harm category this setting applies to.
    pub category: HarmCategory,
    /// Blocking threshold for this category.
    pub threshold: HarmBlockThreshold,
}

/// Per-category safety assessment of generated content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyRating {
    /// Which harm category was assessed.
    pub category: HarmCategory,
    /// Probability of harmful content.
    pub probability: HarmProbability,
    /// Whether the content was blocked.
    #[serde(default)]
    pub blocked: bool,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FinishReason {
    /// Natural stop or stop sequence.
    Stop,
    /// Hit max_output_tokens.
    MaxTokens,
    /// Blocked by safety filter.
    Safety,
    /// Recitation risk.
    Recitation,
    /// Model-internal reasoning (e.g., language).
    Language,
    /// Other or unspecified.
    Other,
    /// Blocklist triggered.
    Blocklist,
    /// Prohibited content.
    ProhibitedContent,
    /// SPII detected.
    Spii,
    /// Malformed function call.
    MalformedFunctionCall,
    /// Unknown/unrecognized.
    #[serde(other)]
    FinishReasonUnspecified,
}

/// Citation metadata for a response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationMetadata {
    /// List of citation sources.
    #[serde(default)]
    pub citation_sources: Vec<CitationSource>,
}

/// A single citation source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationSource {
    /// Start index in the generated text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_index: Option<i32>,
    /// End index in the generated text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_index: Option<i32>,
    /// URI of the cited source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// License of the cited source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

/// Reference to an uploaded file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileData {
    /// URI of the uploaded file.
    pub file_uri: String,
    /// MIME type of the file.
    pub mime_type: String,
}
