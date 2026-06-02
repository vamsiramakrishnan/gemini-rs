use std::sync::Arc;

use axum::extract::ws::WebSocket;
use tokio::sync::broadcast;

use crate::app::{DemoApp, ServerMessage};

/// Handle a WebSocket connection for a specific app.
///
/// Thin wrapper over the shared [`gemini_adk_server_rs::ws::handle_ws`] bridge,
/// forwarding the span-event broadcast onto the same socket.
pub async fn handle_ws(
    socket: WebSocket,
    app: Arc<dyn DemoApp>,
    span_rx: broadcast::Receiver<ServerMessage>,
) {
    gemini_adk_server_rs::ws::handle_ws(socket, app, Some(span_rx)).await;
}
