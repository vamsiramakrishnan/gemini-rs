//! Prometheus metric definitions — counters, histograms, gauges.
//!
//! Feature-gated behind `metrics`. When disabled, all functions compile to no-ops.

/// Record a new session connection.
#[cfg(feature = "metrics")]
pub fn record_session_connected() {
    metrics::counter!("rs_genai_connections_total").increment(1);
    metrics::gauge!("rs_genai_sessions_active").increment(1.0);
}

/// Record a session disconnection.
#[cfg(feature = "metrics")]
pub fn record_session_disconnected() {
    metrics::gauge!("rs_genai_sessions_active").decrement(1.0);
}

/// Record audio send latency.
#[cfg(feature = "metrics")]
pub fn record_audio_latency(latency_ms: f64) {
    metrics::histogram!("rs_genai_audio_latency_ms").record(latency_ms);
}

/// Record time from end-of-speech to first model response.
#[cfg(feature = "metrics")]
pub fn record_response_latency(latency_ms: f64) {
    metrics::histogram!("rs_genai_response_latency_ms").record(latency_ms);
}

/// Record current jitter buffer depth.
#[cfg(feature = "metrics")]
pub fn record_jitter_depth(depth_ms: f64) {
    metrics::gauge!("rs_genai_jitter_buffer_depth_ms").set(depth_ms);
}

/// Record a jitter buffer underrun.
#[cfg(feature = "metrics")]
pub fn record_jitter_underrun() {
    metrics::counter!("rs_genai_jitter_underruns_total").increment(1);
}

/// Record a tool call execution.
#[cfg(feature = "metrics")]
pub fn record_tool_call(function_name: &str, duration_ms: f64) {
    metrics::counter!("rs_genai_tool_calls_total", "function" => function_name.to_string())
        .increment(1);
    metrics::histogram!("rs_genai_tool_call_duration_ms", "function" => function_name.to_string())
        .record(duration_ms);
}

/// Record a VAD event.
#[cfg(feature = "metrics")]
pub fn record_vad_event(event: &str) {
    metrics::counter!("rs_genai_vad_events_total", "event" => event.to_string()).increment(1);
}

/// Record a reconnection attempt.
#[cfg(feature = "metrics")]
pub fn record_reconnection() {
    metrics::counter!("rs_genai_reconnections_total").increment(1);
}

/// Record WebSocket bytes sent.
#[cfg(feature = "metrics")]
pub fn record_ws_bytes_sent(bytes: u64) {
    metrics::counter!("rs_genai_ws_bytes_sent_total").increment(bytes);
}

/// Record WebSocket bytes received.
#[cfg(feature = "metrics")]
pub fn record_ws_bytes_received(bytes: u64) {
    metrics::counter!("rs_genai_ws_bytes_received_total").increment(bytes);
}

// No-op stubs when metrics feature is disabled.
#[cfg(not(feature = "metrics"))]
pub fn record_session_connected() {}
#[cfg(not(feature = "metrics"))]
pub fn record_session_disconnected() {}
#[cfg(not(feature = "metrics"))]
pub fn record_audio_latency(_: f64) {}
#[cfg(not(feature = "metrics"))]
pub fn record_response_latency(_: f64) {}
#[cfg(not(feature = "metrics"))]
pub fn record_jitter_depth(_: f64) {}
#[cfg(not(feature = "metrics"))]
pub fn record_jitter_underrun() {}
#[cfg(not(feature = "metrics"))]
pub fn record_tool_call(_: &str, _: f64) {}
#[cfg(not(feature = "metrics"))]
pub fn record_vad_event(_: &str) {}
#[cfg(not(feature = "metrics"))]
pub fn record_reconnection() {}
#[cfg(not(feature = "metrics"))]
pub fn record_ws_bytes_sent(_: u64) {}
#[cfg(not(feature = "metrics"))]
pub fn record_ws_bytes_received(_: u64) {}
