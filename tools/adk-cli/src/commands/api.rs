use crate::manifest::{self, AgentManifest};
use axum::{
    extract::{Path as AxumPath, State as AxumState},
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for the API server command.
pub struct ApiConfig {
    pub agent_dir: String,
    pub host: String,
    pub port: u16,
    pub allow_origins: Option<String>,
    pub log_level: String,
    pub reload: bool,
    pub a2a: bool,
    pub trace_to_cloud: bool,
    pub session_service_uri: Option<String>,
    pub artifact_storage_uri: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    /// Discovered agents keyed by name.
    agents: Arc<HashMap<String, AgentManifest>>,
    /// In-memory session store: app -> user -> session_id -> session data.
    sessions: Arc<RwLock<HashMap<String, HashMap<String, HashMap<String, SessionData>>>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionData {
    session_id: String,
    app: String,
    user: String,
    messages: Vec<serde_json::Value>,
    #[serde(default)]
    metadata: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RunRequest {
    app: String,
    user: String,
    session_id: Option<String>,
    message: String,
}

#[derive(Debug, Serialize)]
struct RunResponse {
    session_id: String,
    events: Vec<AgentEvent>,
}

#[derive(Debug, Serialize)]
struct AgentEvent {
    event_type: String,
    data: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct AppInfo {
    name: String,
    description: String,
    model: String,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    #[serde(default)]
    metadata: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct TraceSpan {
    span_id: String,
    operation: String,
    start_ms: u64,
    end_ms: u64,
    #[serde(default)]
    attributes: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /list-apps
async fn list_apps(AxumState(state): AxumState<AppState>) -> Json<Vec<AppInfo>> {
    let apps: Vec<AppInfo> = state
        .agents
        .iter()
        .map(|(_, m)| AppInfo {
            name: m.name.clone(),
            description: m.description.clone(),
            model: m.model.clone(),
        })
        .collect();
    Json(apps)
}

/// POST /run
async fn run_agent(
    AxumState(state): AxumState<AppState>,
    Json(req): Json<RunRequest>,
) -> Result<Json<RunResponse>, (StatusCode, String)> {
    let _agent = state
        .agents
        .get(&req.app)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Agent '{}' not found", req.app)))?;

    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // TODO: Execute the agent with the message and collect events.
    let events = vec![AgentEvent {
        event_type: "text".to_string(),
        data: serde_json::json!({
            "content": format!("[{}] (placeholder response — LLM integration pending)", req.app),
        }),
    }];

    // Store in session
    {
        let mut sessions = state.sessions.write().await;
        let user_sessions = sessions
            .entry(req.app.clone())
            .or_default()
            .entry(req.user.clone())
            .or_default();
        let session = user_sessions
            .entry(session_id.clone())
            .or_insert_with(|| SessionData {
                session_id: session_id.clone(),
                app: req.app.clone(),
                user: req.user.clone(),
                messages: Vec::new(),
                metadata: serde_json::Value::Null,
            });
        session.messages.push(serde_json::json!({
            "role": "user",
            "content": req.message,
        }));
        for event in &events {
            session.messages.push(serde_json::json!({
                "role": "agent",
                "event": event,
            }));
        }
    }

    Ok(Json(RunResponse {
        session_id,
        events,
    }))
}

/// POST /run_sse — SSE streaming (stub returns single event)
async fn run_sse(
    AxumState(state): AxumState<AppState>,
    Json(req): Json<RunRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let _agent = state
        .agents
        .get(&req.app)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Agent '{}' not found", req.app)))?;

    // TODO: Implement actual SSE streaming with tokio channels.
    let body = format!(
        "data: {}\n\n",
        serde_json::json!({
            "event_type": "text",
            "data": {
                "content": format!("[{}] (placeholder — SSE streaming pending)", req.app),
            }
        })
    );

    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/event-stream; charset=utf-8",
        )],
        body,
    ))
}

/// GET /apps/:app/users/:user/sessions/:session
async fn get_session(
    AxumState(state): AxumState<AppState>,
    AxumPath((app, user, session)): AxumPath<(String, String, String)>,
) -> Result<Json<SessionData>, StatusCode> {
    let sessions = state.sessions.read().await;
    sessions
        .get(&app)
        .and_then(|u| u.get(&user))
        .and_then(|s| s.get(&session))
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// POST /apps/:app/users/:user/sessions
async fn create_session(
    AxumState(state): AxumState<AppState>,
    AxumPath((app, user)): AxumPath<(String, String)>,
    Json(req): Json<CreateSessionRequest>,
) -> (StatusCode, Json<SessionData>) {
    let session_id = uuid::Uuid::new_v4().to_string();
    let data = SessionData {
        session_id: session_id.clone(),
        app: app.clone(),
        user: user.clone(),
        messages: Vec::new(),
        metadata: req.metadata,
    };

    {
        let mut sessions = state.sessions.write().await;
        sessions
            .entry(app)
            .or_default()
            .entry(user)
            .or_default()
            .insert(session_id, data.clone());
    }

    (StatusCode::CREATED, Json(data))
}

/// DELETE /apps/:app/users/:user/sessions/:session
async fn delete_session(
    AxumState(state): AxumState<AppState>,
    AxumPath((app, user, session)): AxumPath<(String, String, String)>,
) -> StatusCode {
    let mut sessions = state.sessions.write().await;
    let removed = sessions
        .get_mut(&app)
        .and_then(|u| u.get_mut(&user))
        .and_then(|s| s.remove(&session));
    if removed.is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

/// GET /debug/trace/:event_id
async fn trace_event(AxumPath(event_id): AxumPath<String>) -> Json<Vec<TraceSpan>> {
    // TODO: Integrate with tracing/OpenTelemetry span storage.
    let _ = event_id;
    Json(vec![])
}

/// GET /debug/trace/session/:session_id
async fn trace_session(AxumPath(session_id): AxumPath<String>) -> Json<Vec<TraceSpan>> {
    // TODO: Integrate with tracing/OpenTelemetry span storage.
    let _ = session_id;
    Json(vec![])
}

// ---------------------------------------------------------------------------
// Server entry point
// ---------------------------------------------------------------------------

pub async fn run(config: ApiConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Logging
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log_level));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .init();

    dotenvy::dotenv().ok();

    // Discover agents
    let dir = PathBuf::from(&config.agent_dir);
    let discovered = manifest::discover_agents(&dir);
    if discovered.is_empty() {
        return Err(format!("No agent.toml found in '{}'", config.agent_dir).into());
    }

    let mut agents = HashMap::new();
    for (_path, m) in &discovered {
        tracing::info!("Discovered agent: {} (model: {})", m.name, m.model);
        agents.insert(m.name.clone(), m.clone());
    }

    let state = AppState {
        agents: Arc::new(agents),
        sessions: Arc::new(RwLock::new(HashMap::new())),
    };

    // CORS
    let cors = if let Some(ref origins) = config.allow_origins {
        let origins: Vec<_> = origins.split(',').map(|s| s.trim().to_string()).collect();
        let mut layer = CorsLayer::new();
        for origin in origins {
            if let Ok(o) = origin.parse::<HeaderValue>() {
                layer = layer.allow_origin(o);
            }
        }
        layer
    } else {
        CorsLayer::new().allow_origin(Any)
    };

    let router = Router::new()
        .route("/list-apps", get(list_apps))
        .route("/run", post(run_agent))
        .route("/run_sse", post(run_sse))
        .route(
            "/apps/{app}/users/{user}/sessions/{session}",
            get(get_session).delete(delete_session),
        )
        .route(
            "/apps/{app}/users/{user}/sessions",
            post(create_session),
        )
        .route("/debug/trace/{event_id}", get(trace_event))
        .route("/debug/trace/session/{session_id}", get(trace_session))
        .layer(cors)
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("ADK API server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}
