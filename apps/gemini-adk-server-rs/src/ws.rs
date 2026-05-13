//! Shared WebSocket session bridge.
//!
//! This is the single WebSocket implementation reused by `gemini-adk-web-rs`,
//! `gemini-adk-api-rs`, and the `adk web` CLI. The transport plumbing
//! ([`handle_ws`]) is generic over an [`AgentSource`]: any source of agents
//! (a demo registry, a manifest directory, programmatic registration) plugs in
//! by implementing the trait, and the browser-facing wire protocol
//! ([`ServerMessage`] / [`ClientMessage`]) is shared across all of them.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

/// Display/grouping category for an agent in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppCategory {
    /// Foundational, single-feature examples.
    Basic,
    /// Multi-feature compositions.
    Advanced,
    /// Full end-to-end showcases.
    Showcase,
}

impl std::fmt::Display for AppCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppCategory::Basic => write!(f, "Basic"),
            AppCategory::Advanced => write!(f, "Advanced"),
            AppCategory::Showcase => write!(f, "Showcase"),
        }
    }
}

/// Metadata about an agent (sent to the frontend for rendering cards).
#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    /// Unique agent name (also the `/ws/{name}` route key).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// UI grouping category.
    pub category: AppCategory,
    /// Capability tags shown in the UI.
    pub features: Vec<String>,
    /// Usage tips shown in the UI.
    pub tips: Vec<String>,
    /// Example prompts shown in the UI.
    pub try_saying: Vec<String>,
}

/// Messages sent from an agent session to the browser.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    /// Session connected and ready.
    Connected,
    /// Incremental text delta from the model.
    TextDelta {
        /// The text fragment.
        text: String,
    },
    /// Full accumulated text for the current generation.
    TextComplete {
        /// The complete text.
        text: String,
    },
    /// Raw PCM audio bytes. Sent as binary WebSocket frames — never JSON-serialized.
    Audio {
        /// PCM payload.
        #[serde(skip)]
        data: Vec<u8>,
    },
    /// Turn boundary reached.
    TurnComplete,
    /// Model output was interrupted.
    Interrupted,
    /// Non-fatal error.
    Error {
        /// Error message.
        message: String,
    },
    /// ASR transcript of the user's speech.
    InputTranscription {
        /// Transcript text.
        text: String,
    },
    /// Transcript of the model's audio output.
    OutputTranscription {
        /// Transcript text.
        text: String,
    },
    /// Model thought/reasoning summary (when includeThoughts is enabled).
    Thought {
        /// Thought text.
        text: String,
    },
    /// Voice activity detected — user started speaking.
    VoiceActivityStart,
    /// Voice activity ended — user stopped speaking.
    VoiceActivityEnd,
    /// Devtools: a state key changed.
    StateUpdate {
        /// State key.
        key: String,
        /// New value.
        value: serde_json::Value,
    },
    /// Devtools: phase machine transitioned.
    PhaseChange {
        /// Previous phase.
        from: String,
        /// New phase.
        to: String,
        /// Transition reason.
        reason: String,
    },
    /// Devtools: an evaluation result.
    #[allow(dead_code)]
    Evaluation {
        /// Phase evaluated.
        phase: String,
        /// Score.
        score: f64,
        /// Notes.
        notes: String,
    },
    /// Devtools: a guard/rule violation.
    Violation {
        /// Rule name.
        rule: String,
        /// Severity.
        severity: String,
        /// Detail.
        detail: String,
    },
    /// Agent metadata, sent once on connect.
    AppMeta {
        /// The agent's [`AppInfo`].
        info: AppInfo,
    },
    /// Declarative runtime contract for DevTools and replay validation.
    RuntimeContract {
        /// The serialized contract payload.
        contract: serde_json::Value,
    },
    /// Live session telemetry stats (turn count, phase durations, tool calls, etc.)
    Telemetry {
        /// Arbitrary stats payload.
        stats: serde_json::Value,
    },
    /// Real-time tool call event for devtools visualization.
    ToolCallEvent {
        /// Tool name.
        name: String,
        /// Serialized arguments.
        args: String,
        /// Serialized result.
        result: String,
    },
    /// State promotion decision from an extraction field.
    StatePromotionEvent {
        /// Extractor name.
        extractor: String,
        /// Source field.
        field: String,
        /// Target state key.
        state_key: String,
        /// Whether the promotion was accepted.
        accepted: bool,
        /// Reason.
        reason: String,
        /// Value.
        value: serde_json::Value,
    },
    /// OTel span lifecycle event bridged from a tracing span layer.
    SpanEvent {
        /// Span name.
        name: String,
        /// Span id.
        span_id: String,
        /// Parent span id, if any.
        parent_id: Option<String>,
        /// Duration in microseconds.
        duration_us: u64,
        /// Attributes payload.
        attributes: serde_json::Value,
        /// Status.
        status: String,
    },
    /// Per-turn metrics for sparkline visualization.
    TurnMetrics {
        /// Turn number.
        turn: u32,
        /// Latency in ms.
        latency_ms: u32,
        /// Prompt tokens.
        prompt_tokens: u32,
        /// Response tokens.
        response_tokens: u32,
    },
    /// Voice reactor state snapshot for devtools.
    VoiceRuntimeState {
        /// Whether the user is speaking.
        user_speaking: bool,
        /// Whether playback is active.
        playback_active: bool,
        /// Whether a prompt is pending.
        prompt_pending: bool,
        /// Prompt epoch counter.
        prompt_epoch: u64,
        /// Milliseconds since the last barge-in.
        last_barge_in_ms_ago: Option<u64>,
        /// Milliseconds since playback last drained.
        last_playback_drained_ms_ago: Option<u64>,
        /// VAD backend name.
        vad_backend: String,
        /// VAD state.
        vad_state: String,
        /// Whether VAD reports speech.
        vad_speaking: bool,
        /// VAD speech probability.
        vad_probability: Option<f32>,
        /// VAD frame duration in ms.
        vad_frame_duration_ms: u32,
        /// Number of VAD frames processed.
        vad_frames_processed: u64,
        /// Milliseconds since the last VAD transition.
        vad_last_transition_ms_ago: Option<u64>,
    },
}

