use std::sync::Arc;

use axum::{
    extract::{Path, State, WebSocketUpgrade},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Router,
};
use tower_http::services::ServeDir;

mod app;
mod apps;
mod ws_handler;

use app::AppRegistry;

type SharedRegistry = Arc<AppRegistry>;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    // Initialize telemetry (tracing + optional OTel export)
    let _telemetry_guard = init_telemetry().await;

    let mut registry = AppRegistry::new();
    apps::register_all(&mut registry);
    let registry = Arc::new(registry);

    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

    let app = Router::new()
        .route("/", get(landing_page))
        .route("/app/:name", get(app_page))
        .route("/api/apps", get(list_apps))
        .route("/ws/:name", get(ws_upgrade))
        .nest_service("/static", ServeDir::new(static_dir))
        .layer(middleware::from_fn(cross_origin_isolation))
        .with_state(registry);

    let addr = "0.0.0.0:25125";
    tracing::info!("Cookbooks UI at http://localhost:25125");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn init_telemetry() -> rs_genai::telemetry::TelemetryGuard {
    let service_name = std::env::var("OTEL_SERVICE_NAME")
        .unwrap_or_else(|_| "gemini-live-cookbooks".to_string());
    let config = rs_genai::telemetry::TelemetryConfig {
        logging_enabled: true,
        log_filter: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        json_logs: false,
        otel_traces: true,
        otel_metrics: true,
        otel_service_name: service_name,
        otel_gcp_project: std::env::var("GOOGLE_CLOUD_PROJECT").ok(),
        ..Default::default()
    };

    // Prefer GCP-native export when the feature is compiled in and a project is available
    #[cfg(feature = "otel-gcp")]
    if config.otel_gcp_project.is_some() {
        return config.init_gcp().await.expect("Failed to initialize GCP telemetry");
    }

    // Fall back to OTLP (Jaeger, generic collector) or plain logging
    let mut fallback = config;
    #[cfg(not(feature = "otel-otlp"))]
    {
        fallback.otel_traces = false;
        fallback.otel_metrics = false;
    }
    #[cfg(feature = "otel-otlp")]
    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_err() {
        fallback.otel_traces = false;
        fallback.otel_metrics = false;
    }
    fallback.init().expect("Failed to initialize telemetry")
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

async fn landing_page() -> impl IntoResponse {
    Html(include_str!("../static/index.html"))
}

async fn app_page(
    Path(name): Path<String>,
    State(registry): State<SharedRegistry>,
) -> impl IntoResponse {
    if registry.get(&name).is_some() {
        Html(include_str!("../static/app.html")).into_response()
    } else {
        (axum::http::StatusCode::NOT_FOUND, "App not found").into_response()
    }
}

async fn list_apps(State(registry): State<SharedRegistry>) -> Json<Vec<app::AppInfo>> {
    Json(registry.list())
}

async fn ws_upgrade(
    Path(name): Path<String>,
    State(registry): State<SharedRegistry>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if let Some(app) = registry.get(&name) {
        ws.on_upgrade(move |socket| ws_handler::handle_ws(socket, app))
    } else {
        // Return 404 — upgrade and immediately close
        ws.on_upgrade(|socket| async move {
            let _ = socket.close().await;
        })
    }
}
