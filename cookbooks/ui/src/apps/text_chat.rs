use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::info;

use adk_rs_fluent::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, WsSender};

use super::{build_session_config, wait_for_start};

/// Minimal text-only Gemini Live session.
pub struct TextChat;

#[async_trait]
impl CookbookApp for TextChat {
    fn name(&self) -> &str {
        "text-chat"
    }

    fn description(&self) -> &str {
        "Minimal text-only Gemini Live session"
    }

    fn category(&self) -> AppCategory {
        AppCategory::Basic
    }

    fn features(&self) -> Vec<String> {
        vec!["text".into()]
    }

    fn tips(&self) -> Vec<String> {
        vec![
            "Text-only mode — no microphone needed".into(),
            "Watch the streaming text deltas arrive in real time".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "What are three interesting facts about octopuses?".into(),
            "Explain quantum computing in simple terms".into(),
            "Write a short poem about Rust programming".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;
        let bridge = crate::bridge::SessionBridge::new(tx);

        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .text_only()
            .system_instruction(
                start
                    .system_instruction
                    .as_deref()
                    .unwrap_or("You are a helpful assistant."),
            );

        let handle = bridge
            .wire_live(Live::builder())
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        bridge.send_connected();
        bridge.send_meta(self);
        info!("TextChat session connected");

        bridge.recv_loop(&handle, &mut rx).await;
        Ok(())
    }
}
