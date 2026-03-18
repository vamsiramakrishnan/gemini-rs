//! Model, voice, and enumeration types for the Gemini Multimodal Live API.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Model & Voice enumerations
// ---------------------------------------------------------------------------

/// Gemini models that support the Multimodal Live API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GeminiModel {
    /// Gemini 2.0 Flash Live (gemini-2.0-flash-live-001).
    #[serde(rename = "models/gemini-2.0-flash-live-001")]
    Gemini2_0FlashLive,
    /// Gemini Live 2.5 Flash with native audio (default).
    #[serde(rename = "models/gemini-live-2.5-flash-native-audio")]
    #[default]
    GeminiLive2_5FlashNativeAudio,
    /// Custom model string for forward compatibility.
    #[serde(untagged)]
    Custom(String),
}

impl std::fmt::Display for GeminiModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gemini2_0FlashLive => write!(f, "models/gemini-2.0-flash-live-001"),
            Self::GeminiLive2_5FlashNativeAudio => {
                write!(f, "models/gemini-live-2.5-flash-native-audio")
            }
            Self::Custom(s) => write!(f, "{s}"),
        }
    }
}

/// Available voice presets for Gemini Live audio output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Voice {
    /// Aoede voice preset.
    Aoede,
    /// Charon voice preset.
    Charon,
    /// Fenrir voice preset.
    Fenrir,
    /// Kore voice preset.
    Kore,
    /// Puck voice preset (default).
    #[default]
    Puck,
    /// Custom voice name for forward compatibility.
    #[serde(untagged)]
    Custom(String),
}

// ---------------------------------------------------------------------------
// Audio format
// ---------------------------------------------------------------------------

/// Audio encoding formats supported by the Gemini Live API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[derive(Default)]
pub enum AudioFormat {
    /// Raw 16-bit little-endian PCM.
    #[default]
    Pcm16,
    /// Opus-encoded audio.
    Opus,
}

impl AudioFormat {
    /// MIME type string for this format.
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Pcm16 => "audio/pcm",
            Self::Opus => "audio/opus",
        }
    }
}

/// Output modalities the model can produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Modality {
    /// Text output.
    Text,
    /// Audio output.
    Audio,
    /// Image output.
    Image,
}

/// Voice activity detection sensitivity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[derive(Default)]
pub enum Sensitivity {
    /// Disabled — no automatic detection.
    Disabled,
    /// Low sensitivity — fewer false positives, might miss soft speech.
    SensitivityLow,
    /// Medium sensitivity.
    SensitivityMedium,
    /// High sensitivity — catches everything, more false positives.
    SensitivityHigh,
    /// Automatic (server default).
    #[default]
    Automatic,
}

/// How the model should decide when to execute tool calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[derive(Default)]
pub enum FunctionCallingMode {
    /// Model decides when to call functions.
    #[default]
    Auto,
    /// Model always calls one of the declared functions.
    Any,
    /// Model never calls functions.
    None,
}

/// Whether tool calls block model output or run concurrently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[derive(Default)]
pub enum FunctionCallingBehavior {
    /// Model waits for tool response before continuing.
    #[default]
    Blocking,
    /// Model continues generating while tool executes.
    NonBlocking,
}

/// Scheduling mode for non-blocking function responses.
///
/// Controls how the model handles async tool results when
/// [`FunctionCallingBehavior::NonBlocking`] is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FunctionResponseScheduling {
    /// Model halts current output and immediately reports the tool result.
    Interrupt,
    /// Model waits until it finishes current output before handling the result.
    WhenIdle,
    /// Model integrates the result silently without notifying the user.
    Silent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_serialization() {
        let model = GeminiModel::Gemini2_0FlashLive;
        let json = serde_json::to_string(&model).unwrap();
        assert_eq!(json, "\"models/gemini-2.0-flash-live-001\"");
    }

    #[test]
    fn voice_serialization() {
        let voice = Voice::Kore;
        let json = serde_json::to_string(&voice).unwrap();
        assert_eq!(json, "\"Kore\"");
    }
}
