//! Real agent execution — mirrors ADK Python's `Runner.run_async` for the REST surface.
//!
//! ADK Python's `Runner` drives an agent over a session: it appends the user's
//! message, runs the agent to completion, and yields the events the agent
//! produced. This module provides the Rust equivalent for the text (request /
//! response) execution path used by the `POST /run` endpoint.
//!
//! Live (WebSocket) execution is a separate concern handled by the L1
//! [`gemini_adk_rs::Runner`], which is session-oriented and requires a connected
//! Gemini Live socket — so it is not used for one-shot REST calls. Here we use
//! the L1 [`gemini_adk_rs::LlmTextAgent`] (generate → tool-dispatch → loop),
//! the request/response counterpart of the same runtime.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gemini_adk_rs::gemini_genai_rs::prelude::FunctionCall;
use gemini_adk_rs::text::TextAgent;
use gemini_adk_rs::{
    AgentError, BaseLlm, GeminiLlm, GeminiLlmParams, LlmTextAgent, Middleware, State, ToolError,
};

use crate::agents::AgentEntry;
use crate::types::AgentEvent;

/// Factory that resolves the LLM backing an agent entry.
///
/// The default ([`default_llm`]) constructs a [`GeminiLlm`] with auth resolved
/// from the environment. Tests (and embedders) can swap it via
/// [`crate::ServerState::with_llm_factory`] to inject a mock LLM.
pub type LlmFactory = Arc<dyn Fn(&AgentEntry) -> Arc<dyn BaseLlm> + Send + Sync>;

/// Default LLM resolution: a [`GeminiLlm`] for the entry's model (falling back
/// to the SDK default when unset), with auth from the environment.
pub fn default_llm(entry: &AgentEntry) -> Arc<dyn BaseLlm> {
    Arc::new(GeminiLlm::new(GeminiLlmParams {
        model: entry.model.clone(),
        ..Default::default()
    }))
}

/// Outcome of running an agent over a single turn.
pub struct RunOutcome {
    /// The agent's final text response.
    pub response: String,
    /// Structured events produced during the run (mirrors ADK `Event`s).
    pub events: Vec<AgentEvent>,
}

/// Build a runnable text agent from a registry [`AgentEntry`].
///
/// Constructs a [`GeminiLlm`] from the entry's model (falling back to the SDK
/// default when unset) and wraps it in an [`LlmTextAgent`] carrying the entry's
/// instruction. Authentication (API key / Vertex project) is resolved from the
/// environment by `GeminiLlm`, matching the rest of the SDK.
///
/// Note: builtin tools declared on the entry (e.g. `google_search`) are
/// wire-level tools fixed at Live-session setup; they are not executable from
/// the request/response text path, so they are not attached here. The agent
/// still runs a real LLM generation.
pub fn build_text_agent(entry: &AgentEntry) -> Arc<dyn TextAgent> {
    build_text_agent_with(entry, default_llm(entry), None)
}

/// Build a runnable text agent with an explicit LLM and optional middleware.
///
/// Used by the SSE endpoint to attach a [`ChannelEvents`] middleware that
/// forwards real lifecycle events to the stream, and by tests to inject a mock
/// LLM. See [`build_text_agent`] for the default path.
pub fn build_text_agent_with(
    entry: &AgentEntry,
    llm: Arc<dyn BaseLlm>,
    middleware: Option<Arc<dyn Middleware>>,
) -> Arc<dyn TextAgent> {
    let mut agent = LlmTextAgent::new(entry.name.clone(), llm);
    if let Some(instruction) = &entry.instruction {
        agent = agent.instruction(instruction.clone());
    }
    if let Some(mw) = middleware {
        agent = agent.add_middleware(mw);
    }

    Arc::new(agent)
}

/// Middleware that forwards real agent lifecycle events into a channel.
///
/// Backs the `POST /run_sse` endpoint: every event it emits corresponds to
/// something the agent runtime actually did (agent start/completion, tool
/// dispatch). It never fabricates output — token-level deltas are not emitted
/// because [`BaseLlm`] has no streaming generation API.
pub struct ChannelEvents {
    tx: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
}

impl ChannelEvents {
    /// Create a forwarder that sends event payloads into `tx`.
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<serde_json::Value>) -> Self {
        Self { tx }
    }

    fn send(&self, payload: serde_json::Value) {
        // The receiver may have disconnected (client closed the stream) —
        // dropping the event is the correct behavior then.
        let _ = self.tx.send(payload);
    }
}

#[async_trait]
impl Middleware for ChannelEvents {
    fn name(&self) -> &str {
        "channel-events"
    }

    async fn on_event(
        &self,
        event: &gemini_adk_rs::AgentEvent,
    ) -> Result<(), gemini_adk_rs::AgentError> {
        use gemini_adk_rs::AgentEvent as E;
        let payload = match event {
            E::AgentStarted { name } => {
                serde_json::json!({"type": "agent_started", "agent": name})
            }
            E::AgentCompleted { name } => {
                serde_json::json!({"type": "agent_completed", "agent": name})
            }
            E::Timeout => serde_json::json!({"type": "timeout"}),
            _ => return Ok(()),
        };
        self.send(payload);
        Ok(())
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        self.send(serde_json::json!({
            "type": "tool_call_started",
            "tool": call.name,
            "args": call.args,
        }));
        Ok(())
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        self.send(serde_json::json!({
            "type": "tool_call_completed",
            "tool": call.name,
            "result": result,
        }));
        Ok(())
    }

    async fn on_tool_error(&self, call: &FunctionCall, err: &ToolError) -> Result<(), AgentError> {
        self.send(serde_json::json!({
            "type": "tool_call_failed",
            "tool": call.name,
            "error": err.to_string(),
        }));
        Ok(())
    }
}

/// Run an agent to completion over one turn, mirroring ADK `Runner.run_async`.
///
/// 1. Seeds a fresh [`State`] with the prior session state (so multi-turn
///    context carries forward) and the new user `message` under `"input"`.
/// 2. Runs the agent via [`TextAgent::run`], driving the generate → tool →
///    loop cycle to completion.
/// 3. Collects the produced output as structured [`AgentEvent`]s.
///
/// The caller persists the user message and the returned events into the
/// session store.
pub async fn run_agent_turn(
    agent: &Arc<dyn TextAgent>,
    message: &str,
    prior_state: &HashMap<String, serde_json::Value>,
) -> Result<RunOutcome, AgentError> {
    let state = State::new();

    // Rehydrate prior session state so the agent sees accumulated context.
    for (key, value) in prior_state {
        let _ = state.set(key.clone(), value.clone());
    }
    let _ = state.set("input", message);

    let response = agent.run(&state).await?;

    let events = vec![AgentEvent {
        event_type: "text".into(),
        data: serde_json::json!({ "role": "model", "content": response }),
    }];

    Ok(RunOutcome { response, events })
}
