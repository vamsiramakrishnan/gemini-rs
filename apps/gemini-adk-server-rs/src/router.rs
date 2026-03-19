//! REST API router builder — single router shared by all server surfaces.

use axum::{
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;

use crate::{handlers, ServerState};

/// Build the complete REST API router with all upstream ADK endpoints.
///
/// Mount this directly or merge into an existing Axum router:
///
/// ```ignore
/// // Standalone
/// let app = build_api_router(state);
///
/// // Merged with web UI
/// let app = Router::new()
///     .route("/", get(landing))
///     .merge(build_api_router(state));
/// ```
pub fn build_api_router(state: ServerState) -> Router {
    Router::new()
        // Agent execution
        .route("/run", post(handlers::run_agent))
        .route("/run_sse", post(handlers::run_agent_sse))
        // Agent discovery
        .route("/list-apps", get(handlers::list_agents))
        .route("/apps/:name", get(handlers::get_agent))
        // Session management
        .route(
            "/apps/:app/users/:user/sessions",
            get(handlers::list_sessions).post(handlers::create_session),
        )
        .route(
            "/apps/:app/users/:user/sessions/:id",
            get(handlers::get_session).delete(handlers::delete_session),
        )
        .route(
            "/apps/:app/users/:user/sessions/:id/events",
            get(handlers::get_session_events),
        )
        .route(
            "/apps/:app/users/:user/sessions/:id/state",
            get(handlers::get_session_state),
        )
        .route(
            "/apps/:app/users/:user/sessions/:id/rewind",
            post(handlers::rewind_session),
        )
        // Artifacts
        .route(
            "/apps/:app/users/:user/sessions/:session/artifacts",
            get(handlers::list_artifacts),
        )
        .route(
            "/apps/:app/users/:user/sessions/:session/artifacts/:name",
            get(handlers::get_artifact),
        )
        .route(
            "/apps/:app/users/:user/sessions/:session/artifacts/:name/:version",
            get(handlers::get_artifact_version),
        )
        // Debug
        .route("/debug/trace/:trace_id", get(handlers::get_trace))
        .route("/debug/health", get(handlers::health_check))
        // Eval
        .route("/eval/run", post(handlers::run_eval))
        .route("/eval/results", get(handlers::list_eval_results))
        .layer(CorsLayer::permissive())
        .with_state(state)
}
