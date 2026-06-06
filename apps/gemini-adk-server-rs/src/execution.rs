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

use gemini_adk_rs::text::TextAgent;
use gemini_adk_rs::{AgentError, GeminiLlm, GeminiLlmParams, LlmTextAgent, State};

use crate::agents::AgentEntry;
use crate::types::AgentEvent;

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
    let llm = GeminiLlm::new(GeminiLlmParams {
        model: entry.model.clone(),
        ..Default::default()
    });

    let mut agent = LlmTextAgent::new(entry.name.clone(), Arc::new(llm));
    if let Some(instruction) = &entry.instruction {
        agent = agent.instruction(instruction.clone());
    }

    Arc::new(agent)
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
