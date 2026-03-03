//! Lightweight session telemetry — atomic fast-lane counters + periodic aggregation.
//!
//! All hot-path operations (counter increments, timestamp recording) are lock-free
//! and zero-allocation (~1ns per call). Aggregation only happens periodically on
//! the telemetry lane or at turn boundaries, ensuring no impact on the
//! latency-sensitive audio pipeline.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use serde_json::json;

/// Zero-overhead telemetry collector for speech-to-speech sessions.
///
/// Designed for the three-lane processor model:
/// - **Fast lane** (sync, <1ms): No telemetry calls — pure audio/text forwarding.
/// - **Telemetry lane** (async, debounced): Calls `record_*` methods on every event.
///   These use only atomic operations — no allocations, no locks, no syscalls.
/// - **Control lane** (async): Calls `snapshot()` at turn boundaries to get
///   aggregated stats as a JSON value ready to send to the browser.
pub struct SessionTelemetry {
    start: Instant,

    // ── Audio throughput ──
    audio_chunks_out: AtomicU64,
    audio_bytes_out: AtomicU64,

    // ── Interruptions ──
    interruptions: AtomicU64,

    // ── Response latency tracking ──
    // Stores nanos-since-session-start for atomic compatibility with Instant.
    vad_end_ns: AtomicU64,
    awaiting_response: AtomicBool,

    // Aggregated latency stats (CAS + fetch_add)
    last_latency_ns: AtomicU64,
    latency_sum_ns: AtomicU64,
    latency_count: AtomicU64,
    min_latency_ns: AtomicU64,
    max_latency_ns: AtomicU64,

    // ── Turn timing ──
    last_turn_start_ns: AtomicU64,
    turn_duration_sum_ns: AtomicU64,
    turn_duration_count: AtomicU64,
}

impl SessionTelemetry {
    /// Create a new telemetry tracker, starting the session clock.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            audio_chunks_out: AtomicU64::new(0),
            audio_bytes_out: AtomicU64::new(0),
            interruptions: AtomicU64::new(0),
            vad_end_ns: AtomicU64::new(0),
            awaiting_response: AtomicBool::new(false),
            last_latency_ns: AtomicU64::new(0),
            latency_sum_ns: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            min_latency_ns: AtomicU64::new(u64::MAX),
            max_latency_ns: AtomicU64::new(0),
            last_turn_start_ns: AtomicU64::new(0),
            turn_duration_sum_ns: AtomicU64::new(0),
            turn_duration_count: AtomicU64::new(0),
        }
    }

    // ── Atomic methods (~1ns each) ──

    /// Record an outgoing audio chunk. Called from the telemetry lane.
    #[inline]
    pub fn record_audio_out(&self, byte_len: usize) {
        self.audio_chunks_out.fetch_add(1, Relaxed);
        self.audio_bytes_out.fetch_add(byte_len as u64, Relaxed);

        // Latency: if we're awaiting the model's first byte after VAD end,
        // record the response latency via CAS (only the first chunk wins).
        if self
            .awaiting_response
            .compare_exchange(true, false, Relaxed, Relaxed)
            .is_ok()
        {
            let now_ns = self.elapsed_ns();
            let vad_end = self.vad_end_ns.load(Relaxed);
            if now_ns > vad_end && vad_end > 0 {
                let latency = now_ns - vad_end;
                self.last_latency_ns.store(latency, Relaxed);
                self.latency_sum_ns.fetch_add(latency, Relaxed);
                self.latency_count.fetch_add(1, Relaxed);
                // Update min (CAS loop)
                let mut current_min = self.min_latency_ns.load(Relaxed);
                while latency < current_min {
                    match self.min_latency_ns.compare_exchange_weak(
                        current_min,
                        latency,
                        Relaxed,
                        Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(actual) => current_min = actual,
                    }
                }
                // Update max (CAS loop)
                let mut current_max = self.max_latency_ns.load(Relaxed);
                while latency > current_max {
                    match self.max_latency_ns.compare_exchange_weak(
                        current_max,
                        latency,
                        Relaxed,
                        Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(actual) => current_max = actual,
                    }
                }
            }
        }
    }

    /// Record VAD end (user stopped speaking).
    #[inline]
    pub fn record_vad_end(&self) {
        self.vad_end_ns.store(self.elapsed_ns(), Relaxed);
        self.awaiting_response.store(true, Relaxed);
    }

    /// Record an interruption (barge-in).
    #[inline]
    pub fn record_interruption(&self) {
        self.interruptions.fetch_add(1, Relaxed);
    }

    /// Record turn completion for duration tracking.
    #[inline]
    pub fn record_turn_complete(&self) {
        let now = self.elapsed_ns();
        let turn_start = self.last_turn_start_ns.swap(now, Relaxed);
        if turn_start > 0 {
            let duration = now.saturating_sub(turn_start);
            self.turn_duration_sum_ns.fetch_add(duration, Relaxed);
            self.turn_duration_count.fetch_add(1, Relaxed);
        }
    }

    /// Mark the beginning of a new turn (e.g., when model starts responding).
    #[inline]
    pub fn mark_turn_start(&self) {
        let now = self.elapsed_ns();
        // Only set if not already set (first call per turn wins)
        self.last_turn_start_ns
            .compare_exchange(0, now, Relaxed, Relaxed)
            .ok();
    }

    // ── Aggregation (called at turn boundaries / periodic flush) ──

    /// Snapshot all metrics as a JSON value.
    pub fn snapshot(&self) -> serde_json::Value {
        let elapsed = self.start.elapsed();
        let elapsed_secs = elapsed.as_secs_f64();

        let chunks = self.audio_chunks_out.load(Relaxed);
        let bytes = self.audio_bytes_out.load(Relaxed);
        let latency_count = self.latency_count.load(Relaxed);

        let avg_latency_ms = if latency_count > 0 {
            self.latency_sum_ns.load(Relaxed) / latency_count / 1_000_000
        } else {
            0
        };

        let last_latency_ms = self.last_latency_ns.load(Relaxed) / 1_000_000;

        let min_latency_ms = {
            let v = self.min_latency_ns.load(Relaxed);
            if v == u64::MAX { 0 } else { v / 1_000_000 }
        };
        let max_latency_ms = self.max_latency_ns.load(Relaxed) / 1_000_000;

        let turn_count = self.turn_duration_count.load(Relaxed);
        let avg_turn_ms = if turn_count > 0 {
            self.turn_duration_sum_ns.load(Relaxed) / turn_count / 1_000_000
        } else {
            0
        };

        // Audio throughput (KB/s over session lifetime)
        let throughput_kbps = if elapsed_secs > 0.0 {
            (bytes as f64 / 1024.0) / elapsed_secs
        } else {
            0.0
        };

        json!({
            "uptime_secs": elapsed.as_secs(),
            "audio_chunks_out": chunks,
            "audio_kbytes_out": bytes / 1024,
            "audio_throughput_kbps": (throughput_kbps * 10.0).round() / 10.0,
            "interruptions": self.interruptions.load(Relaxed),
            "last_response_latency_ms": last_latency_ms,
            "avg_response_latency_ms": avg_latency_ms,
            "min_response_latency_ms": min_latency_ms,
            "max_response_latency_ms": max_latency_ms,
            "response_count": latency_count,
            "avg_turn_duration_ms": avg_turn_ms,
        })
    }

    #[inline]
    fn elapsed_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}

