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
#[cfg(not(feature = "tracing-support"))]
pub fn agent_run_span(_: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn agent_transfer_span(_: &str, _: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn tool_dispatch_span(_: &str, _: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn agent_tool_span(_: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn runner_span(_: &str) {}