/// Messages received from the browser.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMessage {
    /// Begin a session, with optional overrides.
    Start {
        /// System instruction override.
        #[serde(default)]
        system_instruction: Option<String>,
        /// Model override.
        #[serde(default)]
        model: Option<String>,
        /// Voice override.
        #[serde(default)]
        voice: Option<String>,
    },
    /// A text turn from the user.
    Text {
        /// The text.
        text: String,
    },
    /// Base64-encoded PCM audio from the user.
    Audio {
        /// Base64 payload.
        data: String,
    },
    /// The browser finished playing buffered audio.
    PlaybackDrained,
    /// The browser detected the user started speaking.
    UserSpeechStarted,
    /// The browser detected the user stopped speaking.
    UserSpeechEnded,
    /// End the session.
    Stop,
}

/// Sender handle for delivering [`ServerMessage`]s to the browser.
pub type WsSender = mpsc::UnboundedSender<ServerMessage>;

/// A source of agents the WebSocket bridge can run.
///
/// Implementors own the session loop: given a [`WsSender`] and a receiver of
/// [`ClientMessage`]s, [`handle_session`](AgentSource::handle_session) drives a
/// live (or text) agent for the duration of the connection. The transport
/// framing is provided by [`handle_ws`]. This is the single seam that the web
/// app's demo registry and the CLI's manifest directory both plug into.
#[async_trait]
pub trait AgentSource: Send + Sync {
    /// Unique agent name (the `/ws/{name}` route key).
    fn name(&self) -> &str;
    /// Human-readable description.
    fn description(&self) -> &str;
    /// UI grouping category.
    fn category(&self) -> AppCategory;
    /// Capability tags.
    fn features(&self) -> Vec<String>;
    /// Usage tips.
    fn tips(&self) -> Vec<String> {
        Vec::new()
    }
    /// Example prompts.
    fn try_saying(&self) -> Vec<String> {
        Vec::new()
    }

