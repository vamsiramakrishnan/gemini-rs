//! Request handlers for all API server endpoints.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Json,
    },
};
use serde::{Deserialize, Serialize};

use crate::{AgentEntry, ApiState, ArtifactEntry, SessionData};

// ── Request / Response types ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub agent: String,
    pub message: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default = "default_user_id")]
    pub user_id: String,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

fn default_user_id() -> String {
    "default_user".to_string()
}

#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub session_id: String,
    pub response: String,
    pub events: Vec<serde_json::Value>,
    pub state: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize {
    50
}

#[derive(Debug, Deserialize)]
pub struct RewindRequest {
    pub invocation_id: String,
}

#[derive(Debug, Serialize)]
pub struct ArtifactSummary {
    pub name: String,
    pub versions: usize,
    pub latest_mime_type: String,
    pub latest_size: usize,
}

#[derive(Debug, Deserialize)]
pub struct EvalRunRequest {
    pub agent: String,
    #[serde(default)]
    pub eval_set: Option<String>,
    #[serde(default)]
    pub criteria: Vec<String>,
}

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

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub agents_loaded: usize,
    pub sessions_active: usize,
}

// ── Agent Execution ─────────────────────────────────────────────

pub async fn run_agent(
    State(state): State<ApiState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    let session_id = req.session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Check agent exists
    if !state.agents.iter().any(|a| a.name == req.agent) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Agent '{}' not found", req.agent)})),
        )
            .into_response();
    }

    // Create session if needed
    let mut sessions = state.sessions.write();
    if !sessions.contains_key(&session_id) {
        sessions.insert(
            session_id.clone(),
            SessionData {
                id: session_id.clone(),
                app_name: req.agent.clone(),
                user_id: req.user_id.clone(),
                state: HashMap::new(),
                events: vec![],
                created_at: now_iso8601(),
                updated_at: now_iso8601(),
            },
        );
    }

    // TODO: Wire up actual agent execution via rs_adk::Runner
    let response = RunResponse {
        session_id,
        response: format!("Agent '{}' processed: {}", req.agent, req.message),
        events: vec![],
        state: HashMap::new(),
    };

    Json(response).into_response()
}

pub async fn run_agent_sse(
    State(state): State<ApiState>,
    Json(req): Json<RunRequest>,
) -> Sse<futures::stream::Once<futures::future::Ready<Result<Event, axum::Error>>>> {
    let _ = state;
    let event = Event::default()
        .event("message")
        .data(
            serde_json::json!({
                "type": "response",
                "agent": req.agent,
                "text": format!("Streaming response for: {}", req.message),
            })
            .to_string(),
        );

    Sse::new(futures::stream::once(futures::future::ready(Ok(event))))
}

// ── Agent Discovery ─────────────────────────────────────────────

pub async fn list_agents(State(state): State<ApiState>) -> Json<Vec<AgentEntry>> {
    Json(state.agents.as_ref().clone())
}

