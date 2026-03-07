//! OpenTelemetry span definitions for session operations.
//!
//! Each span carries `session_id` for turn-level correlation.
//! Feature-gated behind `tracing-support`.

/// Create a span for the entire session lifecycle.
#[cfg(feature = "tracing-support")]
pub fn session_span(session_id: &str) -> tracing::Span {
    tracing::info_span!("rs_genai.session", session_id = session_id)
}

/// Create a span for WebSocket connection.
#[cfg(feature = "tracing-support")]
pub fn connect_span(url: &str) -> tracing::Span {
    tracing::info_span!("rs_genai.connect", url = url)
}

/// Create a span for the setup handshake.
#[cfg(feature = "tracing-support")]
pub fn setup_span(session_id: &str) -> tracing::Span {
    tracing::info_span!("rs_genai.setup", session_id = session_id)
}

/// Create a span for audio chunk transmission.
#[cfg(feature = "tracing-support")]
pub fn send_audio_span(chunk_size: usize, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "rs_genai.send_audio",
        chunk_size = chunk_size,
        session_id = session_id,
    )
}

/// Create a span for receiving server content.
#[cfg(feature = "tracing-support")]
pub fn receive_content_span(session_id: &str) -> tracing::Span {
    tracing::info_span!("rs_genai.receive_content", session_id = session_id)
}

/// Create a span for tool call execution.
#[cfg(feature = "tracing-support")]
pub fn tool_call_span(function_name: &str, session_id: &str) -> tracing::Span {
    tracing::info_span!(
        "rs_genai.tool_call",
        function_name = function_name,
        session_id = session_id,
    )
}

/// Create a span for tool response transmission.
#[cfg(feature = "tracing-support")]
pub fn tool_response_span(session_id: &str) -> tracing::Span {
    tracing::info_span!("rs_genai.tool_response", session_id = session_id)
}

/// Create a span for session disconnect.
#[cfg(feature = "tracing-support")]
pub fn disconnect_span(session_id: &str, reason: &str) -> tracing::Span {
    tracing::info_span!(
        "rs_genai.disconnect",
        session_id = session_id,
        reason = reason,
    )
}

/// Create a span for an HTTP REST API request.
#[cfg(feature = "tracing-support")]
pub fn http_request_span(method: &str, url: &str) -> tracing::Span {
    tracing::info_span!(
        "rs_genai.http_request",
        http.method = method,
        http.url = url,
    )
}

// No-op stubs when tracing is disabled — these compile to nothing.
#[cfg(not(feature = "tracing-support"))]
pub fn session_span(_: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn connect_span(_: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn setup_span(_: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn send_audio_span(_: usize, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn receive_content_span(_: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn tool_call_span(_: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn tool_response_span(_: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn disconnect_span(_: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn http_request_span(_: &str, _: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_span_functions_compile() {
        // All span functions should compile and not panic as no-ops.
        session_span("sess-1");
        connect_span("wss://example.com");
        setup_span("sess-1");
        send_audio_span(1024, "sess-1");
        receive_content_span("sess-1");
        tool_call_span("get_weather", "sess-1");
        tool_response_span("sess-1");
        disconnect_span("sess-1", "user requested");
        http_request_span("GET", "https://example.com");
    }

    #[test]
    fn session_span_noop() {
        session_span("test-session");
    }

    #[test]
    fn connect_span_noop() {
        connect_span("wss://example.com/ws");
    }

    #[test]
    fn setup_span_noop() {
        setup_span("test-session");
    }

    #[test]
    fn send_audio_span_noop() {
        send_audio_span(4096, "test-session");
    }

    #[test]
    fn receive_content_span_noop() {
        receive_content_span("test-session");
    }

    #[test]
    fn tool_call_span_noop() {
        tool_call_span("my_tool", "test-session");
    }

    #[test]
    fn tool_response_span_noop() {
        tool_response_span("test-session");
    }

    #[test]
    fn disconnect_span_noop() {
        disconnect_span("test-session", "shutdown");
    }

    #[test]
    fn http_request_span_noop() {
        http_request_span("POST", "https://api.example.com/v1");
    }
}
