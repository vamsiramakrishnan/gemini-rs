//! Structured logging helpers.
//!
//! All log events carry consistent fields (session_id, phase) for correlation.
//! Feature-gated behind `tracing-support`.

/// Log a session lifecycle event.
#[cfg(feature = "tracing-support")]
pub fn log_session_event(session_id: &str, phase: &str, event: &str) {
    tracing::info!(
        session_id = session_id,
        phase = phase,
        event = event,
        "Session event"
    );
}

/// Log a tool call dispatch.
#[cfg(feature = "tracing-support")]
pub fn log_tool_call(session_id: &str, function_name: &str, call_count: usize) {
    tracing::info!(
        session_id = session_id,
        event = "tool_call_received",
        function_name = function_name,
        function_count = call_count,
        "Model requested function calls"
    );
}

/// Log a WebSocket error (warn level).
#[cfg(feature = "tracing-support")]
pub fn log_ws_error(session_id: &str, error: &str) {
    tracing::warn!(
        session_id = session_id,
        event = "websocket_error",
        error = error,
        "WebSocket error"
    );
}

/// Log a jitter buffer underrun (warn level).
#[cfg(feature = "tracing-support")]
pub fn log_jitter_underrun(session_id: &str, depth_ms: f64) {
    tracing::warn!(
        session_id = session_id,
        event = "jitter_underrun",
        depth_ms = depth_ms,
        "Jitter buffer underrun"
    );
}

/// Log a reconnection attempt (warn level).
#[cfg(feature = "tracing-support")]
pub fn log_reconnection(session_id: &str, attempt: u32, delay_ms: u64) {
    tracing::warn!(
        session_id = session_id,
        event = "reconnection",
        attempt = attempt,
        delay_ms = delay_ms,
        "Reconnection attempt"
    );
}

/// Log VAD state change (debug level).
#[cfg(feature = "tracing-support")]
pub fn log_vad_event(session_id: &str, event: &str) {
    tracing::debug!(
        session_id = session_id,
        event = "vad_state_change",
        vad_event = event,
        "VAD event"
    );
}

/// Log an HTTP request (info level).
#[cfg(feature = "tracing-support")]
pub fn log_http_request(method: &str, url: &str) {
    tracing::info!(
        event = "http_request",
        http.method = method,
        http.url = url,
        "HTTP request"
    );
}

/// Log an HTTP response (info level).
#[cfg(feature = "tracing-support")]
pub fn log_http_response(status: u16, duration_ms: f64) {
    tracing::info!(
        event = "http_response",
        http.status = status,
        duration_ms = duration_ms,
        "HTTP response"
    );
}

/// Log an HTTP retry attempt (warn level).
#[cfg(feature = "tracing-support")]
pub fn log_http_retry(url: &str, attempt: u32, delay_ms: u64) {
    tracing::warn!(
        event = "http_retry",
        http.url = url,
        attempt = attempt,
        delay_ms = delay_ms,
        "HTTP retry"
    );
}

// No-op stubs when tracing is disabled.
#[cfg(not(feature = "tracing-support"))]
pub fn log_session_event(_: &str, _: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn log_tool_call(_: &str, _: &str, _: usize) {}
#[cfg(not(feature = "tracing-support"))]
pub fn log_ws_error(_: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn log_jitter_underrun(_: &str, _: f64) {}
#[cfg(not(feature = "tracing-support"))]
pub fn log_reconnection(_: &str, _: u32, _: u64) {}
#[cfg(not(feature = "tracing-support"))]
pub fn log_vad_event(_: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn log_http_request(_: &str, _: &str) {}
#[cfg(not(feature = "tracing-support"))]
pub fn log_http_response(_: u16, _: f64) {}
#[cfg(not(feature = "tracing-support"))]
pub fn log_http_retry(_: &str, _: u32, _: u64) {}
