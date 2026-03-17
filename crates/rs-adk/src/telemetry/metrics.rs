//! Prometheus metric definitions for agent lifecycle — counters, histograms.
//!
//! Feature-gated behind `metrics`. When disabled, all functions compile to no-ops.

/// Record that an agent has started.
#[cfg(feature = "metrics")]
pub fn record_agent_started(agent_name: &str) {
    metrics::counter!("gemini_agent_started_total", "agent" => agent_name.to_string()).increment(1);
}

/// Record that an agent has completed, with its duration.
#[cfg(feature = "metrics")]
pub fn record_agent_completed(agent_name: &str, duration_ms: f64) {
    metrics::counter!("gemini_agent_completed_total", "agent" => agent_name.to_string())
        .increment(1);
    metrics::histogram!("gemini_agent_duration_ms", "agent" => agent_name.to_string())
        .record(duration_ms);
}

/// Record an agent error.
#[cfg(feature = "metrics")]
pub fn record_agent_error(agent_name: &str, error_type: &str) {
    metrics::counter!("gemini_agent_errors_total", "agent" => agent_name.to_string(), "error_type" => error_type.to_string())
        .increment(1);
}

/// Record that a tool was dispatched by an agent.
#[cfg(feature = "metrics")]
pub fn record_agent_tool_dispatched(agent_name: &str, tool_name: &str) {
    metrics::counter!("gemini_agent_tool_dispatched_total", "agent" => agent_name.to_string(), "tool" => tool_name.to_string())
        .increment(1);
}

/// Record tool execution duration for an agent.
#[cfg(feature = "metrics")]
pub fn record_agent_tool_duration(agent_name: &str, tool_name: &str, duration_ms: f64) {
    metrics::histogram!("gemini_agent_tool_duration_ms", "agent" => agent_name.to_string(), "tool" => tool_name.to_string())
        .record(duration_ms);
}

/// Record an agent transfer from one agent to another.
#[cfg(feature = "metrics")]
pub fn record_agent_transfer(from: &str, to: &str) {
    metrics::counter!("gemini_agent_transfers_total", "from" => from.to_string(), "to" => to.to_string())
        .increment(1);
}

/// Record an agent-as-tool dispatch with duration.
#[cfg(feature = "metrics")]
pub fn record_agent_tool_dispatch(parent_agent: &str, child_agent: &str, duration_ms: f64) {
    metrics::counter!("gemini_agent_tool_dispatch_total", "parent" => parent_agent.to_string(), "child" => child_agent.to_string())
        .increment(1);
    metrics::histogram!("gemini_agent_tool_dispatch_duration_ms", "parent" => parent_agent.to_string(), "child" => child_agent.to_string())
        .record(duration_ms);
}

/// Record event loop lag (skipped events).
#[cfg(feature = "metrics")]
pub fn record_event_loop_lag(agent_name: &str, skipped: u64) {
    metrics::counter!("gemini_agent_event_loop_lag_total", "agent" => agent_name.to_string())
        .increment(skipped);
}

// No-op stubs when metrics feature is disabled.
/// Record that an agent has started (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_agent_started(_: &str) {}
/// Record that an agent completed (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_agent_completed(_: &str, _: f64) {}
/// Record an agent error (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_agent_error(_: &str, _: &str) {}
/// Record a tool dispatch (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_agent_tool_dispatched(_: &str, _: &str) {}
/// Record tool execution duration (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_agent_tool_duration(_: &str, _: &str, _: f64) {}
/// Record an agent transfer (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_agent_transfer(_: &str, _: &str) {}
/// Record agent-as-tool dispatch (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_agent_tool_dispatch(_: &str, _: &str, _: f64) {}
/// Record event loop lag (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_event_loop_lag(_: &str, _: u64) {}

/// Record LLM call with duration and token counts.
#[cfg(feature = "metrics")]
pub fn record_llm_call(
    model_id: &str,
    agent_name: &str,
    duration_ms: f64,
    prompt_tokens: u32,
    completion_tokens: u32,
) {
    metrics::counter!("gemini_llm_calls_total", "model" => model_id.to_string(), "agent" => agent_name.to_string())
        .increment(1);
    metrics::histogram!("gemini_llm_call_duration_ms", "model" => model_id.to_string(), "agent" => agent_name.to_string())
        .record(duration_ms);
    metrics::counter!("gemini_llm_prompt_tokens_total", "model" => model_id.to_string(), "agent" => agent_name.to_string())
        .increment(prompt_tokens as u64);
    metrics::counter!("gemini_llm_completion_tokens_total", "model" => model_id.to_string(), "agent" => agent_name.to_string())
        .increment(completion_tokens as u64);
}

/// Record total token usage.
#[cfg(feature = "metrics")]
pub fn record_token_usage(model_id: &str, prompt_tokens: u32, completion_tokens: u32) {
    metrics::counter!("gemini_token_usage_prompt_total", "model" => model_id.to_string())
        .increment(prompt_tokens as u64);
    metrics::counter!("gemini_token_usage_completion_total", "model" => model_id.to_string())
        .increment(completion_tokens as u64);
}

/// Record a phase transition.
#[cfg(feature = "metrics")]
pub fn record_phase_transition(from: &str, to: &str) {
    metrics::counter!("gemini_phase_transitions_total", "from" => from.to_string(), "to" => to.to_string())
        .increment(1);
}

/// Record extraction duration.
#[cfg(feature = "metrics")]
pub fn record_extraction_duration(extractor: &str, duration_ms: f64) {
    metrics::histogram!("gemini_extraction_duration_ms", "extractor" => extractor.to_string())
        .record(duration_ms);
}

/// Record session persistence duration.
#[cfg(feature = "metrics")]
pub fn record_persistence_duration(backend: &str, duration_ms: f64) {
    metrics::histogram!("gemini_persistence_duration_ms", "backend" => backend.to_string())
        .record(duration_ms);
}

// No-op stubs for new metrics when feature is disabled.
/// Record LLM call (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_llm_call(_: &str, _: &str, _: f64, _: u32, _: u32) {}
/// Record total token usage (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_token_usage(_: &str, _: u32, _: u32) {}
/// Record a phase transition (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_phase_transition(_: &str, _: &str) {}
/// Record extraction duration (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_extraction_duration(_: &str, _: f64) {}
/// Record session persistence duration (no-op without `metrics` feature).
#[cfg(not(feature = "metrics"))]
pub fn record_persistence_duration(_: &str, _: f64) {}
