//! adk-server-core — Shared server core for all ADK server surfaces.
//!
//! Provides:
//! - Unified agent loading (TOML + JSON + programmatic)
//! - REST API router with all upstream ADK endpoints
//! - Pluggable session store (in-memory default, swap for DB-backed)
//! - Pluggable artifact store
//! - Shared request/response types
//!
//! Used by `adk-web`, `adk-api-server`, and `adk-cli` — never run directly.

pub mod agents;
pub mod handlers;
pub mod router;
pub mod sessions;
pub mod types;

pub use agents::{AgentEntry, AgentRegistry};
pub use router::build_api_router;
pub use sessions::{InMemorySessionStore, SessionStore};
pub use types::*;

use std::sync::Arc;

/// Shared server state — passed to all Axum handlers.
///
/// Construct via [`ServerState::new`] and chain with [`ServerState::with_session_store`].
#[derive(Clone)]
pub struct ServerState {
    /// Registered agents.
    pub agents: Arc<AgentRegistry>,
    /// Session store (pluggable).
    pub sessions: Arc<dyn SessionStore>,
    /// Artifact store.
    pub artifacts: Arc<parking_lot::RwLock<std::collections::HashMap<String, Vec<ArtifactEntry>>>>,
}

impl ServerState {
    /// Create with defaults (in-memory sessions and artifacts).
    pub fn new(agents: AgentRegistry) -> Self {
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
