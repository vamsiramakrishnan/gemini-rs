//! adk-api-server — Standalone REST API server for ADK agents.
//!
//! Mirrors the upstream `adk api_server` command, exposing REST endpoints
//! for agent execution, session management, artifact access, eval, and
//! debug/trace inspection.
//!
//! # Usage
//!
//! ```bash
//! # From workspace root
//! cargo run -p adk-api-server
//!
//! # With custom port
//! ADK_API_PORT=8080 cargo run -p adk-api-server
//!
//! # Via the CLI
//! adk api_server --port 8080
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Json,
    },
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

mod handlers;

// ── Shared State ────────────────────────────────────────────────

/// Shared application state for the API server.
#[derive(Clone)]
pub struct ApiState {
    /// Registered agent configs (loaded from agent.json / agent.yaml).
    pub agents: Arc<Vec<AgentEntry>>,
    /// In-memory session store (swap for DB-backed in production).
    pub sessions: Arc<parking_lot::RwLock<HashMap<String, SessionData>>>,
    /// In-memory artifact store.
    pub artifacts: Arc<parking_lot::RwLock<HashMap<String, Vec<ArtifactEntry>>>>,
}

/// A registered agent (config-driven or code-defined).
#[derive(Debug, Clone, Serialize)]
pub struct AgentEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub agent_type: String,
    pub tools: Vec<String>,
    pub sub_agents: Vec<String>,
}

/// In-memory session representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub id: String,
    pub app_name: String,
    pub user_id: String,
    pub state: HashMap<String, serde_json::Value>,
    pub events: Vec<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// A stored artifact version.
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactEntry {
    pub name: String,
    pub version: usize,
    pub mime_type: String,
    pub content: String,
    pub size: usize,
    pub timestamp: String,
}

// ── Main ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    init_tracing();

    let state = ApiState {
        agents: Arc::new(discover_agents()),
        sessions: Arc::new(parking_lot::RwLock::new(HashMap::new())),
        artifacts: Arc::new(parking_lot::RwLock::new(HashMap::new())),
    };

    let app = Router::new()
        // Agent execution
        .route("/run", post(handlers::run_agent))
        .route("/run_sse", post(handlers::run_agent_sse))
        // Agent discovery
        .route("/list-apps", get(handlers::list_agents))
        .route("/apps/:name", get(handlers::get_agent))
        // Session management
        .route("/apps/:app/users/:user/sessions", get(handlers::list_sessions))
        .route("/apps/:app/users/:user/sessions", post(handlers::create_session))
        .route("/apps/:app/users/:user/sessions/:id", get(handlers::get_session))
        .route("/apps/:app/users/:user/sessions/:id", delete(handlers::delete_session))
        .route("/apps/:app/users/:user/sessions/:id/events", get(handlers::get_session_events))
        .route("/apps/:app/users/:user/sessions/:id/state", get(handlers::get_session_state))
        .route("/apps/:app/users/:user/sessions/:id/rewind", post(handlers::rewind_session))
        // Artifacts
        .route("/apps/:app/users/:user/sessions/:session/artifacts", get(handlers::list_artifacts))
        .route("/apps/:app/users/:user/sessions/:session/artifacts/:name", get(handlers::get_artifact))
        .route("/apps/:app/users/:user/sessions/:session/artifacts/:name/:version", get(handlers::get_artifact_version))
        // Debug
        .route("/debug/trace/:trace_id", get(handlers::get_trace))
        .route("/debug/health", get(handlers::health_check))
        // Eval
        .route("/eval/run", post(handlers::run_eval))
        .route("/eval/results", get(handlers::list_eval_results))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let port: u16 = std::env::var("ADK_API_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8000);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("ADK API server listening on http://localhost:{port}");

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

/// Discover agent configs from the current directory tree.
fn discover_agents() -> Vec<AgentEntry> {
    let dir = std::env::current_dir().unwrap_or_default();
    match rs_adk::discover_agent_configs(&dir) {
        Ok(configs) => configs
            .into_iter()
            .map(|c| AgentEntry {
                name: c.name,
                description: c.description,
                model: c.model,
                agent_type: c.agent_type,
                tools: c.tools.iter().filter_map(|t| t.name.clone().or(t.builtin.clone())).collect(),
                sub_agents: c.sub_agents.iter().map(|s| s.name.clone()).collect(),
            })
            .collect(),
        Err(e) => {
            tracing::warn!("Agent config discovery failed: {e}");
            vec![]
        }
    }
}
