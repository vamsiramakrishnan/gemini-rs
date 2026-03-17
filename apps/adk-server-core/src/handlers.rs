//! REST endpoint handlers — single implementation used by all server surfaces.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Json,
    },
};
use crate::{agents::AgentEntry, types::*, ServerState};

// ── Agent Execution ─────────────────────────────────────────────

pub async fn run_agent(
    State(state): State<ServerState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    let Some(agent) = state.agents.get(&req.agent) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Agent '{}' not found", req.agent)})),
        )
            .into_response();
    };

    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Ensure session exists
    if state.sessions.get(&session_id).is_none() {
        state.sessions.create(&req.agent, &req.user_id);
    }

    // Record user message
    state.sessions.append_event(
        &session_id,
        serde_json::json!({"role": "user", "content": req.message}),
    );

    // TODO: Execute agent via rs-adk Runner and collect real events.
    let response_text = format!(
        "[{}] (placeholder — wire up rs_adk::Runner for real execution)",
        agent.name
    );

    let events = vec![AgentEvent {
        event_type: "text".into(),
        data: serde_json::json!({"content": &response_text}),
    }];

    // Record agent response
    state.sessions.append_event(
        &session_id,
        serde_json::json!({"role": "agent", "content": &response_text}),
    );

    let session_state = state.sessions.state(&session_id);

    Json(RunResponse {
        session_id,
        response: response_text,
        events,
        state: session_state,
    })
    .into_response()
}

pub async fn run_agent_sse(
    State(state): State<ServerState>,
    Json(req): Json<RunRequest>,
) -> Sse<futures::stream::Once<futures::future::Ready<Result<Event, axum::Error>>>> {
    let agent_name = state
        .agents
        .get(&req.agent)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| req.agent.clone());

    let event = Event::default().event("message").data(
        serde_json::json!({
            "type": "response",
            "agent": agent_name,
            "text": format!("Streaming response for: {}", req.message),
        })
        .to_string(),
    );

    Sse::new(futures::stream::once(futures::future::ready(Ok(event))))
}

// ── Agent Discovery ─────────────────────────────────────────────

pub async fn list_agents(State(state): State<ServerState>) -> Json<Vec<AgentEntry>> {
    Json(state.agents.list().into_iter().cloned().collect())
}

pub async fn get_agent(
    Path(name): Path<String>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    match state.agents.get(&name) {
        Some(agent) => Json(agent.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Session Management ──────────────────────────────────────────

pub async fn list_sessions(
    Path((app, user)): Path<(String, String)>,
    Query(query): Query<SessionQuery>,
    State(state): State<ServerState>,
) -> Json<Vec<SessionData>> {
    Json(state.sessions.list(&app, &user, query.limit, query.offset))
}

pub async fn create_session(
    Path((app, user)): Path<(String, String)>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    let session = state.sessions.create(&app, &user);
    (StatusCode::CREATED, Json(session))
}

pub async fn get_session(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    match state.sessions.get(&id) {
        Some(session) => Json(session).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn delete_session(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    if state.sessions.delete(&id) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

pub async fn get_session_events(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    if state.sessions.get(&id).is_some() {
        Json(state.sessions.events(&id)).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub async fn get_session_state(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    if state.sessions.get(&id).is_some() {
        Json(state.sessions.state(&id)).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub async fn rewind_session(
    Path((_app, _user, id)): Path<(String, String, String)>,
    State(state): State<ServerState>,
    Json(req): Json<RewindRequest>,
) -> impl IntoResponse {
    if state.sessions.get(&id).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let removed = state.sessions.rewind(&id, &req.invocation_id);
    Json(serde_json::json!({
        "id": id,
        "invocation_id": req.invocation_id,
        "events_removed": removed,
    }))
    .into_response()
}

// ── Artifacts ───────────────────────────────────────────────────

pub async fn list_artifacts(
    Path((_app, _user, session_id)): Path<(String, String, String)>,
    State(state): State<ServerState>,
) -> Json<Vec<ArtifactSummary>> {
    let artifacts = state.artifacts.read();
    let prefix = format!("{session_id}:");

    let summaries: Vec<ArtifactSummary> = artifacts
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .filter_map(|(_, versions)| {
            versions.last().map(|latest| ArtifactSummary {
                name: latest.name.clone(),
                versions: versions.len(),
                latest_mime_type: latest.mime_type.clone(),
                latest_size: latest.size,
            })
        })
        .collect();

    Json(summaries)
}

pub async fn get_artifact(
    Path((_app, _user, session_id, name)): Path<(String, String, String, String)>,
    State(state): State<ServerState>,
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
    State(state): State<ServerState>,
) -> impl IntoResponse {
    let key = format!("{session_id}:{name}");
    let ver: usize = version.parse().unwrap_or(0);
    let artifacts = state.artifacts.read();
    match artifacts
        .get(&key)
        .and_then(|v| v.iter().find(|a| a.version == ver))
    {
        Some(entry) => Json(entry.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Debug ───────────────────────────────────────────────────────

pub async fn get_trace(Path(trace_id): Path<String>) -> impl IntoResponse {
    // TODO: Wire up to telemetry span store
    Json(serde_json::json!({ "trace_id": trace_id, "spans": [] }))
}

pub async fn health_check(State(state): State<ServerState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        agents_loaded: state.agents.len(),
        sessions_active: state.sessions.count(),
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
