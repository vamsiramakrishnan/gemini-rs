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

use gemini_adk_server_rs::{run_server, ServeConfig};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    init_tracing();

    // CLI/env arg parsing stays local to this binary.
    let dir = std::env::current_dir().unwrap_or_default();
    let port: u16 = std::env::var("ADK_API_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8000);

    // Delegate discover → build router → bind → serve to the shared helper.
    let config = ServeConfig::new(dir).host("0.0.0.0").port(port);

    if let Err(e) = run_server(config).await {
        tracing::error!("Server error: {e}");
        std::process::exit(1);
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
