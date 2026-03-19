//! REST API endpoints — delegates to `gemini-adk-server` for all shared endpoints.
//!
//! The web UI mounts the shared API router under `/api/` and adds any
//! web-UI-specific endpoints here.

use axum::Router;

/// Build the API router — wraps the shared `gemini-adk-server` router under `/api`.
///
/// Returns a `Router<()>` since the server-core router manages its own state.
pub fn api_router(server_state: gemini_adk_server::ServerState) -> Router {
    Router::new().nest("/api", gemini_adk_server::build_api_router(server_state))
}
