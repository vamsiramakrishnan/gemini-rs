use std::path::PathBuf;

use adk_server_core::{AgentRegistry, ServerState};

/// Configuration for the web dev server command.
#[allow(dead_code)]
pub struct WebConfig {
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

/// Start the web dev server — serves the REST API (use `adk-web` crate for full UI).
///
/// The CLI `adk web` command provides the same REST API as `adk api` but on a
/// different default port. For the full interactive web UI with devtools,
/// run `adk-web` directly.
pub async fn run(config: WebConfig) -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log_level));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    dotenvy::dotenv().ok();

    // Discover agents via unified registry
    let dir = PathBuf::from(&config.agent_dir);
    let mut registry = AgentRegistry::new();
    let count = registry.discover(&dir);

    if count == 0 {
        return Err(format!(
            "No agents found in '{}'. Place an agent.toml or agent.json in the directory.",
            config.agent_dir
        )
        .into());
    }

    let state = ServerState::new(registry);
    let app = adk_server_core::build_api_router(state);

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("ADK web server listening on http://{addr}");
    tracing::info!("For the full interactive UI with devtools, run: cargo run -p adk-web");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
