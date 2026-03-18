use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::info;

use adk_rs_fluent::prelude::*;

use crate::app::{AppError, ClientMessage, DemoApp, WsSender};
use crate::bridge::SessionBridge;
use crate::demo_meta;

use super::resolve_voice;

/// Native audio voice chat with Gemini Live.
pub struct VoiceChat;

#[async_trait]
impl DemoApp for VoiceChat {
    demo_meta! {
        name: "voice-chat",
        description: "Native audio voice chat with Gemini Live",
        category: Basic,
        features: ["voice", "transcription"],
        tips: [
            "Click the microphone button to start speaking",
            "Transcriptions appear below each message showing what was said",
            "You can also type text — the model will respond with voice",
        ],
        try_saying: [
            "Hello! Tell me a joke.",
            "What's the weather like on Mars?",
            "Can you sing a short song?",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        info!("VoiceChat session starting");
        SessionBridge::new(tx)
            .run(self, &mut rx, |live, start| {
                let voice = resolve_voice(start.voice.as_deref());
                live.model(GeminiModel::Gemini2_0FlashLive)
                    .voice(voice)
                    .instruction(
                        start
                            .system_instruction
                            .as_deref()
                            .unwrap_or("You are a helpful voice assistant. Keep your responses concise and conversational."),
                    )
                    .transcription(true, true)
            })
            .await
    }
}
