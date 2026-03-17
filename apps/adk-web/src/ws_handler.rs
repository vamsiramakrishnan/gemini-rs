use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};

use crate::app::{ClientMessage, CookbookApp, ServerMessage};

/// Handle a WebSocket connection for a specific app.
pub async fn handle_ws(
    socket: WebSocket,
    app: Arc<dyn CookbookApp>,
    mut span_rx: broadcast::Receiver<ServerMessage>,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel::<ServerMessage>();
    let (client_tx, client_rx) = mpsc::unbounded_channel::<ClientMessage>();

    // Forward server messages + span events to WebSocket
    let span_server_tx = server_tx.clone();
    let span_task = tokio::spawn(async move {
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
    });

    // Forward server messages to WebSocket.
    // Audio messages are intercepted and sent as Binary frames (raw PCM bytes)
    // to eliminate JSON + base64 overhead on the browser hot path.
    // All other messages are serialized to JSON and sent as Text frames.
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

    // Forward WebSocket messages to app
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
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Run the app session
    let _ = app.handle_session(server_tx, client_rx).await;

    // Clean up
    span_task.abort();
    send_task.abort();
    recv_task.abort();
}
