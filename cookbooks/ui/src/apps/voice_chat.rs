use async_trait::async_trait;
use base64::Engine;
use tokio::sync::mpsc;
use tracing::{info, warn};

use rs_genai::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::{build_session_config, send_app_meta, wait_for_start};

/// Native audio voice chat with Gemini Live.
pub struct VoiceChat;

#[async_trait]
impl CookbookApp for VoiceChat {
    fn name(&self) -> &str {
        "voice-chat"
    }

    fn description(&self) -> &str {
        "Native audio voice chat with Gemini Live"
    }

    fn category(&self) -> AppCategory {
        AppCategory::Basic
    }

    fn features(&self) -> Vec<String> {
        vec!["voice".into(), "transcription".into()]
    }

    fn tips(&self) -> Vec<String> {
        vec![
            "Click the microphone button to start speaking".into(),
            "Transcriptions appear below each message showing what was said".into(),
            "You can also type text — the model will respond with voice".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "Hello! Tell me a joke.".into(),
            "What's the weather like on Mars?".into(),
            "Can you sing a short song?".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;

        // Resolve voice selection (default to Puck).
        let selected_voice = match start.voice.as_deref() {
            Some("Aoede") => Voice::Aoede,
            Some("Charon") => Voice::Charon,
            Some("Fenrir") => Voice::Fenrir,
            Some("Kore") => Voice::Kore,
            Some("Puck") | None => Voice::Puck,
            Some(other) => Voice::Custom(other.to_string()),
        };

        // Build session config for voice mode.
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .system_instruction(
                start
                    .system_instruction
                    .as_deref()
                    .unwrap_or("You are a helpful voice assistant. Keep your responses concise and conversational."),
            );

        // Connect to Gemini Live.
        let handle = ConnectBuilder::new(config)
            .build()
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        handle.wait_for_phase(SessionPhase::Active).await;
        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("VoiceChat session connected");

        // Subscribe to server events.
        let mut events = handle.subscribe();
        let b64 = base64::engine::general_purpose::STANDARD;

        loop {
            tokio::select! {
                // Client -> Gemini
                client_msg = rx.recv() => {
                    match client_msg {
                        Some(ClientMessage::Audio { data }) => {
                            match b64.decode(&data) {
                                Ok(pcm_bytes) => {
                                    if let Err(e) = handle.send_audio(pcm_bytes).await {
                                        warn!("Failed to send audio: {e}");
                                        let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to decode base64 audio: {e}");
                                }
                            }
                        }
                        Some(ClientMessage::Text { text }) => {
                            // Voice chat also supports text input.
                            if let Err(e) = handle.send_text(&text).await {
                                warn!("Failed to send text: {e}");
                                let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                            }
                        }
                        Some(ClientMessage::Stop) | None => {
                            info!("VoiceChat session stopping");
                            let _ = handle.disconnect().await;
                            break;
                        }
                        _ => {}
                    }
                }

                // Gemini -> Client
                event = recv_event(&mut events) => {
                    match event {
                        Some(SessionEvent::AudioData(bytes)) => {
                            let encoded = b64.encode(&bytes);
                            let _ = tx.send(ServerMessage::Audio { data: encoded });
                        }
                        Some(SessionEvent::InputTranscription(text)) => {
                            let _ = tx.send(ServerMessage::InputTranscription { text });
                        }
                        Some(SessionEvent::OutputTranscription(text)) => {
                            let _ = tx.send(ServerMessage::OutputTranscription { text });
                        }
                        Some(SessionEvent::TextDelta(text)) => {
                            let _ = tx.send(ServerMessage::TextDelta { text });
                        }
                        Some(SessionEvent::TextComplete(text)) => {
                            let _ = tx.send(ServerMessage::TextComplete { text });
                        }
                        Some(SessionEvent::TurnComplete) => {
                            let _ = tx.send(ServerMessage::TurnComplete);
                        }
                        Some(SessionEvent::Interrupted) => {
                            let _ = tx.send(ServerMessage::Interrupted);
                        }
                        Some(SessionEvent::VoiceActivityStart) => {
                            let _ = tx.send(ServerMessage::VoiceActivityStart);
                        }
                        Some(SessionEvent::VoiceActivityEnd) => {
                            let _ = tx.send(ServerMessage::VoiceActivityEnd);
                        }
                        Some(SessionEvent::Error(msg)) => {
                            let _ = tx.send(ServerMessage::Error { message: msg });
                        }
                        Some(SessionEvent::Disconnected(_)) => {
                            info!("VoiceChat session disconnected by server");
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }
}
