//! REST API endpoints matching upstream ADK API server.
//!
//! Provides endpoints for:
//! - Agent execution (`/api/run`, `/api/run_sse`)
//! - Session management (`/api/sessions/*`)
//! - Artifact management (`/api/artifacts/*`)
//! - Debug/trace endpoints (`/api/debug/*`)
//! - Agent config discovery (`/api/agents`)

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
use std::collections::HashMap;

use crate::AppState;

/// Build the API router with all REST endpoints.
pub fn api_router() -> Router<AppState> {
    Router::new()
        // Agent execution
        .route("/api/run", post(run_agent))
        .route("/api/run_sse", post(run_agent_sse))
        // Agent discovery
        .route("/api/agents", get(list_agents))
        .route("/api/agents/:name", get(get_agent))
        // Session management
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/:id", get(get_session))
        .route("/api/sessions/:id", delete(delete_session))
        .route("/api/sessions/:id/events", get(get_session_events))
        .route("/api/sessions/:id/state", get(get_session_state))
        .route("/api/sessions/:id/rewind", post(rewind_session))
        // Artifact management
        .route("/api/artifacts/:session_id", get(list_artifacts))
        .route(
            "/api/artifacts/:session_id/:name",
            get(get_artifact),
        )
        .route(
            "/api/artifacts/:session_id/:name/:version",
            get(get_artifact_version),
        )
        // Debug/trace
        .route("/api/debug/traces", get(list_traces))
        .route("/api/debug/traces/:trace_id", get(get_trace))
        .route("/api/debug/health", get(health_check))
        // Eval
        .route("/api/eval/run", post(run_eval))
        .route("/api/eval/results", get(list_eval_results))
}

// ── Request/Response types ──────────────────────────────────────

/// Request body for `/api/run`.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    /// Agent name to execute.
    pub agent: String,
    /// User message / input.
    pub message: String,
    /// Session ID (optional — creates new if absent).
    #[serde(default)]
    pub session_id: Option<String>,
    /// User ID.
    #[serde(default = "default_user_id")]
    pub user_id: String,
    /// Run configuration overrides.
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

fn default_user_id() -> String {
    "default_user".to_string()
}

/// Response from `/api/run`.
#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub session_id: String,
    pub response: String,
    pub events: Vec<serde_json::Value>,
    pub state: HashMap<String, serde_json::Value>,
}

/// Agent info for discovery.
#[derive(Debug, Serialize)]
pub struct AgentInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub tools: Vec<String>,
    pub sub_agents: Vec<String>,
    pub agent_type: String,
}

/// Session summary for listing.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub app_name: String,
    pub user_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub event_count: usize,
}

/// Query parameters for session listing.
#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    #[serde(default)]
    pub app_name: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize {
    50
}

/// Request body for session rewind.
#[derive(Debug, Deserialize)]
pub struct RewindRequest {
    pub invocation_id: String,
}

/// Artifact summary.
#[derive(Debug, Serialize)]
pub struct ArtifactSummary {
    pub name: String,
    pub versions: usize,
    pub latest_mime_type: String,
    pub latest_size: usize,
}

/// Eval run request.
#[derive(Debug, Deserialize)]
pub struct EvalRunRequest {
    pub agent: String,
    #[serde(default)]
    pub eval_set: Option<String>,
    #[serde(default)]
    pub eval_set_inline: Option<serde_json::Value>,
    #[serde(default)]
    pub criteria: Vec<String>,
}

/// Eval result.
#[derive(Debug, Serialize)]
pub struct EvalResultSummary {
    pub agent: String,
    pub timestamp: String,
    pub total_cases: usize,
    pub passed: usize,
    pub failed: usize,
    pub pass_rate: f64,
    pub criteria_scores: HashMap<String, f64>,
}

/// Trace summary.
#[derive(Debug, Serialize)]
pub struct TraceSummary {
    pub trace_id: String,
    pub root_span: String,
    pub duration_ms: f64,
    pub span_count: usize,
    pub timestamp: String,
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_secs: u64,
}

// ── Handlers ────────────────────────────────────────────────────

