//! Client → Server message types for the Gemini Live wire protocol.

use serde::{Deserialize, Serialize};

use crate::protocol::types::*;

/// Top-level setup message sent immediately after WebSocket connect.
#[derive(Debug, Clone, Serialize)]
pub struct SetupMessage {
    /// The setup payload.
    pub setup: SetupPayload,
}

/// Payload of the setup message.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupPayload {
    /// Model URI string (e.g. `"models/gemini-2.0-flash-live-001"`).
    pub model: String,
    /// Generation parameters (modalities, temperature, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    /// System instruction content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    /// Tool declarations for function calling, search, etc.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// Tool usage configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    /// Enable input audio transcription.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_audio_transcription: Option<InputAudioTranscription>,
    /// Enable output audio transcription.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_audio_transcription: Option<OutputAudioTranscription>,
    /// Realtime input configuration (VAD, activity handling).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_input_config: Option<RealtimeInputConfig>,
    /// Session resumption configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_resumption: Option<SessionResumptionConfig>,
    /// Context window compression configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_compression: Option<ContextWindowCompressionConfig>,
    /// Proactivity configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proactivity: Option<ProactivityConfig>,
}

impl SessionConfig {
    /// Build the setup message from this configuration.
    ///
    /// When targeting Vertex AI, `FunctionCallingBehavior` is stripped from
    /// tool declarations since Vertex AI does not support async tool calling.
    pub fn to_setup_message(&self) -> SetupMessage {
        let tools = if self.supports_async_tools() {
            self.tools.clone()
        } else {
            self.tools
                .iter()
                .map(|tool| {
                    let mut t = tool.clone();
                    if let Some(ref mut decls) = t.function_declarations {
                        for d in decls.iter_mut() {
                            d.behavior = None;
                        }
                    }
                    t
                })
                .collect()
        };

        SetupMessage {
            setup: SetupPayload {
                model: self.model_uri(),
                generation_config: Some(self.generation_config.clone()),
                system_instruction: self.system_instruction.clone(),
                tools,
                tool_config: self.tool_config.clone(),
                input_audio_transcription: self.input_audio_transcription.clone(),
                output_audio_transcription: self.output_audio_transcription.clone(),
                realtime_input_config: self.realtime_input_config.clone(),
                session_resumption: self.session_resumption.clone(),
                context_window_compression: self.context_window_compression.clone(),
                proactivity: self.proactivity.clone(),
            },
        }
    }

    /// Pre-serialize the setup message to JSON. Called once at connection time.
    pub fn to_setup_json(&self) -> String {
        serde_json::to_string(&self.to_setup_message())
            .expect("setup message serialization is infallible for valid config")
    }
}

/// Realtime audio input sent as a stream of chunks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeInputMessage {
    /// The realtime input payload.
    pub realtime_input: RealtimeInputPayload,
}

/// Payload for realtime audio input.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeInputPayload {
    /// Deprecated: use `audio` instead. Kept for backward compatibility.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub media_chunks: Vec<MediaChunk>,
    /// Audio input blob (preferred over media_chunks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Blob>,
    /// Video input blob.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<Blob>,
    /// Signal end of audio stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_stream_end: Option<bool>,
    /// Realtime text input (streamed inline, distinct from clientContent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A single chunk of media data (audio). Deprecated — use Blob in `audio` field.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaChunk {
    /// MIME type of the media (e.g. `"audio/pcm"`).
    pub mime_type: String,
    /// Base64-encoded media data.
    pub data: String, // base64-encoded
}

/// Client content message for sending text or conversation history.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientContentMessage {
    /// The client content payload.
    pub client_content: ClientContentPayload,
}

/// Payload for client content.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientContentPayload {
    /// Conversation turns to send.
    pub turns: Vec<Content>,
    /// Whether this completes the client's turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_complete: Option<bool>,
}

/// Tool response message sent after executing function calls.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResponseMessage {
    /// The tool response payload.
    pub tool_response: ToolResponsePayload,
}

/// Payload for tool response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResponsePayload {
    /// Function call responses to return to the model.
    pub function_responses: Vec<FunctionResponse>,
}

/// Activity signal for client-side VAD events.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySignalMessage {
    /// The activity signal payload.
    pub realtime_input: ActivitySignalPayload,
}

/// Payload for activity signals.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySignalPayload {
    /// Present when signaling activity start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_start: Option<ActivityStart>,
    /// Present when signaling activity end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_end: Option<ActivityEnd>,
}

/// Marker for speech activity start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityStart {}

/// Marker for speech activity end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEnd {}
