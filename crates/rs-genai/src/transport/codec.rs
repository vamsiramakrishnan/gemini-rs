//! Message codec — encode commands, decode server messages.

use base64::Engine;

use crate::protocol::messages::*;
use crate::protocol::types::*;
use crate::session::SessionCommand;

/// Error during encoding or decoding.
#[derive(Debug, thiserror::Error, Clone)]
pub enum CodecError {
    #[error("Serialization error: {0}")]
    Serialize(String),
    #[error("Deserialization error: {0}")]
    Deserialize(String),
    #[error("Invalid UTF-8")]
    InvalidUtf8,
}

/// Encodes client commands into wire bytes and decodes server bytes into messages.
pub trait Codec: Send + Sync + 'static {
    /// Encode the initial setup message for the given session configuration.
    fn encode_setup(&self, config: &SessionConfig) -> Result<Vec<u8>, CodecError>;
    /// Encode a session command into wire bytes.
    fn encode_command(
        &self,
        cmd: &SessionCommand,
        config: &SessionConfig,
    ) -> Result<Vec<u8>, CodecError>;
    /// Decode raw bytes from the server into a `ServerMessage`.
    fn decode_message(&self, data: &[u8]) -> Result<ServerMessage, CodecError>;
}

/// Default JSON codec — current behavior extracted from connection.rs.
pub struct JsonCodec;

impl Codec for JsonCodec {
    fn encode_setup(&self, config: &SessionConfig) -> Result<Vec<u8>, CodecError> {
        serde_json::to_vec(&config.to_setup_message())
            .map_err(|e| CodecError::Serialize(e.to_string()))
    }

