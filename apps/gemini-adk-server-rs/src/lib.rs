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
pub mod eval;
pub mod execution;
pub mod handlers;
pub mod router;
pub mod serve;
pub mod sessions;
pub mod trace;
pub mod types;
pub mod ws;

pub use agents::{AgentEntry, ServerAgentRegistry};
pub use execution::{
    build_text_agent, build_text_agent_with, run_agent_turn, ChannelEvents, LlmFactory, RunOutcome,
};
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
    /// Completed evaluation run summaries, newest last.
    pub eval_results: Arc<parking_lot::RwLock<Vec<EvalResultSummary>>>,
    /// Recent execution traces, queryable via the debug endpoint.
    pub traces: Arc<trace::TraceStore>,
    /// Resolves the LLM backing an agent entry (defaults to `GeminiLlm` with
    /// environment auth). Swap via [`ServerState::with_llm_factory`] to inject
    /// a mock LLM in tests or a custom provider when embedding.
    pub llm_factory: LlmFactory,
}

impl ServerState {
    /// Create with defaults (in-memory sessions and artifacts).
    pub fn new(agents: ServerAgentRegistry) -> Self {
        Self {
            agents: Arc::new(agents),
            sessions: Arc::new(InMemorySessionStore::new()),
            artifacts: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            eval_results: Arc::new(parking_lot::RwLock::new(Vec::new())),
            traces: Arc::new(trace::TraceStore::new()),
            llm_factory: Arc::new(execution::default_llm),
        }
    }

    /// Create with a custom session store.
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.sessions = store;
        self
    }

    /// Replace the LLM factory used to back agent execution (`/run`,
    /// `/run_sse`). Useful for injecting a mock LLM in tests or a custom
    /// provider when embedding the server.
    pub fn with_llm_factory(mut self, factory: LlmFactory) -> Self {
        self.llm_factory = factory;
        self
    }

    /// Record a completed evaluation run summary.
    pub fn record_eval_result(&self, summary: EvalResultSummary) {
        self.eval_results.write().push(summary);
    }
}