async fn run_agent(
    State(_state): State<AppState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    // Stub: In a real deployment, this would look up the agent from the registry,
    // create/restore a session, run the agent, and return the result.
    let response = RunResponse {
        session_id: req.session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        response: format!("Agent '{}' received: {}", req.agent, req.message),
        events: vec![],
        state: HashMap::new(),
    };
    Json(response)
}

async fn run_agent_sse(
    State(_state): State<AppState>,
    Json(req): Json<RunRequest>,
) -> Sse<futures::stream::Once<futures::future::Ready<Result<Event, axum::Error>>>> {
    // Stub: SSE streaming endpoint for agent execution.
    let event = Event::default()
        .event("message")
        .data(serde_json::json!({
            "type": "response",
            "agent": req.agent,
            "text": format!("Streaming response for: {}", req.message),
        }).to_string());

    Sse::new(futures::stream::once(futures::future::ready(Ok(event))))
}

async fn list_agents(State(state): State<AppState>) -> Json<Vec<AgentInfo>> {
    let apps = state.registry.list();
    let agents: Vec<AgentInfo> = apps
        .into_iter()
        .map(|info| AgentInfo {
            name: info.name.clone(),
            description: Some(info.description.clone()),
            model: None,
            tools: info.features.clone(),
            sub_agents: vec![],
            agent_type: "llm".to_string(),
        })
        .collect();
    Json(agents)
}

async fn get_agent(
    Path(name): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(app) = state.registry.get(&name) {
        let info = AgentInfo {
            name: app.name().to_string(),
            description: Some(app.description().to_string()),
            model: None,
            tools: app.features(),
            sub_agents: vec![],
            agent_type: "llm".to_string(),
        };
        Json(info).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn list_sessions(
    Query(_query): Query<SessionQuery>,
) -> Json<Vec<SessionSummary>> {
    // Stub: Would query the session service.
    Json(vec![])
}

async fn create_session(
    Json(_body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session_id = uuid::Uuid::new_v4().to_string();
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": session_id,
            "created": true,
        })),
    )
}

async fn get_session(Path(id): Path<String>) -> impl IntoResponse {
    // Stub: Would fetch session from session service.
    Json(serde_json::json!({
        "id": id,
        "state": {},
        "events": [],
    }))
}

async fn delete_session(Path(id): Path<String>) -> impl IntoResponse {
    Json(serde_json::json!({
        "id": id,
        "deleted": true,
    }))
}

async fn get_session_events(Path(id): Path<String>) -> Json<Vec<serde_json::Value>> {
    let _ = id;
    Json(vec![])
}

async fn get_session_state(
    Path(id): Path<String>,
) -> Json<HashMap<String, serde_json::Value>> {
    let _ = id;
    Json(HashMap::new())
}

async fn rewind_session(
    Path(id): Path<String>,
    Json(req): Json<RewindRequest>,
) -> impl IntoResponse {
    Json(serde_json::json!({
        "id": id,
        "invocation_id": req.invocation_id,
        "rewound": true,
        "events_removed": 0,
    }))
}

async fn list_artifacts(Path(session_id): Path<String>) -> Json<Vec<ArtifactSummary>> {
    let _ = session_id;
    Json(vec![])
}

async fn get_artifact(
    Path((session_id, name)): Path<(String, String)>,
) -> impl IntoResponse {
    Json(serde_json::json!({
        "session_id": session_id,
        "name": name,
        "versions": [],
    }))
}

async fn get_artifact_version(
    Path((session_id, name, version)): Path<(String, String, String)>,
) -> impl IntoResponse {
    Json(serde_json::json!({
        "session_id": session_id,
        "name": name,
        "version": version,
        "content": null,
    }))
}

async fn list_traces() -> Json<Vec<TraceSummary>> {
    Json(vec![])
}

async fn get_trace(Path(trace_id): Path<String>) -> impl IntoResponse {
    Json(serde_json::json!({
        "trace_id": trace_id,
        "spans": [],
    }))
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: 0, // Would track actual uptime
    })
}

async fn run_eval(
    Json(req): Json<EvalRunRequest>,
) -> impl IntoResponse {
    Json(serde_json::json!({
        "agent": req.agent,
        "status": "submitted",
        "criteria": req.criteria,
    }))
}

async fn list_eval_results() -> Json<Vec<EvalResultSummary>> {
    Json(vec![])
}
