//! REST endpoint handlers — single implementation used by all server surfaces.

use crate::{agents::AgentEntry, types::*, ServerState};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json, Response,
    },
};
use std::sync::Arc;

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

    // Build a runnable agent from the registry entry (clone the metadata so we
    // don't hold a borrow across the await point). The LLM comes from the
    // state's factory so embedders/tests can swap the provider.
    let runnable = crate::execution::build_text_agent_with(agent, (state.llm_factory)(agent), None);

    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Ensure the session exists under the advertised id.
    state
        .sessions
        .get_or_create(&session_id, &req.agent, &req.user_id);

    // Snapshot prior session state so the agent sees accumulated context, then
    // record the user message (mirrors ADK Runner appending the user turn).
    let prior_state = state.sessions.state(&session_id);
    state.sessions.append_event(
        &session_id,
        serde_json::json!({"role": "user", "content": req.message}),
    );

    // Execute the agent to completion and collect the produced events, recording
    // a trace span tree for the debug endpoint.
    let mut trace = crate::trace::TraceBuilder::new("gemini.agent.run");
    let trace_id = trace.trace_id().to_string();
    let span_start = std::time::Instant::now();
    let result = crate::execution::run_agent_turn(&runnable, &req.message, &prior_state).await;
    let span_dur = span_start.elapsed();

    let outcome = match result {
        Ok(outcome) => {
            trace.span(
                "agent.run",
                span_start,
                span_dur,
                serde_json::json!({"agent": req.agent, "session_id": session_id, "status": "ok"}),
            );
            state.traces.record(trace.finish());
            outcome
        }
        Err(e) => {
            let msg = format!("Agent execution failed: {e}");
            trace.span(
                "agent.run",
                span_start,
                span_dur,
                serde_json::json!({"agent": req.agent, "session_id": session_id, "status": "error", "error": &msg}),
            );
            trace.fail();
            state.traces.record(trace.finish());
            state.sessions.append_event(
                &session_id,
                serde_json::json!({"role": "error", "content": &msg}),
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": msg, "session_id": session_id, "trace_id": trace_id})),
            )
                .into_response();
        }
    };

    // Record the agent response.
    state.sessions.append_event(
        &session_id,
        serde_json::json!({"role": "agent", "content": &outcome.response}),
    );

    let session_state = state.sessions.state(&session_id);

    Json(RunResponse {
        session_id,
        response: outcome.response,
        events: outcome.events,
        state: session_state,
        trace_id,
    })
    .into_response()
}