    /// Handle a full WebSocket session. Called when a client connects to
    /// `/ws/{name}`. The source receives client messages via `rx` and sends
    /// server messages via `tx`.
    async fn handle_session(
        &self,
        tx: WsSender,
        rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError>;
}

/// Error returned by an [`AgentSource`] session.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Failed to establish the upstream connection.
    #[error("Connection error: {0}")]
    Connection(String),
    /// Error during the session.
    #[error("Session error: {0}")]
    Session(String),
    /// Other error.
    #[error("{0}")]
    #[allow(dead_code)]
    Other(String),
}

/// Registry of available agents keyed by name, preserving insertion order.
#[derive(Default)]
pub struct AppRegistry {
    apps: HashMap<String, Arc<dyn AgentSource>>,
    order: Vec<String>,
}

impl AppRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent source.
    pub fn register(&mut self, app: impl AgentSource + 'static) {
        let name = app.name().to_string();
        self.order.push(name.clone());
        self.apps.insert(name, Arc::new(app));
    }

    /// Register an already-`Arc`'d agent source.
    pub fn register_arc(&mut self, app: Arc<dyn AgentSource>) {
        let name = app.name().to_string();
        self.order.push(name.clone());
        self.apps.insert(name, app);
    }

    /// Look up an agent source by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentSource>> {
        self.apps.get(name).cloned()
    }

    /// List metadata for all registered agents in registration order.
    pub fn list(&self) -> Vec<AppInfo> {
        self.order
            .iter()
            .filter_map(|name| self.apps.get(name))
            .map(|app| AppInfo {
                name: app.name().to_string(),
                description: app.description().to_string(),
                category: app.category(),
                features: app.features(),
                tips: app.tips(),
                try_saying: app.try_saying(),
            })
            .collect()
    }
}

/// Drive a WebSocket connection against an [`AgentSource`].
///
/// Splits the socket, forwards [`ServerMessage`]s to the browser (audio as
/// binary frames, everything else as JSON text), forwards inbound
/// [`ClientMessage`]s to the source, and runs the source's session to
/// completion. An optional `span_rx` broadcasts out-of-band server messages
/// (e.g. tracing span events) onto the same socket.
pub async fn handle_ws(
    socket: WebSocket,
    app: Arc<dyn AgentSource>,
    span_rx: Option<broadcast::Receiver<ServerMessage>>,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel::<ServerMessage>();
    let (client_tx, client_rx) = mpsc::unbounded_channel::<ClientMessage>();

    // Optionally forward broadcast (span/devtools) events onto the socket.
    let span_task = span_rx.map(|mut span_rx| {
        let span_server_tx = server_tx.clone();
        tokio::spawn(async move {
            loop {
                match span_rx.recv().await {
                    Ok(msg) => {
                        if span_server_tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    });

    // Forward server messages to the WebSocket. Audio goes out as binary frames
    // (raw PCM) to avoid JSON+base64 overhead; everything else is JSON text.
    let send_task = tokio::spawn(async move {
        while let Some(msg) = server_rx.recv().await {
            match msg {
                ServerMessage::Audio { data } => {
                    if ws_tx.send(Message::Binary(data)).await.is_err() {
                        break;
                    }
                }
                other => {
                    if let Ok(json) = serde_json::to_string(&other) {
                        if ws_tx.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    // Forward inbound WebSocket messages to the source.
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                        if client_tx.send(client_msg).is_err() {
                            break;
                        }
                    }
                }
                Message::Binary(data) => {
                    // Raw audio binary frame from the browser.
                    if client_tx
                        .send(ClientMessage::Audio {
                            data: base64_encode(&data),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Run the session.
    let _ = app.handle_session(server_tx, client_rx).await;

    // Clean up.
    if let Some(span_task) = span_task {
        span_task.abort();
    }
    send_task.abort();
    recv_task.abort();
}

/// Minimal standard base64 encoder (avoids pulling a dependency just for the
/// rare binary-audio path; the common path is base64 already done by browsers).
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
