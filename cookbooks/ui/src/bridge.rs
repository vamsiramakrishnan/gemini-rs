//! SessionBridge — eliminates callback boilerplate in cookbook apps.
//!
//! Wires all standard event callbacks (audio, text, turn, interrupt, VAD, error,
//! transcription, telemetry) onto a Live builder in one call.

use base64::Engine;
use tokio::sync::mpsc;
use tracing::warn;

use adk_rs_fluent::prelude::*;

use crate::app::{AppInfo, CookbookApp, ServerMessage, WsSender};

/// Bridge between a cookbook app's WebSocket sender and a Live session builder.
///
/// Call `bridge.wire_live(builder)` to attach all standard callbacks,
/// then `bridge.recv_loop(handle, rx)` to run the browser->Gemini forwarding loop.
pub struct SessionBridge {
    tx: WsSender,
}

impl SessionBridge {
    pub fn new(tx: WsSender) -> Self {
        Self { tx }
    }

    /// Send the Connected message to the browser.
    pub fn send_connected(&self) {
        let _ = self.tx.send(ServerMessage::Connected);
    }

    /// Send appMeta message so devtools can configure tabs.
    pub fn send_meta(&self, app: &dyn CookbookApp) {
        let _ = self.tx.send(ServerMessage::AppMeta {
            info: AppInfo {
                name: app.name().to_string(),
                description: app.description().to_string(),
                category: app.category(),
                features: app.features(),
                tips: app.tips(),
                try_saying: app.try_saying(),
            },
        });
    }

    /// Wire all standard event callbacks onto a Live builder.
    ///
    /// Attaches: on_audio, on_text, on_text_complete, on_turn_complete,
    /// on_interrupted, on_vad_start, on_vad_end, on_error, on_disconnected,
    /// on_input_transcript, on_output_transcript.
    ///
    /// The builder is returned with all callbacks attached — the app can
    /// add additional callbacks (e.g., on_tool_call) before calling `.connect()`.
    pub fn wire_live(&self, builder: Live) -> Live {
        let b64 = base64::engine::general_purpose::STANDARD;

        let tx_audio = self.tx.clone();
        let tx_text = self.tx.clone();
        let tx_text_complete = self.tx.clone();
        let tx_turn = self.tx.clone();
        let tx_interrupted = self.tx.clone();
        let tx_vad_start = self.tx.clone();
        let tx_vad_end = self.tx.clone();
        let tx_error = self.tx.clone();
        let tx_disconnected = self.tx.clone();
        let tx_input = self.tx.clone();
        let tx_output = self.tx.clone();

        builder
            .on_audio(move |data| {
                let encoded = b64.encode(data);
                let _ = tx_audio.send(ServerMessage::Audio { data: encoded });
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
                async move {}
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
    }

    /// Spawn a periodic telemetry sender that emits `Telemetry` and `TurnMetrics`
    /// messages to the browser every 2 seconds.
    ///
    /// Returns the `JoinHandle` so the caller can abort it on disconnect.
    pub fn spawn_telemetry(&self, handle: &LiveHandle) -> tokio::task::JoinHandle<()> {
        let telem = handle.telemetry().clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
            let mut prev_turn_count = 0u64;
            loop {
                interval.tick().await;
                let stats = telem.snapshot();

                // Emit per-turn metrics when the turn count advances
                if let Some(obj) = stats.as_object() {
                    let turn_count = obj.get("turn_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    if turn_count > prev_turn_count {
                        let latency_ms = obj
                            .get("last_latency_ms")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        let prompt_tokens = obj
                            .get("prompt_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        let response_tokens = obj
                            .get("response_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        let _ = tx.send(ServerMessage::TurnMetrics {
                            turn: turn_count as u32,
                            latency_ms,
                            prompt_tokens,
                            response_tokens,
                        });
                        prev_turn_count = turn_count;
                    }
                }

                if tx.send(ServerMessage::Telemetry { stats }).is_err() {
                    break;
                }
            }
        })
    }

    /// Run the browser->Gemini forwarding loop.
    ///
    /// Handles Audio, Text, and Stop messages from the browser.
    /// Also spawns a periodic telemetry sender that emits `Telemetry`
    /// and `TurnMetrics` messages automatically.
    /// Returns when the client sends Stop or disconnects.
    pub async fn recv_loop(
        &self,
        handle: &LiveHandle,
        rx: &mut mpsc::UnboundedReceiver<crate::app::ClientMessage>,
    ) {
        use crate::app::ClientMessage;

        let telem_task = self.spawn_telemetry(handle);

        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Audio { data } => {
                    if let Ok(pcm_bytes) = b64.decode(&data) {
                        if let Err(e) = handle.send_audio(pcm_bytes).await {
                            warn!("Failed to send audio: {e}");
                            let _ = self.tx.send(ServerMessage::Error {
                                message: e.to_string(),
                            });
                        }
                    }
                }
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = self.tx.send(ServerMessage::Error {
                            message: e.to_string(),
                        });
                    }
                }
                ClientMessage::Stop => {
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }

        telem_task.abort();
    }

    /// Get a clone of the sender for custom callbacks.
    pub fn sender(&self) -> WsSender {
        self.tx.clone()
    }
}
