//! Model, voice, and enumeration types for the Gemini Multimodal Live API.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

// ---------------------------------------------------------------------------
// Model identifier & Voice enumeration
// ---------------------------------------------------------------------------

/// A Gemini model identifier, exactly as the API accepts it.
///
/// Either the resource form `models/gemini-…` or a bare `gemini-…` name; the
/// wire layer normalises whichever you give it. This is a string newtype
/// rather than an enum on purpose: the model catalog changes faster than any
/// release cycle, and an enum of "known" models guarantees that the named
/// variants are the stale ones while the real work goes through an escape
/// hatch. The known-good names live here as constants instead, so they are
/// discoverable without being the only thing that typechecks.
///
/// ```
/// use gemini_genai_rs::prelude::ModelId;
///
/// let a: ModelId = "models/gemini-2.5-flash-native-audio-latest".into();
/// let b = ModelId::new(String::from("gemini-flash-latest"));
/// let c = ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO;
/// assert_eq!(a.as_str(), "models/gemini-2.5-flash-native-audio-latest");
/// assert_ne!(b, c);
/// ```
///
/// Leave the model unset on [`SessionConfig`](crate::protocol::types::SessionConfig)
/// and connect resolves a platform-appropriate default via
/// [`ModelId::live_default`]; set it only when you need a specific model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(Cow<'static, str>);

impl ModelId {
    /// Vertex AI's GA Live model name (per Google Cloud docs).
    pub const LIVE_2_5_FLASH_NATIVE_AUDIO: ModelId =
        ModelId::from_static("models/gemini-live-2.5-flash-native-audio");

    /// Google AI's rolling alias for the current native-audio Live model.
    /// Verified reachable on Google AI 2026-08; the dated names it aliases
    /// are retired without notice, which is why the alias is the default.
    pub const FLASH_2_5_NATIVE_AUDIO_LATEST: ModelId =
        ModelId::from_static("models/gemini-2.5-flash-native-audio-latest");

    /// Google AI's rolling alias for the current Flash text model
    /// (`generateContent`). Dated `gemini-2.5-flash` names 404 there.
    pub const FLASH_LATEST: ModelId = ModelId::from_static("gemini-flash-latest");

    /// A model id from a `'static` string, usable in `const` context.
    pub const fn from_static(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    /// A model id from any string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(Cow::Owned(id.into()))
    }

    /// A model id with the `models/` resource prefix added when the name is
    /// bare (contains no `/`). Qualified names — `models/…`, `projects/…` —
    /// pass through untouched.
    pub fn qualified(id: impl Into<String>) -> Self {
        let id = id.into();
        if id.contains('/') {
            Self::new(id)
        } else {
            Self::new(format!("models/{id}"))
        }
    }

    /// The identifier as given.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The identifier without a leading `models/`, which is the form Vertex
    /// AI wants inside its publisher path.
    pub fn bare_name(&self) -> &str {
        self.0.strip_prefix("models/").unwrap_or(&self.0)
    }

    /// The Live model to use when none was set: `GEMINI_LIVE_MODEL` if set,
    /// else `GEMINI_MODEL`, else the platform's current native-audio Flash
    /// model.
    ///
    /// `GEMINI_LIVE_MODEL` exists because `GEMINI_MODEL` is also the text
    /// model override (`GeminiLlm` reads `GEMINI_TEXT_MODEL`, then
    /// `GEMINI_MODEL`): a native-audio name in the shared variable would 404
    /// every `generateContent` call. A bare name (no `/`) gets the `models/`
    /// prefix the wire expects; a qualified one passes through untouched.
    pub fn live_default(vertex: bool) -> Self {
        if let Some(m) = ["GEMINI_LIVE_MODEL", "GEMINI_MODEL"]
            .iter()
            .find_map(|k| std::env::var(k).ok())
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
        {
            return Self::qualified(m);
        }
        if vertex {
            Self::LIVE_2_5_FLASH_NATIVE_AUDIO
        } else {
            Self::FLASH_2_5_NATIVE_AUDIO_LATEST
        }
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ModelId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for ModelId {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&String> for ModelId {
    fn from(s: &String) -> Self {
        Self::new(s.as_str())
    }
}

impl std::str::FromStr for ModelId {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(s))
    }
}

impl PartialEq<str> for ModelId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ModelId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Available voice presets for Gemini Live audio output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[non_exhaustive]
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

impl std::fmt::Display for Voice {
    /// The name as the API's `prebuiltVoiceConfig.voiceName` wants it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Aoede => "Aoede",
            Self::Charon => "Charon",
            Self::Fenrir => "Fenrir",
            Self::Kore => "Kore",
            Self::Puck => "Puck",
            Self::Custom(name) => name,
        })
    }
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

