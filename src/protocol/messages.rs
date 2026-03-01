//! Client→Server and Server→Client message envelopes for the Gemini Live wire protocol.

use serde::{Deserialize, Serialize};

use super::types::*;

// ---------------------------------------------------------------------------
// Client → Server messages
// ---------------------------------------------------------------------------

/// Top-level setup message sent immediately after WebSocket connect.
#[derive(Debug, Clone, Serialize)]
pub struct SetupMessage {
    pub setup: SetupPayload,
}

/// Payload of the setup message.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupPayload {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_audio_transcription: Option<InputAudioTranscription>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_audio_transcription: Option<OutputAudioTranscription>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_input_config: Option<RealtimeInputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_resumption: Option<SessionResumptionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_compression: Option<ContextWindowCompressionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proactivity: Option<ProactivityConfig>,
}

impl SessionConfig {
    /// Build the setup message from this configuration.
    pub fn to_setup_message(&self) -> SetupMessage {
        SetupMessage {
            setup: SetupPayload {
                model: self.model_uri(),
                generation_config: Some(self.generation_config.clone()),
                system_instruction: self.system_instruction.clone(),
                tools: self.tools.clone(),
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
    pub mime_type: String,
    pub data: String, // base64-encoded
}

/// Client content message for sending text or conversation history.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientContentMessage {
    pub client_content: ClientContentPayload,
}

/// Payload for client content.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientContentPayload {
    pub turns: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_complete: Option<bool>,
}

/// Tool response message sent after executing function calls.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResponseMessage {
    pub tool_response: ToolResponsePayload,
}

/// Payload for tool response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResponsePayload {
    pub function_responses: Vec<FunctionResponse>,
}

/// Activity signal for client-side VAD events.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySignalMessage {
    pub realtime_input: ActivitySignalPayload,
}

/// Payload for activity signals.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySignalPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_start: Option<ActivityStart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_end: Option<ActivityEnd>,
}

/// Marker for speech activity start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityStart {}

/// Marker for speech activity end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEnd {}

// ---------------------------------------------------------------------------
// Server → Client messages
// ---------------------------------------------------------------------------

/// Server setup complete acknowledgment.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCompleteMessage {
    pub setup_complete: SetupCompletePayload,
}

/// Payload for setup complete.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCompletePayload {
    #[serde(default)]
    pub session_resumption: Option<SessionResumptionResult>,
}

/// Session resumption result from server.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionResult {
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub resumed: Option<bool>,
}

/// Server content message containing model output.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerContentMessage {
    pub server_content: ServerContentPayload,
}

/// Payload for server content.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerContentPayload {
    #[serde(default)]
    pub model_turn: Option<Content>,
    #[serde(default)]
    pub turn_complete: Option<bool>,
    #[serde(default)]
    pub generation_complete: Option<bool>,
    #[serde(default)]
    pub interrupted: Option<bool>,
    #[serde(default)]
    pub input_transcription: Option<TranscriptionPayload>,
    #[serde(default)]
    pub output_transcription: Option<TranscriptionPayload>,
    #[serde(default)]
    pub grounding_metadata: Option<GroundingMetadata>,
    #[serde(default)]
    pub url_context_metadata: Option<UrlContextMetadata>,
}

/// Transcription text from server.
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptionPayload {
    #[serde(default)]
    pub text: Option<String>,
}

/// Server tool call request message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallMessage {
    pub tool_call: ToolCallPayload,
}

/// Payload for tool call.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallPayload {
    pub function_calls: Vec<FunctionCall>,
}

/// Server tool call cancellation message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallCancellationMessage {
    pub tool_call_cancellation: ToolCallCancellationPayload,
}

/// Payload for tool call cancellation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallCancellationPayload {
    pub ids: Vec<String>,
}

/// Server GoAway signal — requesting graceful disconnect.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoAwayMessage {
    pub go_away: GoAwayPayload,
}

