//! REST API endpoints — delegates to `adk-server-core` for all shared endpoints.
//!
//! The web UI mounts the shared API router under `/api/` and adds any
//! web-UI-specific endpoints here.

use axum::Router;

/// Build the API router — wraps the shared `adk-server-core` router under `/api`.
///
/// Returns a `Router<()>` since the server-core router manages its own state.
pub fn api_router(server_state: adk_server_core::ServerState) -> Router {
    Router::new().nest("/api", adk_server_core::build_api_router(server_state))
}
