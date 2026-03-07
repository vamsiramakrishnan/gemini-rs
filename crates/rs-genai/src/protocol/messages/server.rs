//! Server → Client message types for the Gemini Live wire protocol.

use serde::{Deserialize, Serialize};

use crate::protocol::types::*;

/// Server setup complete acknowledgment.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCompleteMessage {
    /// The setup complete payload.
    pub setup_complete: SetupCompletePayload,
}

/// Payload for setup complete.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCompletePayload {
    /// Session resumption result, if resumption was requested.
    #[serde(default)]
    pub session_resumption: Option<SessionResumptionResult>,
}

/// Session resumption result from server.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionResult {
    /// Opaque handle for future session resumption.
    #[serde(default)]
    pub handle: Option<String>,
    /// Whether the session was successfully resumed.
    #[serde(default)]
    pub resumed: Option<bool>,
}

/// Server content message containing model output.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerContentMessage {
    /// The server content payload.
    pub server_content: ServerContentPayload,
    /// Token usage metadata (present on most server messages).
    #[serde(default)]
    pub usage_metadata: Option<UsageMetadata>,
}

/// Payload for server content.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerContentPayload {
    /// Model output content for this turn.
    #[serde(default)]
    pub model_turn: Option<Content>,
    /// Whether the model's turn is complete.
    #[serde(default)]
    pub turn_complete: Option<bool>,
    /// Whether all generation (including tool use) is complete.
    #[serde(default)]
    pub generation_complete: Option<bool>,
    /// Whether the model was interrupted by user barge-in.
    #[serde(default)]
    pub interrupted: Option<bool>,
    /// Transcription of user audio input.
    #[serde(default)]
    pub input_transcription: Option<TranscriptionPayload>,
    /// Transcription of model audio output.
    #[serde(default)]
    pub output_transcription: Option<TranscriptionPayload>,
    /// Grounding metadata from search results.
    #[serde(default)]
    pub grounding_metadata: Option<GroundingMetadata>,
    /// URL context metadata for content sourced from URLs.
    #[serde(default)]
    pub url_context_metadata: Option<UrlContextMetadata>,
    /// Reason why the model's turn completed (e.g. "STOP", "MAX_TOKENS").
    #[serde(default)]
    pub turn_complete_reason: Option<String>,
    /// Whether the server is waiting for user input.
    #[serde(default)]
    pub waiting_for_input: Option<bool>,
}

/// Transcription text from server.
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptionPayload {
    /// The transcribed text.
    #[serde(default)]
    pub text: Option<String>,
}

/// Server tool call request message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallMessage {
    /// The tool call payload.
    pub tool_call: ToolCallPayload,
}

/// Payload for tool call.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallPayload {
    /// Function calls requested by the model.
    pub function_calls: Vec<FunctionCall>,
}

/// Server tool call cancellation message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallCancellationMessage {
    /// The tool call cancellation payload.
    pub tool_call_cancellation: ToolCallCancellationPayload,
}

/// Payload for tool call cancellation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallCancellationPayload {
    /// IDs of the cancelled tool calls.
    pub ids: Vec<String>,
}

/// Server GoAway signal — requesting graceful disconnect.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoAwayMessage {
    /// The GoAway payload.
    pub go_away: GoAwayPayload,
}

/// Payload for GoAway.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoAwayPayload {
    /// Time remaining before forced disconnect (e.g. `"30s"`).
    #[serde(default)]
    pub time_left: Option<String>,
}

/// Session resumption update from server (sent during active session).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionUpdateMessage {
    /// The session resumption update payload.
    pub session_resumption_update: SessionResumptionUpdatePayload,
}

/// Payload for session resumption update.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionUpdatePayload {
    /// New opaque handle for session resumption.
    #[serde(default)]
    pub new_handle: Option<String>,
    /// Whether the session is currently resumable.
    #[serde(default)]
    pub resumable: Option<bool>,
    /// Index of the last client message consumed by the server.
    #[serde(default)]
    pub last_consumed_client_message_index: Option<String>,
}

/// Server-side voice activity detection event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceActivityMessage {
    /// The voice activity payload.
    pub voice_activity: VoiceActivityPayload,
}

/// Payload for voice activity detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceActivityPayload {
    /// The type of voice activity event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_activity_type: Option<VoiceActivityType>,
}

/// Type of voice activity event from the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceActivityType {
    /// Voice activity started (user began speaking).
    #[serde(rename = "VOICE_ACTIVITY_START")]
    VoiceActivityStart,
    /// Voice activity ended (user stopped speaking).
    #[serde(rename = "VOICE_ACTIVITY_END")]
    VoiceActivityEnd,
}

/// Server message wrapper — includes optional usage metadata alongside the message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerMessageWrapper {
    /// Token usage metadata (present on most server messages).
    #[serde(default)]
    pub usage_metadata: Option<UsageMetadata>,
}

/// Unified server message enum — parsed from incoming WebSocket text frames.
///
/// We use manual dispatch instead of `#[serde(untagged)]` for performance:
/// untagged tries every variant in order. String-contains + targeted parse
/// is O(1) routing.
#[derive(Debug, Clone)]
pub enum ServerMessage {
    /// Setup handshake completed successfully.
    SetupComplete(SetupCompleteMessage),
    /// Model output content (text, audio, transcription, etc.).
    ServerContent(Box<ServerContentMessage>),
    /// Model requested one or more tool/function calls.
    ToolCall(ToolCallMessage),
    /// Server cancelled previously requested tool calls.
    ToolCallCancellation(ToolCallCancellationMessage),
    /// Server requesting graceful disconnect.
    GoAway(GoAwayMessage),
    /// Updated session resumption handle.
    SessionResumptionUpdate(SessionResumptionUpdateMessage),
    /// Server-side voice activity detection event.
    VoiceActivity(VoiceActivityMessage),
    /// Unrecognized message type (forward compatibility).
    Unknown(serde_json::Value),
}

impl ServerMessage {
    /// Parse a server message from a JSON text frame.
    ///
    /// Uses string-contains routing for O(1) dispatch instead of
    /// serde(untagged)'s O(N) try-all-variants approach.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        if text.contains("\"setupComplete\"") {
            serde_json::from_str::<SetupCompleteMessage>(text).map(ServerMessage::SetupComplete)
        } else if text.contains("\"toolCallCancellation\"") {
            // Must check before "toolCall" since it contains "toolCall" as substring
            serde_json::from_str::<ToolCallCancellationMessage>(text)
                .map(ServerMessage::ToolCallCancellation)
        } else if text.contains("\"toolCall\"") {
            serde_json::from_str::<ToolCallMessage>(text).map(ServerMessage::ToolCall)
        } else if text.contains("\"serverContent\"") {
            serde_json::from_str::<ServerContentMessage>(text).map(|sc| ServerMessage::ServerContent(Box::new(sc)))
        } else if text.contains("\"goAway\"") {
            serde_json::from_str::<GoAwayMessage>(text).map(ServerMessage::GoAway)
        } else if text.contains("\"sessionResumptionUpdate\"") {
            serde_json::from_str::<SessionResumptionUpdateMessage>(text)
                .map(ServerMessage::SessionResumptionUpdate)
        } else if text.contains("\"voiceActivity\"") {
            serde_json::from_str::<VoiceActivityMessage>(text)
                .map(ServerMessage::VoiceActivity)
        } else {
            serde_json::from_str::<serde_json::Value>(text).map(ServerMessage::Unknown)
        }
    }
}