/// `POST /run_sse` — execute an agent and stream real lifecycle events.
///
/// Emits Server-Sent Events as the run progresses, in order:
///
/// - `started` — run accepted; carries `session_id` and `trace_id`
/// - `agent_started` / `agent_completed` — agent lifecycle (from the runtime's
///   middleware events)
/// - `tool_call_started` / `tool_call_completed` / `tool_call_failed` — fired
///   per tool dispatch when the agent calls tools
/// - `response` — the final agent text, or `error` on failure
///
/// Note on granularity: the agent runtime's [`gemini_adk_rs::BaseLlm`] exposes
/// only a request/response `generate()` — there is no token-level streaming
/// API — so this endpoint streams the real execution milestones above rather
/// than fabricated token chunks.
pub async fn run_agent_sse(
    State(state): State<ServerState>,
    Json(req): Json<RunRequest>,
) -> Response {
    let Some(entry) = state.agents.get(&req.agent).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Agent '{}' not found", req.agent)})),
        )
            .into_response();
    };

    let session_id = req
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Ensure the session exists under the advertised id, then record the
    // user turn (mirrors `POST /run`).
    state
        .sessions
        .get_or_create(&session_id, &req.agent, &req.user_id);
    let prior_state = state.sessions.state(&session_id);
    state.sessions.append_event(
        &session_id,
        serde_json::json!({"role": "user", "content": req.message}),
    );

    // Lifecycle events flow through this channel: the `ChannelEvents`
    // middleware forwards runtime events, and the driver task adds the
    // `started` / `response` / `error` envelope events.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

    let runnable = crate::execution::build_text_agent_with(
        &entry,
        (state.llm_factory)(&entry),
        Some(Arc::new(crate::execution::ChannelEvents::new(tx.clone()))),
    );

    let mut trace = crate::trace::TraceBuilder::new("gemini.agent.run_sse");
    let trace_id = trace.trace_id().to_string();

    let _ = tx.send(serde_json::json!({
        "type": "started",
        "agent": req.agent,
        "session_id": session_id,
        "trace_id": trace_id,
    }));

    // Drive the agent in a background task; the response streams as it runs.
    let sessions = state.sessions.clone();
    let traces = state.traces.clone();
    let agent_name = req.agent.clone();
    let message = req.message.clone();
    tokio::spawn(async move {
        let span_start = std::time::Instant::now();
        let result = crate::execution::run_agent_turn(&runnable, &message, &prior_state).await;
        let span_dur = span_start.elapsed();

        match result {
            Ok(outcome) => {
                trace.span(
                    "agent.run",
                    span_start,
                    span_dur,
                    serde_json::json!({"agent": agent_name, "session_id": session_id, "status": "ok"}),
                );
                traces.record(trace.finish());
                sessions.append_event(
                    &session_id,
                    serde_json::json!({"role": "agent", "content": &outcome.response}),
                );
                let _ = tx.send(serde_json::json!({
                    "type": "response",
                    "agent": agent_name,
                    "session_id": session_id,
                    "trace_id": trace_id,
                    "text": outcome.response,
                }));
            }
            Err(e) => {
                let msg = format!("Agent execution failed: {e}");
                trace.span(
                    "agent.run",
                    span_start,
                    span_dur,
                    serde_json::json!({"agent": agent_name, "session_id": session_id, "status": "error", "error": &msg}),
                );
                trace.fail();
                traces.record(trace.finish());
                sessions.append_event(
                    &session_id,
                    serde_json::json!({"role": "error", "content": &msg}),
                );
                let _ = tx.send(serde_json::json!({
                    "type": "error",
                    "agent": agent_name,
                    "session_id": session_id,
                    "trace_id": trace_id,
                    "error": msg,
                }));
            }
        }
        // `tx` (and the middleware's clone, held by `runnable`) drop here,
        // closing the channel and terminating the SSE stream.
    });

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|payload| {
            let name = payload
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("message")
                .to_string();
            let event = Event::default().event(name).data(payload.to_string());
            (Ok::<_, std::convert::Infallible>(event), rx)
        })
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
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
    Query(query): Query<PageQuery>,
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
    let ver: usize = match version.parse() {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "invalid artifact version '{version}': expected a non-negative integer"
                    ),
                })),
            )
                .into_response();
        }
    };
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

/// `GET /debug/traces` — list all retained execution traces, oldest first.
pub async fn list_traces(State(state): State<ServerState>) -> Json<Vec<crate::trace::TraceRecord>> {
    Json(state.traces.list())
}

pub async fn get_trace(
    Path(trace_id): Path<String>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    match state.traces.get(&trace_id) {
        Some(record) => Json(record).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "trace not found", "trace_id": trace_id })),
        )
            .into_response(),
    }
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

pub async fn run_eval(
    State(state): State<ServerState>,
    Json(req): Json<EvalRunRequest>,
) -> impl IntoResponse {
    match crate::eval::run_evalset(&state, &req).await {
        Ok(summary) => Json(summary).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err })),
        )
            .into_response(),
    }
}

/// `GET /eval/results` — list eval run summaries with `limit`/`offset`
/// pagination (same parameters as session listing; default limit 50).
pub async fn list_eval_results(
    Query(query): Query<PageQuery>,
    State(state): State<ServerState>,
) -> Json<Vec<EvalResultSummary>> {
    Json(
        state
            .eval_results
            .read()
            .iter()
            .skip(query.offset)
            .take(query.limit)
            .cloned()
            .collect(),
    )
}
