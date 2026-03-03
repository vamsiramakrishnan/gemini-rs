use std::sync::Arc;

use axum::{
    extract::{Path, State, WebSocketUpgrade},
    response::{Html, IntoResponse, Json},
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
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

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
        .with_state(registry);

    let addr = "0.0.0.0:25125";
    tracing::info!("Cookbooks UI at http://localhost:25125");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
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