/// The sample rate the Live API takes on the way in.
///
/// Fixed rather than configurable because the API states it natively accepts
/// 16 kHz and resamples anything else — so the only rate worth declaring is the
/// one callers are told to send.
pub const LIVE_INPUT_SAMPLE_RATE: u32 = 16_000;

impl AudioFormat {
    /// MIME type string for this format, as the realtime input contract needs it.
    ///
    /// The `rate` parameter on PCM is **required**, not decorative. The Live API
    /// documents realtime input as `audio/pcm;rate=16000`, and a bare
    /// `audio/pcm` is accepted by the socket and then silently transcribed as
    /// nothing: the session stays open, no error is returned, the input
    /// transcript comes back empty and the model never answers.
    ///
    /// That is exactly how this was found. Every audio test in the workspace
    /// drove sessions with `send_text`, so nothing exercised this path until a
    /// test fed real synthesised speech and got a 94-second turn with an empty
    /// transcript back.
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Pcm16 => "audio/pcm;rate=16000",
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
mod audio_format_tests {
    use super::*;

    /// The rate parameter is required by the Live API and its absence fails
    /// silently — empty transcript, no error, no answer. Pinned so it cannot
    /// be "tidied" back to a bare `audio/pcm`.
    #[test]
    fn pcm_declares_its_sample_rate() {
        assert_eq!(
            AudioFormat::Pcm16.mime_type(),
            "audio/pcm;rate=16000",
            "realtime input must declare the rate; a bare audio/pcm is accepted \
             by the socket and then transcribed as silence"
        );
        assert!(
            AudioFormat::Pcm16
                .mime_type()
                .contains(&LIVE_INPUT_SAMPLE_RATE.to_string())
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_id_is_a_transparent_string_on_the_wire() {
        let model = ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO;
        let json = serde_json::to_string(&model).unwrap();
        assert_eq!(json, "\"models/gemini-live-2.5-flash-native-audio\"");
        let back: ModelId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, model);
    }

    #[test]
    fn model_id_accepts_bare_and_resource_names_and_strips_the_prefix_for_vertex() {
        let resource: ModelId = "models/gemini-flash-latest".into();
        let bare = ModelId::new("gemini-flash-latest");
        assert_eq!(resource.bare_name(), "gemini-flash-latest");
        assert_eq!(bare.bare_name(), "gemini-flash-latest");
        assert_ne!(
            resource, bare,
            "the two spellings are distinct ids as given"
        );
        assert_eq!(bare, "gemini-flash-latest");
        assert_eq!(resource.to_string(), "models/gemini-flash-latest");
    }

    #[test]
    fn live_default_follows_the_platform_unless_overridden() {
        // GEMINI_MODEL is process-global; only assert the platform branch when
        // it is not set in this environment.
        if std::env::var_os("GEMINI_MODEL").is_none()
            && std::env::var_os("GEMINI_LIVE_MODEL").is_none()
        {
            assert_eq!(
                ModelId::live_default(true),
                ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO
            );
            assert_eq!(
                ModelId::live_default(false),
                ModelId::FLASH_2_5_NATIVE_AUDIO_LATEST
            );
        }
    }

    #[test]
    fn qualified_prefixes_bare_names_only() {
        assert_eq!(ModelId::qualified("gemini-x"), "models/gemini-x");
        assert_eq!(ModelId::qualified("models/gemini-x"), "models/gemini-x");
        assert_eq!(
            ModelId::qualified("projects/p/models/gemini-x"),
            "projects/p/models/gemini-x"
        );
    }

    #[test]
    fn voice_serialization() {
        let voice = Voice::Kore;
        let json = serde_json::to_string(&voice).unwrap();
        assert_eq!(json, "\"Kore\"");
    }
}
