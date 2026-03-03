use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::{build_session_config, send_app_meta, wait_for_start};

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

        // Build session config for text-only mode.
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .text_only()
            .system_instruction(
                start
                    .system_instruction
                    .as_deref()
                    .unwrap_or("You are a helpful assistant."),
            );

        // Build Live session with callbacks.
        let tx_text = tx.clone();
        let tx_text_complete = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupted = tx.clone();
        let tx_error = tx.clone();
        let tx_disconnected = tx.clone();

        let handle = Live::builder()
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
            .on_error(move |msg| {
                let tx = tx_error.clone();
                async move {
                    let _ = tx.send(ServerMessage::Error { message: msg });
                }
            })
            .on_disconnected(move |_reason| {
                let _tx = tx_disconnected.clone();
                async move {
                    info!("TextChat session disconnected by server");
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("TextChat session connected");

        // Browser -> Gemini loop.
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = tx.send(ServerMessage::Error {
                            message: e.to_string(),
                        });
                    }
                }
                ClientMessage::Stop => {
                    info!("TextChat session stopping");
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {} // Ignore audio messages in text mode
            }
        }

        Ok(())
    }
}
