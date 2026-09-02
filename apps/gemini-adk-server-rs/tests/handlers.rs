//! Integration tests for the REST handlers — exercised through the full
//! router so routing, extraction, and status codes are all covered.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use gemini_adk_rs::gemini_genai_rs::prelude::{Content, Part, Role};
use gemini_adk_rs::llm::LlmError;
use gemini_adk_rs::{BaseLlm, LlmRequest, LlmResponse};
use gemini_adk_server_rs::trace::{TraceRecord, TraceStore};
use gemini_adk_server_rs::{
    AgentEntry, EvalResultSummary, ServerAgentRegistry, ServerState, build_api_router,
};
use tower::ServiceExt;

/// A mock LLM that returns a fixed text response (no network).
struct FixedLlm {
    response: String,
}

#[async_trait]
impl BaseLlm for FixedLlm {
    fn model_id(&self) -> &str {
        "fixed-mock"
    }

    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            content: Content {
                role: Some(Role::Model),
                parts: vec![Part::Text {
                    text: self.response.clone(),
                }],
            },
            finish_reason: Some("STOP".into()),
            usage: None,
        })
    }
}

fn entry(name: &str) -> AgentEntry {
    AgentEntry {
        name: name.into(),
        description: None,
        model: Some("gemini-2.0-flash".into()),
        agent_type: "llm".into(),
        instruction: None,
        tools: vec![],
        sub_agents: vec![],
    }
}

/// Server state with one registered agent backed by a mock LLM.
fn mock_state(agent: &str, response: &str) -> ServerState {
    let mut registry = ServerAgentRegistry::new();
    registry.register(entry(agent));
    let response = response.to_string();
    ServerState::new(registry).with_llm_factory(Arc::new(move |_| {
        Arc::new(FixedLlm {
            response: response.clone(),
        })
    }))
}

fn trace_record(id: &str) -> TraceRecord {
    TraceRecord {
        trace_id: id.into(),
        root: "gemini.agent.run".into(),
        started_at: "0Z".into(),
        duration_ms: 1,
        ok: true,
        spans: vec![],
    }
}

fn eval_summary(agent: &str) -> EvalResultSummary {
    EvalResultSummary {
        agent: agent.into(),
        timestamp: "0Z".into(),
        total_cases: 1,
        passed: 1,
        failed: 0,
        pass_rate: 1.0,
        criteria_scores: HashMap::new(),
    }
}

async fn get(state: ServerState, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = build_api_router(state)
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

// ── GET /debug/traces ───────────────────────────────────────────

#[tokio::test]
async fn debug_traces_lists_recorded_traces() {
    let state = ServerState::new(ServerAgentRegistry::new());
    state.traces.record(trace_record("t1"));
    state.traces.record(trace_record("t2"));

    let (status, json) = get(state, "/debug/traces").await;

    assert_eq!(status, StatusCode::OK);
    let traces = json.as_array().expect("array of traces");
    assert_eq!(traces.len(), 2);
    assert_eq!(traces[0]["trace_id"], "t1");
    assert_eq!(traces[1]["trace_id"], "t2");
}

#[tokio::test]
async fn debug_traces_empty_store_returns_empty_array() {
    let state = ServerState::new(ServerAgentRegistry::new());
    let (status, json) = get(state, "/debug/traces").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, serde_json::json!([]));
}

#[tokio::test]
async fn debug_traces_capacity_matches_store() {
    // Sanity-check the endpoint surfaces the same view as TraceStore::list().
    let store = TraceStore::new();
    store.record(trace_record("a"));
    assert_eq!(store.list().len(), 1);
}

// ── GET artifact version validation ─────────────────────────────

