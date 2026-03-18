//! Client→Server and Server→Client message envelopes for the Gemini Live wire protocol.

pub mod client;
pub mod server;

pub use client::*;
pub use server::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::*;

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
                    scheduling: None,
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
                turns: vec![Content::user("Hello")],
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
    fn voice_activity_type_serialization() {
        let start = VoiceActivityType::VoiceActivityStart;
        let json = serde_json::to_string(&start).unwrap();
        assert_eq!(json, "\"VOICE_ACTIVITY_START\"");
        let parsed: VoiceActivityType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, start);

        let end = VoiceActivityType::VoiceActivityEnd;
        let json = serde_json::to_string(&end).unwrap();
        assert_eq!(json, "\"VOICE_ACTIVITY_END\"");
        let parsed: VoiceActivityType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, end);
    }

    #[test]
    fn parse_voice_activity_message() {
        let json = r#"{"voiceActivity":{"voiceActivityType":"VOICE_ACTIVITY_START"}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::VoiceActivity(va) => {
                assert_eq!(
                    va.voice_activity.voice_activity_type,
                    Some(VoiceActivityType::VoiceActivityStart)
                );
            }
            _ => panic!("Expected VoiceActivity"),
        }

        let json = r#"{"voiceActivity":{"voiceActivityType":"VOICE_ACTIVITY_END"}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        match msg {
            ServerMessage::VoiceActivity(va) => {
                assert_eq!(
                    va.voice_activity.voice_activity_type,
                    Some(VoiceActivityType::VoiceActivityEnd)
                );
            }
            _ => panic!("Expected VoiceActivity"),
        }
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
                let text = sc.server_content.input_transcription.unwrap().text.unwrap();
                assert_eq!(text, "Hello world");
            }
            _ => panic!("Expected ServerContent"),
        }
    }
}
