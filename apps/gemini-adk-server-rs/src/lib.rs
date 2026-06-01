//! gemini-adk-server-rs — Shared server core for all ADK server surfaces.
//!
//! Provides:
//! - Unified agent loading (TOML + JSON + programmatic)
//! - REST API router with all upstream ADK endpoints
//! - Pluggable session store (in-memory default, swap for DB-backed)
//! - Pluggable artifact store
//! - Shared request/response types
//!
//! Used by `gemini-adk-web-rs`, `gemini-adk-api-rs`, and `gemini-adk-cli-rs` — never run directly.

pub mod agents;
pub mod execution;
pub mod handlers;
pub mod router;
pub mod serve;
pub mod sessions;
pub mod types;
pub mod ws;

pub use agents::{AgentEntry, ServerAgentRegistry};
pub use execution::{build_text_agent, run_agent_turn, RunOutcome};
pub use router::build_api_router;
pub use serve::{run_server, ServeConfig};
pub use sessions::{InMemorySessionStore, SessionStore};
pub use types::*;
pub use ws::{
    handle_ws, AgentSource, AppCategory, AppError, AppInfo, AppRegistry, ClientMessage,
    ServerMessage, WsSender,
};

use std::sync::Arc;

/// Shared server state — passed to all Axum handlers.
///
/// Construct via [`ServerState::new`] and chain with [`ServerState::with_session_store`].
#[derive(Clone)]
pub struct ServerState {
    /// Registered agents.
    pub agents: Arc<ServerAgentRegistry>,
    /// Session store (pluggable).
    pub sessions: Arc<dyn SessionStore>,
    /// Artifact store.
    pub artifacts: Arc<parking_lot::RwLock<std::collections::HashMap<String, Vec<ArtifactEntry>>>>,
}

impl ServerState {
    /// Create with defaults (in-memory sessions and artifacts).
    pub fn new(agents: ServerAgentRegistry) -> Self {
        Self {
            agents: Arc::new(agents),
            sessions: Arc::new(InMemorySessionStore::new()),
            artifacts: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Create with a custom session store.
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.sessions = store;
        self
    }
}
