use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::prelude::*;

use crate::app::{AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};
use crate::cookbook_meta;

use super::{build_session_config, send_app_meta, wait_for_start};

// ---------------------------------------------------------------------------
// Extended configuration parsed from the Start message
// ---------------------------------------------------------------------------

/// All configurable options for Gemini Live, parsed from the system_instruction
/// field when it contains JSON. Falls back to plain text instruction if parsing fails.
#[derive(Debug, Deserialize, Default)]
struct AllConfigOptions {
    /// The actual system instruction text.
    system_instruction: Option<String>,
    /// Generation temperature (0.0 - 2.0).
    temperature: Option<f32>,
    /// Output modality: "text", "audio", or "both".
    modality: Option<String>,
    /// Voice name for audio output.
    voice: Option<String>,
    /// Enable input audio transcription.
    enable_transcription: Option<bool>,
    /// Enable Google Search grounding.
    enable_google_search: Option<bool>,
    /// Enable code execution.
    enable_code_execution: Option<bool>,
    /// Simple tool definitions.
    tools: Option<Vec<ToolDef>>,
    /// Context window compression target tokens.
    context_window_compression: Option<u32>,
    /// Enable session resumption.
    enable_session_resumption: Option<bool>,
}

/// Simple tool definition from the client.
#[derive(Debug, Deserialize)]
struct ToolDef {
    name: String,
    description: String,
}

/// Parse extended config from the system_instruction field.
/// If the field contains valid JSON matching our schema, extract it.
/// Otherwise, treat the entire string as a plain system instruction.
fn parse_config(raw: Option<&str>, voice_override: Option<&str>) -> AllConfigOptions {
    let mut config = match raw {
        Some(s) => {
            // Try to parse as JSON config.
            match serde_json::from_str::<AllConfigOptions>(s) {
                Ok(c) => c,
                Err(_) => {
                    // Not JSON — treat the whole string as a system instruction.
                    AllConfigOptions {
                        system_instruction: Some(s.to_string()),
                        ..Default::default()
                    }
                }
            }
        }
        None => AllConfigOptions::default(),
    };

    // The voice from the Start message takes precedence over JSON config.
    if voice_override.is_some() {
        config.voice = voice_override.map(|s| s.to_string());
    }

    config
}

/// Resolve a voice string to the Voice enum.
fn resolve_voice(name: Option<&str>) -> Voice {
    match name {
        Some("Aoede") => Voice::Aoede,
        Some("Charon") => Voice::Charon,
        Some("Fenrir") => Voice::Fenrir,
        Some("Kore") => Voice::Kore,
        Some("Puck") | None => Voice::Puck,
        Some(other) => Voice::Custom(other.to_string()),
    }
}

/// Build a summary of the active configuration for sending to the client.
fn config_summary(opts: &AllConfigOptions) -> serde_json::Value {
    json!({
        "system_instruction": opts.system_instruction.as_deref().unwrap_or("(default)"),
        "temperature": opts.temperature,
        "modality": opts.modality.as_deref().unwrap_or("audio"),
        "voice": opts.voice.as_deref().unwrap_or("Puck"),
        "enable_transcription": opts.enable_transcription.unwrap_or(true),
        "enable_google_search": opts.enable_google_search.unwrap_or(false),
        "enable_code_execution": opts.enable_code_execution.unwrap_or(false),
        "tools": opts.tools.as_ref().map(|t| t.iter().map(|d| &d.name).collect::<Vec<_>>()).unwrap_or_default(),
        "context_window_compression": opts.context_window_compression,
        "enable_session_resumption": opts.enable_session_resumption.unwrap_or(false),
    })
}

/// Execute a mock tool call — returns the arguments echoed back as the result.
fn execute_mock_tool(name: &str, args: &serde_json::Value) -> serde_json::Value {
    json!({
        "tool": name,
        "status": "mock_response",
        "echoed_args": args,
        "note": "This is a mock response. The tool was defined dynamically via all-config."
    })
}

