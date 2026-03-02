use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{info, warn};

use rs_genai::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

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

        // Connect to Gemini Live.
        let handle = ConnectBuilder::new(config)
            .build()
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        handle.wait_for_phase(SessionPhase::Active).await;
        let _ = tx.send(ServerMessage::Connected);
        info!("TextChat session connected");

        // Subscribe to server events.
        let mut events = handle.subscribe();

        loop {
            tokio::select! {
                // Client -> Gemini
                client_msg = rx.recv() => {
                    match client_msg {
                        Some(ClientMessage::Text { text }) => {
                            if let Err(e) = handle.send_text(&text).await {
                                warn!("Failed to send text: {e}");
                                let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                            }
                        }
                        Some(ClientMessage::Stop) | None => {
                            info!("TextChat session stopping");
                            let _ = handle.disconnect().await;
                            break;
                        }
                        _ => {} // Ignore audio messages in text mode
                    }
                }

                // Gemini -> Client
                event = recv_event(&mut events) => {
                    match event {
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
                        Some(SessionEvent::Error(msg)) => {
                            let _ = tx.send(ServerMessage::Error { message: msg });
                        }
                        Some(SessionEvent::Disconnected(_)) => {
                            info!("TextChat session disconnected by server");
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
