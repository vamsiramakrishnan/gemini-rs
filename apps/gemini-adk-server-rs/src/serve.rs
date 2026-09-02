//! Reusable "discover agents → build router → bind → serve" entrypoint.
//!
//! Both `gemini-adk-api-rs` (the standalone binary) and the `adk api` CLI
//! command are thin wrappers over this helper. Each binary keeps its own
//! CLI-argument parsing and logging setup, then delegates the actual server
//! lifecycle here so the discover/build/serve logic lives in exactly one place.

use std::path::PathBuf;

use crate::{ServerAgentRegistry, ServerState, build_api_router};

/// Configuration for serving the ADK REST API.
pub struct ServeConfig {
    /// Directory to discover agents from (`agent.json` / `agent.toml`).
    pub agent_dir: PathBuf,
    /// Host/interface to bind (e.g. `"0.0.0.0"`).
    pub host: String,
    /// Port to bind.
    pub port: u16,
    /// If `true`, return an error when no agents are discovered. If `false`,
    /// log a warning and serve anyway (useful for dev where agents are added
    /// later).
    pub require_agents: bool,
}

impl ServeConfig {
    /// Create a config with the conventional defaults (`0.0.0.0:8000`, current
    /// directory, agents not required).
    pub fn new(agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            host: "0.0.0.0".to_string(),
            port: 8000,
            require_agents: false,
        }
    }

    /// Set the bind host.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Set the bind port.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Require at least one discovered agent.
    pub fn require_agents(mut self, require: bool) -> Self {
        self.require_agents = require;
        self
    }
}

/// Discover agents, build the API router, bind, and serve until shutdown.
///
/// This is the shared core behind the standalone `gemini-adk-api-rs` binary and
/// the `adk api` CLI command. Logging/tracing and `.env` loading are the
/// caller's responsibility (they differ per binary).
pub async fn run_server(config: ServeConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Discover agents via the unified registry.
    let mut registry = ServerAgentRegistry::new();
    let count = registry.discover(&config.agent_dir);

    if count == 0 {
        if config.require_agents {
            return Err(format!(
                "No agents found in '{}'. Place an agent.toml or agent.json in the directory.",
                config.agent_dir.display()
            )
            .into());
        }
        tracing::warn!(
            "No agents discovered in '{}'. Place an agent.json or agent.toml in the directory.",
            config.agent_dir.display()
        );
    }

    let state = ServerState::new(registry);
    let app = build_api_router(state);

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("ADK API server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
