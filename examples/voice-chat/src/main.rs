//! Voice Chat example — native audio chat with Gemini Live.
//!
//! Bidirectional audio on the L2 fluent crate: the browser streams PCM in,
//! `on_audio` streams the model's voice back out, and `.transcription()`
//! turns both sides of the conversation into text via the
//! `on_input_transcript` / `on_output_transcript` callbacks.
//!
//! Usage:
//!   cargo run -p example-voice-chat
//!   # then open http://127.0.0.1:3002

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use futures::{sink::SinkExt, stream::StreamExt};
use gemini_adk_fluent_rs::live::LiveEvent;
use gemini_adk_fluent_rs::prelude::*;
use serde::{Deserialize, Serialize, Serializer};
use tokio::sync::mpsc;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};
use tracing::{error, info};

/// Messages from the browser UI.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMessage {
    Start {
        voice: Option<String>,
        #[serde(alias = "systemInstruction")]
        system_instruction: Option<String>,
    },
    Text {
        text: String,
    },
    Audio {
        data: String,
    },
    Stop,
}

/// Messages to the browser UI.
#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ServerMessage {
    Connected,
    TextDelta {
        text: String,
    },
    TextComplete {
        text: String,
    },
    /// PCM16 24 kHz audio from the model, base64-encoded on the wire.
    Audio {
        #[serde(serialize_with = "as_base64")]
        data: Vec<u8>,
    },
    TurnComplete,
    Interrupted,
    Error {
        message: String,
    },
    InputTranscription {
        text: String,
    },
    OutputTranscription {
        text: String,
    },
    VoiceActivityStart,
    VoiceActivityEnd,
}

fn as_base64<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&BASE64.encode(bytes))
}

type WsSender = mpsc::UnboundedSender<ServerMessage>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Credentials come from the environment (or `.env`): GEMINI_API_KEY for
    // Google AI, or GOOGLE_GENAI_USE_VERTEXAI=true + GOOGLE_CLOUD_PROJECT for
    // Vertex AI. `connect_from_env()` reads them when a session starts.
    let _ = dotenvy::dotenv();

    let static_dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/gemini-adk-web-rs/static"
    );

    let app = Router::new()
        .fallback_service(
            ServeDir::new(static_dir).fallback(ServeFile::new(format!("{static_dir}/index.html"))),
        )
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive());

    let addr = "127.0.0.1:3002";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Voice Chat example running at http://{addr}");

    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

/// The voice the browser picked; unknown names fall back to Aoede.
fn parse_voice(name: Option<&str>) -> Voice {
    match name {
        Some("Puck") => Voice::Puck,
        Some("Charon") => Voice::Charon,
        Some("Kore") => Voice::Kore,
        Some("Fenrir") => Voice::Fenrir,
        _ => Voice::Aoede,
    }
}

/// Open a native-audio Live session whose events are forwarded to `tx`.
async fn start_session(
    tx: &WsSender,
    voice: Voice,
    system_instruction: Option<String>,
) -> Result<LiveHandle, AgentError> {
    let mut live = Live::builder().voice(voice).transcription();
    if let Some(instruction) = system_instruction {
        live = live.instruction(instruction);
    }

    // Fast-lane callbacks (`on_audio`, `on_text`, transcripts, VAD) run on
    // the event hot path, so they only push onto the channel; the WebSocket
    // task does the base64 encoding and the network I/O.
    let tx_audio = tx.clone();
    let tx_delta = tx.clone();
    let tx_complete = tx.clone();
    let tx_input = tx.clone();
    let tx_output = tx.clone();
    let tx_vad_start = tx.clone();
    let tx_vad_end = tx.clone();
    let tx_turn = tx.clone();
    let tx_interrupted = tx.clone();
    let tx_error = tx.clone();

    live.on_audio(move |pcm| {
        let _ = tx_audio.send(ServerMessage::Audio { data: pcm.to_vec() });
    })
    .on_text(move |t| {
        let _ = tx_delta.send(ServerMessage::TextDelta { text: t.into() });
    })
    .on_text_complete(move |t| {
        let _ = tx_complete.send(ServerMessage::TextComplete { text: t.into() });
    })
    .on_input_transcript(move |text, _is_final| {
        let _ = tx_input.send(ServerMessage::InputTranscription { text: text.into() });
    })
    .on_output_transcript(move |text, _is_final| {
        let _ = tx_output.send(ServerMessage::OutputTranscription { text: text.into() });
    })
    .on_vad_start(move || {
        let _ = tx_vad_start.send(ServerMessage::VoiceActivityStart);
    })
    .on_vad_end(move || {
        let _ = tx_vad_end.send(ServerMessage::VoiceActivityEnd);
    })
    .on_turn_complete(move || {
        let _ = tx_turn.send(ServerMessage::TurnComplete);
        async {}
    })
    .on_interrupted(move || {
        let _ = tx_interrupted.send(ServerMessage::Interrupted);
        async {}
    })
    .on_error(move |message| {
        error!("Session error: {message}");
        let _ = tx_error.send(ServerMessage::Error { message });
        async {}
    })
    // No `.model(..)`: connect picks the platform's current native-audio
    // Live model (override with GEMINI_LIVE_MODEL).
    .connect_from_env()
    .await
}

async fn handle_socket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Session callbacks push into `ws_tx`; this task writes to the browser.
    let send_task = tokio::spawn(async move {
        while let Some(msg) = ws_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg)
                && sender.send(Message::Text(json)).await.is_err()
            {
                break;
            }
        }
    });

    let mut session: Option<LiveHandle> = None;

    while let Some(Ok(Message::Text(text))) = receiver.next().await {
        let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) else {
            continue;
        };
        match client_msg {
            ClientMessage::Start {
                voice,
                system_instruction,
            } => {
                let voice = parse_voice(voice.as_deref());
                info!("Starting native audio session (voice: {voice})");
                if let Some(old) = session.take() {
                    let _ = old.disconnect().await;
                }
                match start_session(&ws_tx, voice, system_instruction).await {
                    Ok(handle) => {
                        info!("Session active");
                        let _ = ws_tx.send(ServerMessage::Connected);
                        // One line per turn: how long the model took to start
                        // answering (end of user speech → first audio byte).
                        let telemetry = handle.telemetry().clone();
                        let mut turns = handle.stream();
                        tokio::spawn(async move {
                            while let Some(event) = turns.next().await {
                                if matches!(event, LiveEvent::TurnComplete) {
                                    info!("response latency: {}", telemetry.latency());
                                }
                            }
                        });
                        session = Some(handle);
                    }
                    Err(e) => {
                        error!("Failed to connect: {e}");
                        let _ = ws_tx.send(ServerMessage::Error {
                            message: format!("Failed to connect: {e}"),
                        });
                    }
                }
            }
            ClientMessage::Text { text } => {
                if let Some(handle) = &session
                    && let Err(e) = handle.send_text(text).await
                {
                    error!("Failed to send text: {e}");
                }
            }
            ClientMessage::Audio { data } => {
                if let Some(handle) = &session
                    && let Ok(pcm) = BASE64.decode(data)
                    && let Err(e) = handle.send_audio(pcm).await
                {
                    error!("Failed to send audio: {e}");
                }
            }
            ClientMessage::Stop => {
                info!("Stopping session");
                if let Some(handle) = session.take() {
                    let _ = handle.disconnect().await;
                }
            }
        }
    }

    if let Some(handle) = session {
        let _ = handle.disconnect().await;
    }
    send_task.abort();
}