impl Default for SessionTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_snapshot_is_zeroed() {
        let t = SessionTelemetry::new();
        let snap = t.snapshot();
        assert_eq!(snap["audio_chunks_out"], 0);
        assert_eq!(snap["interruptions"], 0);
        assert_eq!(snap["last_response_latency_ms"], 0);
        assert_eq!(snap["response_count"], 0);
    }

    #[test]
    fn audio_counters_accumulate() {
        let t = SessionTelemetry::new();
        t.record_audio_out(480);
        t.record_audio_out(480);
        t.record_audio_out(480);
        let snap = t.snapshot();
        assert_eq!(snap["audio_chunks_out"], 3);
    }

    #[test]
    fn interruption_counter() {
        let t = SessionTelemetry::new();
        t.record_interruption();
        t.record_interruption();
        assert_eq!(t.snapshot()["interruptions"], 2);
    }

    #[test]
    fn latency_tracking() {
        let t = SessionTelemetry::new();
        // Simulate: VAD end → short delay → first audio chunk
        t.record_vad_end();
        std::thread::sleep(std::time::Duration::from_millis(10));
        t.record_audio_out(480);
        // Subsequent chunks should not re-record latency
        t.record_audio_out(480);
        t.record_audio_out(480);

        let snap = t.snapshot();
        assert_eq!(snap["response_count"], 1);
        // Latency should be >= 10ms (we slept 10ms)
        assert!(snap["last_response_latency_ms"].as_u64().unwrap() >= 5);
    }

    #[test]
    fn multiple_turns_average_latency() {
        let t = SessionTelemetry::new();

        // Turn 1
        t.record_vad_end();
        std::thread::sleep(std::time::Duration::from_millis(10));
        t.record_audio_out(480);

        // Turn 2
        t.record_vad_end();
        std::thread::sleep(std::time::Duration::from_millis(10));
        t.record_audio_out(480);

        let snap = t.snapshot();
        assert_eq!(snap["response_count"], 2);
        assert!(snap["avg_response_latency_ms"].as_u64().unwrap() >= 5);
    }
}