// ---------------------------------------------------------------------------
// AllConfig app
// ---------------------------------------------------------------------------

/// Showcase: Configuration playground exposing every Gemini Live option.
pub struct AllConfig;

#[async_trait]
impl CookbookApp for AllConfig {
    cookbook_meta! {
        name: "all-config",
        description: "Configuration playground — every Gemini Live option",
        category: Showcase,
        features: ["text", "voice", "tools", "transcription"],
        tips: [
            "Send JSON as the system instruction to configure: temperature, modality, voice, tools, and more",
            "Supports text-only, audio-only, and both output modalities",
            "Try enabling Google Search or code execution via the JSON config",
        ],
        try_saying: [
            "Hello! (with default audio config)",
            r#"{"modality": "text", "temperature": 1.5}"#,
            "Ask it to search the web (if Google Search enabled)",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;

        // Parse extended config from system_instruction JSON (or plain text fallback).
        let opts = parse_config(start.system_instruction.as_deref(), start.voice.as_deref());

        // Build session config with all specified options.
        let mut config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?;

        // System instruction.
        let instruction = opts.system_instruction.as_deref().unwrap_or(
            "You are a helpful assistant. This session was started via the all-config playground.",
        );
        config = config.system_instruction(instruction);

        // Temperature.
        if let Some(temp) = opts.temperature {
            config = config.temperature(temp);
        }

        // Modality and voice.
        let modality_str = opts.modality.as_deref().unwrap_or("audio");
        let is_audio = modality_str == "audio" || modality_str == "both";
        let is_text = modality_str == "text" || modality_str == "both";

        match modality_str {
            "text" => {
                config = config.text_only();
            }
            "both" => {
                config = config.response_modalities(vec![Modality::Audio, Modality::Text]);
                config = config.voice(resolve_voice(opts.voice.as_deref()));
            }
            _ => {
                // Default: audio only.
                config = config.response_modalities(vec![Modality::Audio]);
                config = config.voice(resolve_voice(opts.voice.as_deref()));
            }
        }

        // Transcription (enabled by default for audio modes).
        if opts.enable_transcription.unwrap_or(is_audio) {
            config = config
                .enable_input_transcription()
                .enable_output_transcription();
        }

        // Google Search.
        if opts.enable_google_search.unwrap_or(false) {
            config = config.with_google_search();
        }

        // Code execution.
        if opts.enable_code_execution.unwrap_or(false) {
            config = config.with_code_execution();
        }

        // Custom tools.
        if let Some(ref tool_defs) = opts.tools {
            if !tool_defs.is_empty() {
                let declarations: Vec<FunctionDeclaration> = tool_defs
                    .iter()
                    .map(|td| FunctionDeclaration {
                        name: td.name.clone(),
                        description: td.description.clone(),
                        parameters: Some(json!({
                            "type": "object",
                            "properties": {
                                "input": {
                                    "type": "string",
                                    "description": "Input for the tool"
                                }
                            }
                        })),
                        behavior: Some(FunctionCallingBehavior::NonBlocking),
                    })
                    .collect();
                config = config.add_tool(Tool::functions(declarations));
            }
        }

        // Context window compression.
        if let Some(target_tokens) = opts.context_window_compression {
            config = config.context_window_compression(target_tokens);
        }

        // Session resumption.
        if opts.enable_session_resumption.unwrap_or(false) {
            config = config.session_resumption(None);
        }

        // Build Live session with all callbacks.
        let tx_audio = tx.clone();
        let tx_input = tx.clone();
        let tx_output = tx.clone();
        let tx_text = tx.clone();
        let tx_text_complete = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupted = tx.clone();
        let tx_vad_start = tx.clone();
        let tx_vad_end = tx.clone();
        let tx_error = tx.clone();
        let tx_disconnected = tx.clone();
        let tx_tool = tx.clone();

        let handle = Live::builder()
            .on_audio(move |data| {
                let _ = tx_audio.send(ServerMessage::Audio {
                    data: data.to_vec(),
                });
            })
            .on_input_transcript(move |text, _is_final| {
                let _ = tx_input.send(ServerMessage::InputTranscription {
                    text: text.to_string(),
                });
            })
            .on_output_transcript(move |text, _is_final| {
                let _ = tx_output.send(ServerMessage::OutputTranscription {
                    text: text.to_string(),
                });
            })
            .on_text(move |t| {
                let _ = tx_text.send(ServerMessage::TextDelta {
                    text: t.to_string(),
                });
            })
            .on_text_complete(move |t| {
                let _ = tx_text_complete.send(ServerMessage::TextComplete {
                    text: t.to_string(),
                });
            })
            .on_tool_call(move |calls, _state| {
                let tx = tx_tool.clone();
                async move {
                    info!("Tool calls received: {}", calls.len());

                    let responses: Vec<FunctionResponse> = calls
                        .iter()
                        .map(|call| {
                            let result = execute_mock_tool(&call.name, &call.args);
                            info!("Mock tool '{}' -> {}", call.name, result);

                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: format!("tool:{}", call.name),
                                value: json!({
                                    "name": call.name,
                                    "args": call.args,
                                    "result": result,
                                }),
                            });

                            FunctionResponse {
                                name: call.name.clone(),
                                response: result,
                                id: call.id.clone(),
                                scheduling: Some(FunctionResponseScheduling::WhenIdle),
                            }
                        })
                        .collect();

                    Some(responses)
                }
            })
            .on_turn_complete(move || {
                let tx = tx_turn.clone();
                async move {
                    let _ = tx.send(ServerMessage::TurnComplete);
                }
            })
            .on_interrupted(move || {
                let tx = tx_interrupted.clone();
                async move {
                    let _ = tx.send(ServerMessage::Interrupted);
                }
            })
            .on_vad_start(move || {
                let _ = tx_vad_start.send(ServerMessage::VoiceActivityStart);
            })
            .on_vad_end(move || {
                let _ = tx_vad_end.send(ServerMessage::VoiceActivityEnd);
            })
            .on_error(move |msg| {
                let tx = tx_error.clone();
                async move {
                    let _ = tx.send(ServerMessage::Error { message: msg });
                }
            })
            .on_disconnected(move |_reason| {
                let _tx = tx_disconnected.clone();
                async move {
                    info!("AllConfig session disconnected by server");
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("AllConfig session connected");

        // Send active configuration to the client.
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "config".into(),
            value: config_summary(&opts),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "modality".into(),
            value: json!(modality_str),
        });

        // Periodic telemetry
        let telem = handle.telemetry().clone();
        let tx_telem = tx.clone();
        let telem_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                interval.tick().await;
                let stats = telem.snapshot();
                if tx_telem.send(ServerMessage::Telemetry { stats }).is_err() {
                    break;
                }
            }
        });

        // Browser -> Gemini loop.
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Audio { data } => {
                    if is_audio {
                        match b64.decode(&data) {
                            Ok(pcm_bytes) => {
                                if let Err(e) = handle.send_audio(pcm_bytes).await {
                                    warn!("Failed to send audio: {e}");
                                    let _ = tx.send(ServerMessage::Error {
                                        message: e.to_string(),
                                    });
                                }
                            }
                            Err(e) => {
                                warn!("Failed to decode base64 audio: {e}");
                            }
                        }
                    }
                }
                ClientMessage::Text { text } => {
                    if is_text || is_audio {
                        if let Err(e) = handle.send_text(&text).await {
                            warn!("Failed to send text: {e}");
                            let _ = tx.send(ServerMessage::Error {
                                message: e.to_string(),
                            });
                        }
                    }
                }
                ClientMessage::Stop => {
                    info!("AllConfig session stopping");
                    telem_task.abort();
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppCategory;

    #[test]
    fn parse_json_config() {
        let json =
            r#"{"system_instruction": "Be helpful", "temperature": 0.9, "modality": "text"}"#;
        let config = parse_config(Some(json), None);
        assert_eq!(config.system_instruction.as_deref(), Some("Be helpful"));
        assert_eq!(config.temperature, Some(0.9));
        assert_eq!(config.modality.as_deref(), Some("text"));
    }

    #[test]
    fn parse_plain_text_fallback() {
        let plain = "You are a pirate. Speak only in pirate dialect.";
        let config = parse_config(Some(plain), None);
        assert_eq!(config.system_instruction.as_deref(), Some(plain));
        assert_eq!(config.temperature, None);
        assert_eq!(config.modality, None);
    }

    #[test]
    fn parse_none_input() {
        let config = parse_config(None, None);
        assert_eq!(config.system_instruction, None);
        assert_eq!(config.temperature, None);
    }

    #[test]
    fn voice_override_takes_precedence() {
        let json = r#"{"voice": "Kore"}"#;
        let config = parse_config(Some(json), Some("Charon"));
        assert_eq!(config.voice.as_deref(), Some("Charon"));
    }

    #[test]
    fn parse_with_tools() {
        let json = r#"{"tools": [{"name": "lookup", "description": "Look up data"}]}"#;
        let config = parse_config(Some(json), None);
        let tools = config.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "lookup");
    }

    #[test]
    fn parse_all_options() {
        let json = r#"{
            "system_instruction": "Test",
            "temperature": 0.5,
            "modality": "both",
            "voice": "Fenrir",
            "enable_transcription": true,
            "enable_google_search": true,
            "enable_code_execution": true,
            "context_window_compression": 1024,
            "enable_session_resumption": true
        }"#;
        let config = parse_config(Some(json), None);
        assert_eq!(config.system_instruction.as_deref(), Some("Test"));
        assert_eq!(config.temperature, Some(0.5));
        assert_eq!(config.modality.as_deref(), Some("both"));
        assert_eq!(config.voice.as_deref(), Some("Fenrir"));
        assert_eq!(config.enable_transcription, Some(true));
        assert_eq!(config.enable_google_search, Some(true));
        assert_eq!(config.enable_code_execution, Some(true));
        assert_eq!(config.context_window_compression, Some(1024));
        assert_eq!(config.enable_session_resumption, Some(true));
    }

    #[test]
    fn config_summary_defaults() {
        let opts = AllConfigOptions::default();
        let summary = config_summary(&opts);
        assert_eq!(summary["modality"], "audio");
        assert_eq!(summary["voice"], "Puck");
        assert_eq!(summary["enable_google_search"], false);
    }

    #[test]
    fn mock_tool_execution() {
        let args = json!({"input": "hello"});
        let result = execute_mock_tool("test_tool", &args);
        assert_eq!(result["tool"], "test_tool");
        assert_eq!(result["status"], "mock_response");
        assert_eq!(result["echoed_args"], args);
    }

    #[test]
    fn resolve_known_voices() {
        assert!(matches!(resolve_voice(Some("Aoede")), Voice::Aoede));
        assert!(matches!(resolve_voice(Some("Charon")), Voice::Charon));
        assert!(matches!(resolve_voice(Some("Fenrir")), Voice::Fenrir));
        assert!(matches!(resolve_voice(Some("Kore")), Voice::Kore));
        assert!(matches!(resolve_voice(Some("Puck")), Voice::Puck));
        assert!(matches!(resolve_voice(None), Voice::Puck));
    }

    #[test]
    fn resolve_custom_voice() {
        match resolve_voice(Some("CustomVoice")) {
            Voice::Custom(name) => assert_eq!(name, "CustomVoice"),
            _ => panic!("Expected Custom voice"),
        }
    }

    #[test]
    fn app_metadata() {
        let app = AllConfig;
        assert_eq!(app.name(), "all-config");
        assert_eq!(app.category(), AppCategory::Showcase);
        assert!(app.features().contains(&"tools".to_string()));
        assert!(app.features().contains(&"voice".to_string()));
        assert!(app.features().contains(&"text".to_string()));
        assert!(app.features().contains(&"transcription".to_string()));
    }
}
