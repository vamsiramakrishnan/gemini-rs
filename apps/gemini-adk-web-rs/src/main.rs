use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State, WebSocketUpgrade},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
};
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

mod api;
mod app;
mod apps;
mod bridge;
mod span_layer;
mod studio;
mod ws_handler;

use app::{AppRegistry, ServerMessage};

/// Shared application state passed to all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    registry: Arc<AppRegistry>,
    /// Broadcast sender for span events from the tracing layer.
    /// Each WebSocket handler subscribes to receive span events.
    span_tx: broadcast::Sender<ServerMessage>,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    // Initialize telemetry with WebSocketSpanLayer wired in
    let (_telemetry_guard, span_tx) = init_telemetry().await;

    let mut registry = AppRegistry::new();
    apps::register_all(&mut registry);

    let state = AppState {
        registry: Arc::new(registry),
        span_tx,
    };

    // Discover agents from working directory for the REST API
    let mut agent_registry = gemini_adk_server_rs::ServerAgentRegistry::new();
    let dir = std::env::current_dir().unwrap_or_default();
    agent_registry.discover(&dir);
    let server_state = gemini_adk_server_rs::ServerState::new(agent_registry);

    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

    // Build the API router first (has its own state baked in)
    let api = api::api_router(server_state);

    let app = Router::new()
        .route("/", get(landing_page))
        .route("/app/{name}", get(app_page))
        .route("/api/apps", get(list_apps))
        .route("/ws/{name}", get(ws_upgrade))
        .route("/favicon.ico", get(favicon))
        .with_state(state)
        .merge(studio::router(static_dir))
        .merge(api)
        .nest_service("/static", ServeDir::new(static_dir))
        .layer(middleware::from_fn(no_cache_static))
        .layer(middleware::from_fn(cross_origin_isolation));

    // Local only unless the operator says otherwise: the server has no
    // authentication of its own.
    let addr = std::env::var("ADK_WEB_ADDR").unwrap_or_else(|_| "127.0.0.1:25125".to_string());
    tracing::info!("ADK Web UI at http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Logging, the browser's span stream, and, when the environment asks for
/// it, trace and metric export (`TelemetryConfig::from_env`):
/// `OTEL_EXPORTER_OTLP_ENDPOINT` with the `otel-otlp` feature, or
/// `ADK_TELEMETRY=gcp` with `otel-gcp`.
#[cfg_attr(
    not(feature = "otel-gcp"),
    allow(clippy::unused_async, reason = "Cloud Trace setup is async")
)]
async fn init_telemetry() -> (
    gemini_genai_rs::telemetry::TelemetryGuard,
    broadcast::Sender<ServerMessage>,
) {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::prelude::*;

    // Create the WebSocketSpanLayer (bridges tracing spans → browser)
    let (ws_layer, span_tx) = span_layer::WebSocketSpanLayer::new(256);
    let config = gemini_genai_rs::telemetry::TelemetryConfig::from_env();
    let filter = EnvFilter::try_new(&config.log_filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(ws_layer);

    #[cfg(any(feature = "otel-otlp", feature = "otel-gcp"))]
    {
        let exporters = if config.wants_gcp() {
            #[cfg(feature = "otel-gcp")]
            {
                config.build_gcp().await
            }
            #[cfg(not(feature = "otel-gcp"))]
            {
                Err("ADK_TELEMETRY=gcp needs the otel-gcp feature".into())
            }
        } else if config.otel_traces || config.otel_metrics {
            #[cfg(feature = "otel-otlp")]
            {
                config.build_otlp()
            }
            #[cfg(not(feature = "otel-otlp"))]
            {
                Err("OTEL_EXPORTER_OTLP_ENDPOINT needs the otel-otlp feature".into())
            }
        } else {
            Ok(gemini_genai_rs::telemetry::Exporters::default())
        };
        match exporters {
            Ok(exporters) => {
                let subscriber = registry.with(exporters.layer());
                tracing::subscriber::set_global_default(subscriber)
                    .expect("Failed to set tracing subscriber");
                return (exporters.guard, span_tx);
            }
            Err(e) => eprintln!("telemetry export disabled: {e}"),
        }
    }

    tracing::subscriber::set_global_default(registry).expect("Failed to set tracing subscriber");
    (Default::default(), span_tx)
}

/// Disable caching for static files during development.
async fn no_cache_static(request: axum::http::Request<axum::body::Body>, next: Next) -> Response {
    let is_static = request.uri().path().starts_with("/static");
    let mut response = next.run(request).await;
    if is_static {
        response.headers_mut().insert(
            "cache-control",
            "no-cache, no-store, must-revalidate".parse().unwrap(),
        );
    }
    response
}

/// Cross-Origin-Isolation middleware — enables SharedArrayBuffer in AudioWorklet.
/// Uses `credentialless` COEP (not `require-corp`) so cross-origin resources
/// like Google Fonts load without needing explicit CORS headers.
async fn cross_origin_isolation(
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.insert("cross-origin-opener-policy", "same-origin".parse().unwrap());
    h.insert(
        "cross-origin-embedder-policy",
        "credentialless".parse().unwrap(),
    );
    response
}

async fn favicon() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><text y='.9em' font-size='90'>&#x1F680;</text></svg>",
    )
}

async fn landing_page() -> impl IntoResponse {
    Html(include_str!("../static/index.html"))
}

async fn app_page(Path(name): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    if state.registry.get(&name).is_some() {
        Html(include_str!("../static/app.html")).into_response()
    } else {
        (axum::http::StatusCode::NOT_FOUND, "App not found").into_response()
    }
}

async fn list_apps(State(state): State<AppState>) -> Json<Vec<app::AppInfo>> {
    Json(state.registry.list())
}

async fn ws_upgrade(
    Path(name): Path<String>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if let Some(app) = state.registry.get(&name) {
        let span_rx = state.span_tx.subscribe();
        ws.on_upgrade(move |socket| ws_handler::handle_ws(socket, app, span_rx))
    } else {
        // Return 404 — upgrade and immediately close
        ws.on_upgrade(|mut socket| async move {
            use futures::SinkExt as _;
            let _ = socket.close().await;
        })
    }
}