/// Payload for GoAway.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoAwayPayload {
    #[serde(default)]
    pub time_left: Option<String>,
}

/// Session resumption update from server (sent during active session).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionUpdateMessage {
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
    SetupComplete(SetupCompleteMessage),
    ServerContent(ServerContentMessage),
    ToolCall(ToolCallMessage),
    ToolCallCancellation(ToolCallCancellationMessage),
    GoAway(GoAwayMessage),
    SessionResumptionUpdate(SessionResumptionUpdateMessage),
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
            serde_json::from_str::<ServerContentMessage>(text).map(ServerMessage::ServerContent)
        } else if text.contains("\"goAway\"") {
            serde_json::from_str::<GoAwayMessage>(text).map(ServerMessage::GoAway)
        } else if text.contains("\"sessionResumptionUpdate\"") {
            serde_json::from_str::<SessionResumptionUpdateMessage>(text)
                .map(ServerMessage::SessionResumptionUpdate)
        } else {
            serde_json::from_str::<serde_json::Value>(text).map(ServerMessage::Unknown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_message_serialization() {
        let config = SessionConfig::new("test-key")
            .model(GeminiModel::Gemini2_0FlashLive)
            .voice(Voice::Kore)
            .system_instruction("You are a helpful assistant.");

        let json = config.to_setup_json();
        assert!(json.contains("\"setup\""));
        assert!(json.contains("\"generationConfig\""));
        assert!(json.contains("\"Kore\""));
        assert!(json.contains("\"systemInstruction\""));
    }

    #[test]
    fn parse_setup_complete() {
        let json = r#"{"setupComplete":{"sessionResumption":{"handle":"abc123"}}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::SetupComplete(sc) => {
                let handle = sc.setup_complete.session_resumption.unwrap().handle;
                assert_eq!(handle, Some("abc123".to_string()));
            }
            _ => panic!("Expected SetupComplete"),
        }
    }

    #[test]
    fn parse_server_content_text() {
        let json = r#"{
            "serverContent": {
                "modelTurn": {
                    "parts": [{"text": "Hello! How can I help?"}]
                },
                "turnComplete": true
            }
        }"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::ServerContent(sc) => {
                assert!(sc.server_content.turn_complete.unwrap_or(false));
                let turn = sc.server_content.model_turn.unwrap();
                assert_eq!(turn.parts.len(), 1);
                match &turn.parts[0] {
                    Part::Text { text } => assert_eq!(text, "Hello! How can I help?"),
                    _ => panic!("Expected text part"),
                }
            }
            _ => panic!("Expected ServerContent"),
        }
    }

    #[test]
    fn parse_server_content_audio() {
        let json = r#"{
            "serverContent": {
                "modelTurn": {
                    "parts": [{"inlineData": {"mimeType": "audio/pcm", "data": "AAAA"}}]
                }
            }
        }"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::ServerContent(sc) => {
                let turn = sc.server_content.model_turn.unwrap();
                match &turn.parts[0] {
                    Part::InlineData { inline_data } => {
                        assert_eq!(inline_data.mime_type, "audio/pcm");
                    }
                    _ => panic!("Expected inline data part"),
                }
            }
            _ => panic!("Expected ServerContent"),
        }
    }

    #[test]
    fn parse_tool_call() {
        let json = r#"{
            "toolCall": {
                "functionCalls": [
                    {"name": "get_weather", "args": {"city": "London"}, "id": "call-1"}
                ]
            }
        }"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::ToolCall(tc) => {
                assert_eq!(tc.tool_call.function_calls.len(), 1);
                assert_eq!(tc.tool_call.function_calls[0].name, "get_weather");
            }
            _ => panic!("Expected ToolCall"),
        }
    }

    #[test]
    fn parse_tool_call_cancellation() {
        let json = r#"{"toolCallCancellation": {"ids": ["call-1", "call-2"]}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::ToolCallCancellation(tc) => {
                assert_eq!(tc.tool_call_cancellation.ids, vec!["call-1", "call-2"]);
            }
            _ => panic!("Expected ToolCallCancellation"),
        }
    }

    #[test]
    fn parse_go_away() {
        let json = r#"{"goAway": {"timeLeft": "30s"}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::GoAway(ga) => {
                assert_eq!(ga.go_away.time_left, Some("30s".to_string()));
            }
            _ => panic!("Expected GoAway"),
        }
    }

    #[test]
    fn parse_interrupted() {
        let json = r#"{"serverContent": {"interrupted": true}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::ServerContent(sc) => {
                assert!(sc.server_content.interrupted.unwrap_or(false));
            }
            _ => panic!("Expected ServerContent"),
        }
    }

    #[test]
    fn parse_unknown_message() {
        let json = r#"{"newFeature": {"value": 42}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        assert!(matches!(msg, ServerMessage::Unknown(_)));
    }

    #[test]
    fn realtime_input_serialization_audio() {
        let msg = RealtimeInputMessage {
            realtime_input: RealtimeInputPayload {
                media_chunks: Vec::new(),
                audio: Some(Blob {
                    mime_type: "audio/pcm".to_string(),
                    data: "AQIDBA==".to_string(),
                }),
                video: None,
                audio_stream_end: None,
                text: None,
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"realtimeInput\""));
        assert!(json.contains("\"audio\""));
        assert!(json.contains("\"mimeType\""));
        // Deprecated field should not appear when empty
        assert!(!json.contains("\"mediaChunks\""));
    }

    #[test]
    fn realtime_input_serialization_legacy() {
        let msg = RealtimeInputMessage {
            realtime_input: RealtimeInputPayload {
                media_chunks: vec![MediaChunk {
                    mime_type: "audio/pcm".to_string(),
                    data: "AQIDBA==".to_string(),
                }],
                audio: None,
                video: None,
                audio_stream_end: None,
                text: None,
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"mediaChunks\""));
    }

    #[test]
    fn parse_session_resumption_update() {
        let json = r#"{"sessionResumptionUpdate": {"newHandle": "handle-xyz", "resumable": true}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::SessionResumptionUpdate(sru) => {
                assert_eq!(
                    sru.session_resumption_update.new_handle,
                    Some("handle-xyz".to_string())
                );
                assert_eq!(sru.session_resumption_update.resumable, Some(true));
            }
            _ => panic!("Expected SessionResumptionUpdate"),
        }
    }

    #[test]
    fn tool_response_serialization() {
        let msg = ToolResponseMessage {
            tool_response: ToolResponsePayload {
                function_responses: vec![FunctionResponse {
                    name: "get_weather".to_string(),
                    response: serde_json::json!({"temp": 22}),
                    id: Some("call-1".to_string()),
                }],
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"toolResponse\""));
        assert!(json.contains("\"functionResponses\""));
    }

    #[test]
    fn client_content_serialization() {
        let msg = ClientContentMessage {
            client_content: ClientContentPayload {
                turns: vec![Content {
                    role: Some("user".to_string()),
                    parts: vec![Part::Text {
                        text: "Hello".to_string(),
                    }],
                }],
                turn_complete: Some(true),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"clientContent\""));
        assert!(json.contains("\"turnComplete\""));
    }

    #[test]
    fn activity_signal_serialization() {
        let msg = ActivitySignalMessage {
            realtime_input: ActivitySignalPayload {
                activity_start: Some(ActivityStart {}),
                activity_end: None,
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"activityStart\""));
    }

    #[test]
    fn parse_input_transcription() {
        let json = r#"{
            "serverContent": {
                "inputTranscription": {"text": "Hello world"}
            }
        }"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::ServerContent(sc) => {
                let text = sc
                    .server_content
                    .input_transcription
                    .unwrap()
                    .text
                    .unwrap();
                assert_eq!(text, "Hello world");
            }
            _ => panic!("Expected ServerContent"),
        }
    }
}
