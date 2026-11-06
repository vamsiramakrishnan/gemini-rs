//! Route logic the demo web UI and `adk web` share.
//!
//! Each surface keeps its own axum handlers, because they extract their own
//! state type and serve their own page assets; these functions hold what is
//! the same underneath: the app list, the favicon, and the WebSocket upgrade
//! for a named app.

use axum::extract::ws::WebSocketUpgrade;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use tokio::sync::broadcast;

use crate::ws::{AppInfo, AppRegistry, ServerMessage, handle_ws};

/// The registered apps, as the landing page lists them (`GET /api/apps`).
pub fn list_apps(registry: &AppRegistry) -> Json<Vec<AppInfo>> {
    Json(registry.list())
}

/// An SVG favicon of one emoji (`GET /favicon.ico`).
pub fn favicon(emoji: &str) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/svg+xml")],
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><text y=".9em" font-size="90">{emoji}</text></svg>"#
        ),
    )
        .into_response()
}

/// Upgrade to a WebSocket and run app `name` on it, forwarding `span_rx`
/// (devtools span events) onto the socket when given. An unknown name logs
/// a warning and closes the socket straight after the upgrade.
pub fn upgrade_app_socket(
    ws: WebSocketUpgrade,
    registry: &AppRegistry,
    name: String,
    span_rx: Option<broadcast::Receiver<ServerMessage>>,
) -> Response {
    let app = registry.get(&name);
    ws.on_upgrade(move |mut socket| async move {
        match app {
            Some(app) => handle_ws(socket, app, span_rx).await,
            None => {
                use futures::SinkExt as _;
                tracing::warn!("Agent '{name}' not found");
                let _ = socket.close().await;
            }
        }
    })
}