pub async fn get_agent(
    Path(name): Path<String>,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    match state.agents.iter().find(|a| a.name == name) {
        Some(agent) => Json(agent.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Session Management ──────────────────────────────────────────

pub async fn list_sessions(
    Path((app, user)): Path<(String, String)>,
    Query(query): Query<SessionQuery>,
    State(state): State<ApiState>,
) -> Json<Vec<SessionData>> {
    let sessions = state.sessions.read();
    let results: Vec<SessionData> = sessions
        .values()
        .filter(|s| s.app_name == app && s.user_id == user)
        .skip(query.offset)
        .take(query.limit)
        .cloned()
        .collect();
    Json(results)
}

pub async fn create_session(
    Path((app, user)): Path<(String, String)>,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso8601();
    let session = SessionData {
        id: id.clone(),
        app_name: app,
        user_id: user,
        state: HashMap::new(),
        events: vec![],
        created_at: now.clone(),
        updated_at: now,
    };
    state.sessions.write().insert(id.clone(), session.clone());
    (StatusCode::CREATED, Json(session))
}

pub async fn get_session(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    match state.sessions.read().get(&id) {
        Some(session) => Json(session.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn delete_session(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    match state.sessions.write().remove(&id) {
        Some(_) => StatusCode::NO_CONTENT.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn get_session_events(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    match state.sessions.read().get(&id) {
        Some(session) => Json(session.events.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn get_session_state(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    match state.sessions.read().get(&id) {
        Some(session) => Json(session.state.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn rewind_session(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ApiState>,
    Json(req): Json<RewindRequest>,
) -> impl IntoResponse {
    let mut sessions = state.sessions.write();
    match sessions.get_mut(&id) {
        Some(session) => {
            // Find the invocation boundary and truncate events
            let cutoff = session
                .events
                .iter()
                .rposition(|e| e.get("invocation_id").and_then(|v| v.as_str()) == Some(&req.invocation_id));

            let removed = match cutoff {
                Some(idx) => {
                    let count = session.events.len() - (idx + 1);
                    session.events.truncate(idx + 1);
                    session.updated_at = now_iso8601();
                    count
                }
                None => 0,
            };

            Json(serde_json::json!({
                "id": id,
                "invocation_id": req.invocation_id,
                "events_removed": removed,
            }))
            .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Artifacts ───────────────────────────────────────────────────

pub async fn list_artifacts(
    Path((_app, _user, session_id)): Path<(String, String, String)>,
    State(state): State<ApiState>,
) -> Json<Vec<ArtifactSummary>> {
    let artifacts = state.artifacts.read();
    let key_prefix = format!("{session_id}:");

    let mut summaries: HashMap<String, ArtifactSummary> = HashMap::new();
    for (key, versions) in artifacts.iter() {
        if key.starts_with(&key_prefix) {
            if let Some(latest) = versions.last() {
                summaries.insert(
                    latest.name.clone(),
                    ArtifactSummary {
                        name: latest.name.clone(),
                        versions: versions.len(),
                        latest_mime_type: latest.mime_type.clone(),
                        latest_size: latest.size,
                    },
                );
            }
        }
    }

    Json(summaries.into_values().collect())
}

pub async fn get_artifact(
    Path((_app, _user, session_id, name)): Path<(String, String, String, String)>,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    let key = format!("{session_id}:{name}");
    let artifacts = state.artifacts.read();
    match artifacts.get(&key).and_then(|v| v.last()) {
        Some(entry) => Json(entry.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn get_artifact_version(
    Path((_app, _user, session_id, name, version)): Path<(String, String, String, String, String)>,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    let key = format!("{session_id}:{name}");
    let version_num: usize = version.parse().unwrap_or(0);
    let artifacts = state.artifacts.read();
    match artifacts
        .get(&key)
        .and_then(|v| v.iter().find(|a| a.version == version_num))
    {
        Some(entry) => Json(entry.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Debug / Traces ──────────────────────────────────────────────

pub async fn get_trace(Path(trace_id): Path<String>) -> impl IntoResponse {
    // TODO: Wire up to telemetry span store
    Json(serde_json::json!({
        "trace_id": trace_id,
        "spans": [],
    }))
}

pub async fn health_check(State(state): State<ApiState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        agents_loaded: state.agents.len(),
        sessions_active: state.sessions.read().len(),
    })
}

// ── Eval ────────────────────────────────────────────────────────

pub async fn run_eval(Json(req): Json<EvalRunRequest>) -> impl IntoResponse {
    // TODO: Wire up to rs_adk::evaluation
    Json(serde_json::json!({
        "agent": req.agent,
        "status": "submitted",
        "criteria": req.criteria,
    }))
}

pub async fn list_eval_results() -> Json<Vec<EvalResultSummary>> {
    Json(vec![])
}

// ── Helpers ─────────────────────────────────────────────────────

fn now_iso8601() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}Z", dur.as_secs())
}
