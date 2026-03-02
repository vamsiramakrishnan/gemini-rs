use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::app::{ClientMessage, CookbookApp, ServerMessage};

/// Handle a WebSocket connection for a specific app.
pub async fn handle_ws(socket: WebSocket, app: Arc<dyn CookbookApp>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel::<ServerMessage>();
    let (client_tx, client_rx) = mpsc::unbounded_channel::<ClientMessage>();

    // Forward server messages to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = server_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if ws_tx.send(Message::Text(json.into())).await.is_err() {
                    break;
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
    send_task.abort();
    recv_task.abort();
}
