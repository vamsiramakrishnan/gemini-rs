use std::path::PathBuf;

use gemini_adk_server::{AgentRegistry, ServerState};

/// Configuration for the API server command.
#[allow(dead_code)]
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

/// Start the headless API server — delegates to `gemini-adk-server`.
pub async fn run(config: ApiConfig) -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log_level));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    dotenvy::dotenv().ok();

    // Discover agents via gemini-adk-server unified registry
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
    let app = gemini_adk_server::build_api_router(state);

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("ADK API server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
