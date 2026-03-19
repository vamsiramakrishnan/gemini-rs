//! OpenTelemetry span definitions for agent lifecycle operations.
//!
//! Each span carries contextual fields for agent-level correlation.
//! Feature-gated behind `tracing-support`.

/// Create a span for an agent's run lifecycle.
#[cfg(feature = "tracing-support")]
pub fn agent_run_span(agent_name: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.run",
        agent_name = agent_name,
        session_id = session_id,
    )
}

/// Create a span for an agent transfer.
#[cfg(feature = "tracing-support")]
pub fn agent_transfer_span(from: &str, to: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.transfer",
        from = from,
        to = to,
        session_id = session_id,
    )
}

/// Create a span for tool dispatch within an agent.
#[cfg(feature = "tracing-support")]
pub fn tool_dispatch_span(tool_name: &str, tool_class: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.tool_dispatch",
        tool_name = tool_name,
        tool_class = tool_class,
        session_id = session_id,
    )
}

/// Create a span for an agent-as-tool invocation.
#[cfg(feature = "tracing-support")]
pub fn agent_tool_span(agent_name: &str, parent_agent: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.agent_tool",
        agent_name = agent_name,
        parent_agent = parent_agent,
    )
}

/// Create a span for the top-level runner.
#[cfg(feature = "tracing-support")]
pub fn runner_span(root_agent: &str) -> tracing::Span {
    tracing::info_span!("gemini.agent.runner", root_agent = root_agent)
}

// No-op stubs when tracing is disabled — these compile to nothing.
/// Create a span for an agent's run lifecycle (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn agent_run_span(_: &str, _: &str) {}
/// Create a span for an agent transfer (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn agent_transfer_span(_: &str, _: &str, _: &str) {}
/// Create a span for tool dispatch (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn tool_dispatch_span(_: &str, _: &str, _: &str) {}
/// Create a span for agent-as-tool invocation (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn agent_tool_span(_: &str, _: &str) {}
/// Create a span for the top-level runner (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn runner_span(_: &str) {}

/// Create a span for an LLM generate call.
#[cfg(feature = "tracing-support")]
pub fn call_llm_span(model_id: &str, agent_name: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.call_llm",
        model_id = model_id,
        agent_name = agent_name,
        session_id = session_id,
    )
}

/// Create a span for a full invocation (top-level).
#[cfg(feature = "tracing-support")]
pub fn invocation_span(invocation_id: &str, root_agent: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.invocation",
        invocation_id = invocation_id,
        root_agent = root_agent,
    )
}

/// Create a span for a phase transition.
#[cfg(feature = "tracing-support")]
pub fn phase_transition_span(from_phase: &str, to_phase: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.phase_transition",
        from_phase = from_phase,
        to_phase = to_phase,
        session_id = session_id,
    )
}

/// Create a span for an extraction operation.
#[cfg(feature = "tracing-support")]
pub fn extraction_span(extractor_name: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.extraction",
        extractor_name = extractor_name,
        session_id = session_id,
    )
}

// No-op stubs for new spans when tracing is disabled.
/// Create a span for an LLM generate call (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn call_llm_span(_: &str, _: &str, _: &str) {}
/// Create a span for a full invocation (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn invocation_span(_: &str, _: &str) {}
/// Create a span for a phase transition (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn phase_transition_span(_: &str, _: &str, _: &str) {}
/// Create a span for an extraction operation (no-op without `tracing-support` feature).
#[cfg(not(feature = "tracing-support"))]
pub fn extraction_span(_: &str, _: &str) {}
