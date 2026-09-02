use gemini_adk_server_rs::{ServeConfig, run_server};

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

/// Start the headless API server — delegates to `gemini-adk-server-rs`.
pub async fn run(config: ApiConfig) -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log_level));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    dotenvy::dotenv().ok();

    // Delegate discover → build router → bind → serve to the shared helper.
    let serve_config = ServeConfig::new(std::path::PathBuf::from(&config.agent_dir))
        .host(config.host)
        .port(config.port)
        .require_agents(true);

    run_server(serve_config).await
}
