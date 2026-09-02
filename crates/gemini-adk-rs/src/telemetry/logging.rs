//! Structured logging helpers for agent lifecycle.
//!
//! All log events carry consistent fields for correlation.

/// Log that an agent has started.
pub fn log_agent_started(agent_name: &str, tool_count: usize) {
    tracing::info!(
        agent_name = agent_name,
        tool_count = tool_count,
        "Agent started"
    );
}

/// Log that an agent has completed.
pub fn log_agent_completed(agent_name: &str, duration_ms: f64) {
    tracing::info!(
        agent_name = agent_name,
        duration_ms = duration_ms,
        "Agent completed"
    );
}

/// Log a tool dispatch.
pub fn log_tool_dispatch(agent_name: &str, tool_name: &str, tool_class: &str) {
    tracing::info!(
        agent_name = agent_name,
        tool_name = tool_name,
        tool_class = tool_class,
        "Tool dispatched"
    );
}

/// Log a tool result.
pub fn log_tool_result(agent_name: &str, tool_name: &str, success: bool, duration_ms: f64) {
    tracing::info!(
        agent_name = agent_name,
        tool_name = tool_name,
        success = success,
        duration_ms = duration_ms,
        "Tool result"
    );
}

/// Log an agent transfer.
pub fn log_agent_transfer(from: &str, to: &str) {
    tracing::info!(from = from, to = to, "Agent transfer");
}

/// Log an agent error (warn level).
pub fn log_agent_error(agent_name: &str, error: &str) {
    tracing::warn!(agent_name = agent_name, error = error, "Agent error");
}

/// Log an agent-as-tool dispatch.
pub fn log_agent_tool_dispatch(parent: &str, child: &str) {
    tracing::info!(parent = parent, child = child, "Agent tool dispatch");
}

/// Log event loop lag (warn level).
pub fn log_event_loop_lag(agent_name: &str, skipped: u64) {
    tracing::warn!(
        agent_name = agent_name,
        skipped = skipped,
        "Event loop lag — skipped events"
    );
}

/// Log an LLM call.
pub fn log_llm_call(
    model_id: &str,
    agent_name: &str,
    prompt_tokens: u32,
    completion_tokens: u32,
    duration_ms: f64,
) {
    tracing::info!(
        model_id = model_id,
        agent_name = agent_name,
        prompt_tokens = prompt_tokens,
        completion_tokens = completion_tokens,
        duration_ms = duration_ms,
        "LLM call completed"
    );
}

/// Log a phase transition.
pub fn log_phase_transition(from: &str, to: &str) {
    tracing::info!(from = from, to = to, "Phase transition");
}

/// Log an extraction result.
pub fn log_extraction_result(extractor: &str, success: bool, duration_ms: f64) {
    tracing::info!(
        extractor = extractor,
        success = success,
        duration_ms = duration_ms,
        "Extraction completed"
    );
}

/// Log session persistence.
pub fn log_session_persisted(session_id: &str, backend: &str, duration_ms: f64) {
    tracing::info!(
        session_id = session_id,
        backend = backend,
        duration_ms = duration_ms,
        "Session persisted"
    );
}