    fn encode_command(
        &self,
        cmd: &SessionCommand,
        config: &SessionConfig,
    ) -> Result<Vec<u8>, CodecError> {
        match cmd {
            SessionCommand::SendAudio(data) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                let msg = RealtimeInputMessage {
                    realtime_input: RealtimeInputPayload {
                        media_chunks: Vec::new(),
                        audio: Some(Blob {
                            mime_type: config.input_audio_format.mime_type().to_string(),
                            data: encoded,
                        }),
                        video: None,
                        audio_stream_end: None,
                        text: None,
                    },
                };
                serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
            }
            SessionCommand::SendText(text) => {
                let msg = ClientContentMessage {
                    client_content: ClientContentPayload {
                        turns: vec![Content::user(text)],
                        turn_complete: Some(true),
                    },
                };
                serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
            }
            SessionCommand::SendToolResponse(responses) => {
                let msg = ToolResponseMessage {
                    tool_response: ToolResponsePayload {
                        function_responses: responses.clone(),
                    },
                };
                serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
            }
            SessionCommand::ActivityStart => {
                let msg = ActivitySignalMessage {
                    realtime_input: ActivitySignalPayload {
                        activity_start: Some(ActivityStart {}),
                        activity_end: None,
                    },
                };
                serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
            }
            SessionCommand::ActivityEnd => {
                let msg = ActivitySignalMessage {
                    realtime_input: ActivitySignalPayload {
                        activity_start: None,
                        activity_end: Some(ActivityEnd {}),
                    },
                };
                serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
            }
            SessionCommand::SendClientContent {
                turns,
                turn_complete,
            } => {
                let msg = ClientContentMessage {
                    client_content: ClientContentPayload {
                        turns: turns.clone(),
                        turn_complete: Some(*turn_complete),
                    },
                };
                serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
            }
            SessionCommand::SendVideo(data) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                let msg = RealtimeInputMessage {
                    realtime_input: RealtimeInputPayload {
                        media_chunks: Vec::new(),
                        audio: None,
                        video: Some(Blob {
                            mime_type: "image/jpeg".to_string(),
                            data: encoded,
                        }),
                        audio_stream_end: None,
                        text: None,
                    },
                };
                serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
            }
            SessionCommand::UpdateInstruction(instruction) => {
                let msg = ClientContentMessage {
                    client_content: ClientContentPayload {
                        turns: vec![Content {
                            role: Some(Role::System),
                            parts: vec![Part::Text { text: instruction.clone() }],
                        }],
                        turn_complete: Some(false),
                    },
                };
                serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
            }
            SessionCommand::Disconnect => Ok(Vec::new()),
        }
    }

    fn decode_message(&self, data: &[u8]) -> Result<ServerMessage, CodecError> {
        let text = std::str::from_utf8(data).map_err(|_| CodecError::InvalidUtf8)?;
        ServerMessage::parse(text).map_err(|e| CodecError::Deserialize(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SessionConfig {
        SessionConfig::new("test-key")
            .model(GeminiModel::Gemini2_0FlashLive)
            .voice(Voice::Puck)
    }

    // -----------------------------------------------------------------------
    // Encode tests
    // -----------------------------------------------------------------------

    #[test]
    fn json_codec_encode_setup() {
        let codec = JsonCodec;
        let config = test_config();
        let bytes = codec.encode_setup(&config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(json.contains("\"setup\""), "should contain setup key");
        assert!(
            json.contains("gemini-2.0-flash-live-001"),
            "should contain model name"
        );
    }

    #[test]
    fn json_codec_encode_send_text() {
        let codec = JsonCodec;
        let config = test_config();
        let cmd = SessionCommand::SendText("Hello, world!".to_string());
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains("\"clientContent\""),
            "should contain clientContent"
        );
        assert!(
            json.contains("Hello, world!"),
            "should contain the text payload"
        );
        assert!(
            json.contains("\"turnComplete\""),
            "should contain turnComplete"
        );
    }

    #[test]
    fn json_codec_encode_send_audio() {
        let codec = JsonCodec;
        let config = test_config();
        let audio_data = vec![1u8, 2, 3, 4];
        let cmd = SessionCommand::SendAudio(audio_data);
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains("\"realtimeInput\""),
            "should contain realtimeInput"
        );
        assert!(json.contains("\"audio\""), "should contain audio field");
        assert!(
            json.contains("audio/pcm"),
            "should contain the audio mime type"
        );
        // base64 of [1,2,3,4] is "AQIDBA=="
        assert!(json.contains("AQIDBA=="), "should contain base64-encoded data");
    }

    #[test]
    fn json_codec_encode_tool_response() {
        let codec = JsonCodec;
        let config = test_config();
        let cmd = SessionCommand::SendToolResponse(vec![FunctionResponse {
            name: "get_weather".to_string(),
            response: serde_json::json!({"temp": 22}),
            id: Some("call-1".to_string()),
        }]);
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains("\"toolResponse\""),
            "should contain toolResponse"
        );
        assert!(
            json.contains("\"functionResponses\""),
            "should contain functionResponses"
        );
        assert!(
            json.contains("get_weather"),
            "should contain the function name"
        );
    }

    #[test]
    fn json_codec_encode_activity_start() {
        let codec = JsonCodec;
        let config = test_config();
        let cmd = SessionCommand::ActivityStart;
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains("\"activityStart\""),
            "should contain activityStart"
        );
        assert!(
            !json.contains("\"activityEnd\""),
            "should not contain activityEnd"
        );
    }

    #[test]
    fn json_codec_encode_activity_end() {
        let codec = JsonCodec;
        let config = test_config();
        let cmd = SessionCommand::ActivityEnd;
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains("\"activityEnd\""),
            "should contain activityEnd"
        );
        assert!(
            !json.contains("\"activityStart\""),
            "should not contain activityStart"
        );
    }

    #[test]
    fn json_codec_encode_client_content() {
        let codec = JsonCodec;
        let config = test_config();
        let cmd = SessionCommand::SendClientContent {
            turns: vec![Content::user("context message")],
            turn_complete: false,
        };
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains("\"clientContent\""),
            "should contain clientContent"
        );
        assert!(
            json.contains("context message"),
            "should contain the text content"
        );
        assert!(
            json.contains("\"turnComplete\":false"),
            "should contain turnComplete set to false"
        );
    }

    #[test]
    fn json_codec_encode_send_video() {
        let codec = JsonCodec;
        let config = test_config();
        let cmd = SessionCommand::SendVideo(vec![0xFF, 0xD8, 0xFF]); // JPEG magic bytes
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["realtimeInput"]["video"]["mimeType"].as_str().unwrap(),
            "image/jpeg"
        );
        assert!(json["realtimeInput"]["video"]["data"].is_string());
    }

    #[test]
    fn json_codec_encode_update_instruction() {
        let codec = JsonCodec;
        let config = test_config();
        let cmd = SessionCommand::UpdateInstruction("New instruction".into());
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let turns = &json["clientContent"]["turns"];
        assert_eq!(turns[0]["role"], "system");
        assert_eq!(turns[0]["parts"][0]["text"], "New instruction");
    }

    #[test]
    fn json_codec_encode_disconnect() {
        let codec = JsonCodec;
        let config = test_config();
        let cmd = SessionCommand::Disconnect;
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        assert!(bytes.is_empty(), "Disconnect should produce empty bytes");
    }

    // -----------------------------------------------------------------------
    // Decode tests
    // -----------------------------------------------------------------------

    #[test]
    fn json_codec_decode_setup_complete() {
        let codec = JsonCodec;
        let json = r#"{"setupComplete":{"sessionResumption":{"handle":"abc123"}}}"#;
        let msg = codec.decode_message(json.as_bytes()).unwrap();
        match msg {
            ServerMessage::SetupComplete(sc) => {
                let handle = sc.setup_complete.session_resumption.unwrap().handle;
                assert_eq!(handle, Some("abc123".to_string()));
            }
            _ => panic!("Expected SetupComplete"),
        }
    }

    #[test]
    fn json_codec_decode_server_content() {
        let codec = JsonCodec;
        let json = r#"{
            "serverContent": {
                "modelTurn": {
                    "parts": [{"text": "Hello! How can I help?"}]
                },
                "turnComplete": true
            }
        }"#;
        let msg = codec.decode_message(json.as_bytes()).unwrap();
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
    fn json_codec_decode_tool_call() {
        let codec = JsonCodec;
        let json = r#"{
            "toolCall": {
                "functionCalls": [
                    {"name": "get_weather", "args": {"city": "London"}, "id": "call-1"}
                ]
            }
        }"#;
        let msg = codec.decode_message(json.as_bytes()).unwrap();
        match msg {
            ServerMessage::ToolCall(tc) => {
                assert_eq!(tc.tool_call.function_calls.len(), 1);
                assert_eq!(tc.tool_call.function_calls[0].name, "get_weather");
            }
            _ => panic!("Expected ToolCall"),
        }
    }

    #[test]
    fn json_codec_decode_invalid_utf8() {
        let codec = JsonCodec;
        let bad_bytes: &[u8] = &[0xFF, 0xFE, 0xFD];
        let result = codec.decode_message(bad_bytes);
        match result {
            Err(CodecError::InvalidUtf8) => {} // expected
            other => panic!("Expected CodecError::InvalidUtf8, got {:?}", other),
        }
    }
}
