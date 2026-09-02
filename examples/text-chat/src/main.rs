//! Text Chat example — simple text-only chat with Gemini Live.
//!
//! The smallest possible Live integration on the L2 fluent crate: one
//! `Live::builder()` per browser tab, `.text_only()` for text responses,
//! `.connect_from_env()` for auth, and three callbacks that forward the
//! model's output to the browser over a WebSocket.
//!
//! Usage:
//!   cargo run -p example-text-chat
//!   # then open http://127.0.0.1:3001

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use futures::{sink::SinkExt, stream::StreamExt};
use gemini_adk_fluent_rs::prelude::*;
use serde::{Deserialize, Serialize};
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
    TextDelta { text: String },
    TextComplete { text: String },
    TurnComplete,
    Interrupted,
    Error { message: String },
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

    let addr = "127.0.0.1:3001";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Text Chat example running at http://{addr}");

    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

/// Open a text-only Live session whose output is forwarded to `tx`.
async fn start_session(
    tx: &WsSender,
    system_instruction: Option<String>,
) -> Result<LiveHandle, AgentError> {
    let mut live = Live::builder()
        // Google AI's catalog serves native-audio Live models; `.text_only()`
        // asks that model for text responses instead of speech.
        .text_only();
    if let Some(instruction) = system_instruction {
        live = live.instruction(instruction);
    }

    let (tx_delta, tx_complete, tx_turn, tx_interrupted, tx_error) =
        (tx.clone(), tx.clone(), tx.clone(), tx.clone(), tx.clone());
    live.on_text(move |t| {
        let _ = tx_delta.send(ServerMessage::TextDelta { text: t.into() });
    })
    .on_text_complete(move |t| {
        let _ = tx_complete.send(ServerMessage::TextComplete { text: t.into() });
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
    // No `.model(..)`: connect picks the platform's current Live model
    // (override with GEMINI_LIVE_MODEL).
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
            ClientMessage::Start { system_instruction } => {
                info!("Starting text-only session");
                if let Some(old) = session.take() {
                    let _ = old.disconnect().await;
                }
                match start_session(&ws_tx, system_instruction).await {
                    Ok(handle) => {
                        info!("Session active");
                        let _ = ws_tx.send(ServerMessage::Connected);
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
