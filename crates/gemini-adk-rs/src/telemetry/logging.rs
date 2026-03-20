//! Structured logging helpers for agent lifecycle.
//!
//! All log events carry consistent fields for correlation.
//! Feature-gated behind `tracing-support`.

/// Log that an agent has started.
#[cfg(feature = "tracing-support")]
pub fn log_agent_started(agent_name: &str, tool_count: usize) {
    tracing::info!(
        agent_name = agent_name,
        tool_count = tool_count,
        "Agent started"
    );
}

/// Log that an agent has completed.
#[cfg(feature = "tracing-support")]
pub fn log_agent_completed(agent_name: &str, duration_ms: f64) {
    tracing::info!(
        agent_name = agent_name,
        duration_ms = duration_ms,
        "Agent completed"
    );
}

/// Log a tool dispatch.
#[cfg(feature = "tracing-support")]
pub fn log_tool_dispatch(agent_name: &str, tool_name: &str, tool_class: &str) {
    tracing::info!(
        agent_name = agent_name,
        tool_name = tool_name,
        tool_class = tool_class,
        "Tool dispatched"
    );
}

/// Log a tool result.
#[cfg(feature = "tracing-support")]
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
#[cfg(feature = "tracing-support")]
pub fn log_agent_transfer(from: &str, to: &str) {
    tracing::info!(from = from, to = to, "Agent transfer");
}

/// Log an agent error (warn level).
#[cfg(feature = "tracing-support")]
pub fn log_agent_error(agent_name: &str, error: &str) {
    tracing::warn!(agent_name = agent_name, error = error, "Agent error");
}

/// Log an agent-as-tool dispatch.
#[cfg(feature = "tracing-support")]
pub fn log_agent_tool_dispatch(parent: &str, child: &str) {
    tracing::info!(parent = parent, child = child, "Agent tool dispatch");
}

/// Log event loop lag (warn level).
#[cfg(feature = "tracing-support")]
pub fn log_event_loop_lag(agent_name: &str, skipped: u64) {
    tracing::warn!(
        agent_name = agent_name,
        skipped = skipped,
        "Event loop lag — skipped events"
    );
}

// No-op stubs when tracing is disabled.
/// Log that an agent has started (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_agent_started(_: &str, _: usize) {}
/// Log that an agent completed (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_agent_completed(_: &str, _: f64) {}
/// Log a tool dispatch (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_tool_dispatch(_: &str, _: &str, _: &str) {}
/// Log a tool result (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_tool_result(_: &str, _: &str, _: bool, _: f64) {}
/// Log an agent transfer (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_agent_transfer(_: &str, _: &str) {}
/// Log an agent error (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_agent_error(_: &str, _: &str) {}
/// Log an agent-as-tool dispatch (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_agent_tool_dispatch(_: &str, _: &str) {}
/// Log event loop lag (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_event_loop_lag(_: &str, _: u64) {}

/// Log an LLM call.
#[cfg(feature = "tracing-support")]
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
#[cfg(feature = "tracing-support")]
pub fn log_phase_transition(from: &str, to: &str) {
    tracing::info!(from = from, to = to, "Phase transition");
}

/// Log an extraction result.
#[cfg(feature = "tracing-support")]
pub fn log_extraction_result(extractor: &str, success: bool, duration_ms: f64) {
    tracing::info!(
        extractor = extractor,
        success = success,
        duration_ms = duration_ms,
        "Extraction completed"
    );
}

/// Log session persistence.
#[cfg(feature = "tracing-support")]
pub fn log_session_persisted(session_id: &str, backend: &str, duration_ms: f64) {
    tracing::info!(
        session_id = session_id,
        backend = backend,
        duration_ms = duration_ms,
        "Session persisted"
    );
}

// No-op stubs for new logging functions when tracing is disabled.
/// Log an LLM call (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_llm_call(_: &str, _: &str, _: u32, _: u32, _: f64) {}
/// Log a phase transition (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_phase_transition(_: &str, _: &str) {}
/// Log an extraction result (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_extraction_result(_: &str, _: bool, _: f64) {}
/// Log session persistence (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn log_session_persisted(_: &str, _: &str, _: f64) {}
