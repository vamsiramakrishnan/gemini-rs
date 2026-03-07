use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::info;

use adk_rs_fluent::prelude::*;

use crate::app::{AppError, ClientMessage, CookbookApp, WsSender};
use crate::bridge::SessionBridge;
use crate::cookbook_meta;

/// Minimal text-only Gemini Live session.
pub struct TextChat;

#[async_trait]
impl CookbookApp for TextChat {
    cookbook_meta! {
        name: "text-chat",
        description: "Minimal text-only Gemini Live session",
        category: Basic,
        features: ["text"],
        tips: [
            "Text-only mode — no microphone needed",
            "Watch the streaming text deltas arrive in real time",
        ],
        try_saying: [
            "What are three interesting facts about octopuses?",
            "Explain quantum computing in simple terms",
            "Write a short poem about Rust programming",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        info!("TextChat session starting");
        SessionBridge::new(tx)
            .run(self, &mut rx, |live, start| {
                live.model(GeminiModel::Gemini2_0FlashLive)
                    .text_only()
                    .instruction(
                        start
                            .system_instruction
                            .as_deref()
                            .unwrap_or("You are a helpful assistant."),
                    )
            })
            .await
    }
}
