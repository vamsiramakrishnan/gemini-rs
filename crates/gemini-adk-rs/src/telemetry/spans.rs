//! OpenTelemetry span definitions for agent lifecycle operations.
//!
//! Each span carries contextual fields for agent-level correlation.

/// Create a span for an agent's run lifecycle.
pub fn agent_run_span(agent_name: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.run",
        agent_name = agent_name,
        session_id = session_id,
    )
}

/// Create a span for an agent transfer.
pub fn agent_transfer_span(from: &str, to: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.transfer",
        from = from,
        to = to,
        session_id = session_id,
    )
}

/// Create a span for tool dispatch within an agent.
pub fn tool_dispatch_span(tool_name: &str, tool_class: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.tool_dispatch",
        tool_name = tool_name,
        tool_class = tool_class,
        session_id = session_id,
    )
}

/// Create a span for an agent-as-tool invocation.
pub fn agent_tool_span(agent_name: &str, parent_agent: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.agent_tool",
        agent_name = agent_name,
        parent_agent = parent_agent,
    )
}

/// Create a span for the top-level runner.
pub fn runner_span(root_agent: &str) -> tracing::Span {
    tracing::info_span!("gemini.agent.runner", root_agent = root_agent)
}

/// Create a span for an LLM generate call.
pub fn call_llm_span(model_id: &str, agent_name: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.call_llm",
        model_id = model_id,
        agent_name = agent_name,
        session_id = session_id,
    )
}

/// Create a span for a full invocation (top-level).
pub fn invocation_span(invocation_id: &str, root_agent: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.invocation",
        invocation_id = invocation_id,
        root_agent = root_agent,
    )
}

/// Create a span for a phase transition.
pub fn phase_transition_span(from_phase: &str, to_phase: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.phase_transition",
        from_phase = from_phase,
        to_phase = to_phase,
        session_id = session_id,
    )
}

/// Create a span for an extraction operation.
pub fn extraction_span(extractor_name: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "gemini.agent.extraction",
        extractor_name = extractor_name,
        session_id = session_id,
    )
}