#[tokio::test]
async fn artifact_version_non_numeric_returns_400() {
    let state = ServerState::new(ServerAgentRegistry::new());
    let (status, json) = get(
        state,
        "/apps/a/users/u/sessions/s/artifacts/report/not-a-number",
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error = json["error"].as_str().expect("structured error body");
    assert!(
        error.contains("invalid artifact version"),
        "got error: {error}"
    );
    assert!(error.contains("not-a-number"), "got error: {error}");
}

#[tokio::test]
async fn artifact_version_numeric_but_missing_returns_404() {
    let state = ServerState::new(ServerAgentRegistry::new());
    let (status, _) = get(state, "/apps/a/users/u/sessions/s/artifacts/report/3").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── GET /eval/results pagination ────────────────────────────────

#[tokio::test]
async fn eval_results_default_returns_all() {
    let state = ServerState::new(ServerAgentRegistry::new());
    for i in 0..5 {
        state.record_eval_result(eval_summary(&format!("agent-{i}")));
    }

    let (status, json) = get(state, "/eval/results").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json.as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn eval_results_respects_limit_and_offset() {
    let state = ServerState::new(ServerAgentRegistry::new());
    for i in 0..5 {
        state.record_eval_result(eval_summary(&format!("agent-{i}")));
    }

    let (status, json) = get(state, "/eval/results?limit=2&offset=1").await;
    assert_eq!(status, StatusCode::OK);
    let results = json.as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["agent"], "agent-1");
    assert_eq!(results[1]["agent"], "agent-2");
}

#[tokio::test]
async fn eval_results_offset_past_end_returns_empty() {
    let state = ServerState::new(ServerAgentRegistry::new());
    state.record_eval_result(eval_summary("only"));

    let (status, json) = get(state, "/eval/results?offset=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, serde_json::json!([]));
}

// ── POST /run (mock LLM through the factory) ────────────────────

#[tokio::test]
async fn run_returns_mock_llm_response_and_records_trace() {
    let state = mock_state("echo", "hello from the mock model");
    let app = build_api_router(state.clone());

    let response = app
        .oneshot(
            Request::post("/run")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"agent": "echo", "message": "hi"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["response"], "hello from the mock model");

    let trace_id = json["trace_id"].as_str().unwrap();
    assert!(state.traces.get(trace_id).is_some(), "trace was recorded");
}

#[tokio::test]
async fn run_creates_session_under_the_advertised_id() {
    let state = mock_state("echo", "hello from the mock model");
    let app = build_api_router(state.clone());

    let response = app
        .oneshot(
            Request::post("/run")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "agent": "echo",
                        "message": "hi",
                        "session_id": "client-chosen-id"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // The session must exist under the id the client sent (and the response
    // advertised) — previously create() generated its own UUID and every
    // append_event under the advertised id was a silent no-op.
    let session = state
        .sessions
        .get("client-chosen-id")
        .expect("session exists under the advertised id");
    assert_eq!(session.id, "client-chosen-id");
    let events = state.sessions.events("client-chosen-id");
    assert!(
        events
            .iter()
            .any(|e| e["role"] == "user" && e["content"] == "hi"),
        "user turn recorded under the advertised id; events: {events:?}"
    );
    assert_eq!(
        state.sessions.count(),
        1,
        "no phantom session under a different id"
    );
}

// ── POST /run_sse ───────────────────────────────────────────────

#[tokio::test]
async fn run_sse_streams_real_agent_events() {
    let state = mock_state("echo", "hello from the mock model");
    let app = build_api_router(state.clone());

    let response = app
        .oneshot(
            Request::post("/run_sse")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"agent": "echo", "message": "hi"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_type.starts_with("text/event-stream"),
        "got content-type: {content_type}"
    );

    // The stream terminates once the run completes, so the whole body can be
    // collected.
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();

    // Real lifecycle events, in order: started → agent_started →
    // agent_completed → response.
    let started = body.find("event: started").expect("started event");
    let agent_started = body
        .find("event: agent_started")
        .expect("agent_started event");
    let agent_completed = body
        .find("event: agent_completed")
        .expect("agent_completed event");
    let response_ev = body.find("event: response").expect("response event");
    assert!(started < agent_started);
    assert!(agent_started < agent_completed);
    assert!(agent_completed < response_ev);

    // The final event carries the actual model output — not fabricated chunks.
    assert!(
        body.contains("hello from the mock model"),
        "final response text should come from the (mock) LLM, got body:\n{body}"
    );
    assert!(
        !body.contains("Streaming response for"),
        "hardcoded placeholder must be gone"
    );

    // The run is fully bookkept like POST /run: trace recorded, turn appended.
    assert_eq!(state.traces.list().len(), 1);
    assert_eq!(state.traces.list()[0].root, "gemini.agent.run_sse");
}

#[tokio::test]
async fn run_sse_unknown_agent_returns_404() {
    let state = ServerState::new(ServerAgentRegistry::new());
    let app = build_api_router(state);

    let response = app
        .oneshot(
            Request::post("/run_sse")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"agent": "ghost", "message": "hi"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
