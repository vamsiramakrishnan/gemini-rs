use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::info;

use adk_rs_fluent::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, WsSender};

use super::{build_session_config, resolve_voice, wait_for_start};

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
        let bridge = crate::bridge::SessionBridge::new(tx);

        let selected_voice = resolve_voice(start.voice.as_deref());
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .system_instruction(
                start.system_instruction.as_deref()
                    .unwrap_or("You are a helpful voice assistant. Keep your responses concise and conversational."),
            );

        let handle = bridge
            .wire_live(Live::builder())
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        bridge.send_connected();
        bridge.send_meta(self);
        info!("VoiceChat session connected");

        bridge.recv_loop(&handle, &mut rx).await;
        Ok(())
    }
}
