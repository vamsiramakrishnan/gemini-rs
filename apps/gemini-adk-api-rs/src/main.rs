//! gemini-adk-api-rs — Standalone headless REST API server for ADK agents.
//!
//! Thin wrapper around `gemini-adk-server-rs`. Auto-discovers agents from the
//! current directory (both `agent.json` and `agent.toml` formats) and
//! serves them via REST endpoints.
//!
//! ```bash
//! cargo run -p gemini-adk-api-rs
//! ADK_API_PORT=8080 cargo run -p gemini-adk-api-rs
//! ```

use gemini_adk_server_rs::{AgentRegistry, ServerState};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    init_tracing();

    // Discover agents from working directory
    let mut registry = AgentRegistry::new();
    let dir = std::env::current_dir().unwrap_or_default();
    let count = registry.discover(&dir);

    if count == 0 {
        tracing::warn!(
            "No agents discovered in '{}'. Place an agent.json or agent.toml in the working directory.",
            dir.display()
        );
    }

    let state = ServerState::new(registry);
    let app = gemini_adk_server_rs::build_api_router(state);

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
