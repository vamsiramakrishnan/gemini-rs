//! Lightweight session telemetry — atomic fast-lane counters + periodic aggregation.
//!
//! All hot-path operations (counter increments, timestamp recording) are lock-free
//! and zero-allocation (~1ns per call). Aggregation only happens periodically on
//! the telemetry lane or at turn boundaries, ensuring no impact on the
//! latency-sensitive audio pipeline.
//!
//! The number a voice product is judged on is **response latency**: the time
//! from the user's end of speech to the model's first audio byte. It is
//! recorded per turn into [`LatencyStats`] — last, min, max, mean, the p50/
//! p90/p99 of recent turns, and a fixed-bucket histogram — and read through
//! [`SessionTelemetry::latency`] or [`SessionTelemetry::snapshot`].

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

/// Upper bounds, in milliseconds, of the response-latency histogram buckets.
///
/// A final open bucket collects everything above the last bound. The bounds
/// are denser where voice products live (150–1000 ms) and coarser beyond.
pub const LATENCY_BUCKETS_MS: [u64; 17] = [
    50, 100, 150, 200, 300, 400, 500, 650, 800, 1000, 1300, 1600, 2000, 2500, 3000, 4000, 5000,
];

/// Number of most-recent samples the percentiles are computed over.
///
/// Percentiles are exact over this window (nearest-rank on a sorted copy),
/// which covers every turn of all but the longest sessions; the histogram
/// carries the full session.
pub const LATENCY_RECENT_WINDOW: usize = 256;

const BUCKET_COUNT: usize = LATENCY_BUCKETS_MS.len() + 1;

/// One bucket of the response-latency histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencyBucket {
    /// Inclusive upper bound in milliseconds; `None` for the open top bucket.
    pub upper_ms: Option<u64>,
    /// Number of turns whose latency fell in this bucket.
    pub count: u64,
}

/// Response-latency distribution for the session so far.
///
/// All durations are whole milliseconds. Every field is `0` until the first
/// turn has been measured (`count == 0`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LatencyStats {
    /// Number of measured turns.
    pub count: u64,
    /// Latency of the most recent turn.
    pub last_ms: u64,
    /// Fastest turn.
    pub min_ms: u64,
    /// Slowest turn.
    pub max_ms: u64,
    /// Mean over all measured turns.
    pub mean_ms: u64,
    /// Median of the most recent [`LATENCY_RECENT_WINDOW`] turns.
    pub p50_ms: u64,
    /// 90th percentile of the most recent [`LATENCY_RECENT_WINDOW`] turns.
    pub p90_ms: u64,
    /// 99th percentile of the most recent [`LATENCY_RECENT_WINDOW`] turns.
    pub p99_ms: u64,
    /// Full-session histogram, one entry per [`LATENCY_BUCKETS_MS`] bound plus
    /// the open top bucket.
    pub histogram: Vec<LatencyBucket>,
}

impl fmt::Display for LatencyStats {
    /// One line, fit for a log: `turns=12 last=420ms p50=380ms p90=610ms max=900ms`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.count == 0 {
            return write!(f, "turns=0 (no response measured yet)");
        }
        write!(
            f,
            "turns={} last={}ms p50={}ms p90={}ms p99={}ms min={}ms max={}ms",
            self.count,
            self.last_ms,
            self.p50_ms,
            self.p90_ms,
            self.p99_ms,
            self.min_ms,
            self.max_ms
        )
    }
}

/// Lock-free per-turn latency recorder: scalars, a histogram, and a ring of
/// recent samples, all atomics.
struct LatencyRecorder {
    last_ns: AtomicU64,
    sum_ns: AtomicU64,
    count: AtomicU64,
    min_ns: AtomicU64,
    max_ns: AtomicU64,
    buckets: [AtomicU64; BUCKET_COUNT],
    recent: [AtomicU64; LATENCY_RECENT_WINDOW],
    recent_next: AtomicU64,
}

impl LatencyRecorder {
    fn new() -> Self {
        Self {
            last_ns: AtomicU64::new(0),
            sum_ns: AtomicU64::new(0),
            count: AtomicU64::new(0),
            min_ns: AtomicU64::new(u64::MAX),
            max_ns: AtomicU64::new(0),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            recent: std::array::from_fn(|_| AtomicU64::new(0)),
            recent_next: AtomicU64::new(0),
        }
    }

    #[inline]
    fn record(&self, latency_ns: u64) {
        self.last_ns.store(latency_ns, Relaxed);
        self.sum_ns.fetch_add(latency_ns, Relaxed);
        self.count.fetch_add(1, Relaxed);
        self.min_ns.fetch_min(latency_ns, Relaxed);
        self.max_ns.fetch_max(latency_ns, Relaxed);

        let ms = latency_ns / 1_000_000;
        let bucket = LATENCY_BUCKETS_MS
            .iter()
            .position(|&upper| ms <= upper)
            .unwrap_or(BUCKET_COUNT - 1);
        self.buckets[bucket].fetch_add(1, Relaxed);

        // Ring of recent samples: one atomic slot per turn, index wraps.
        let slot = self.recent_next.fetch_add(1, Relaxed) as usize % LATENCY_RECENT_WINDOW;
        self.recent[slot].store(latency_ns, Relaxed);
    }

    fn stats(&self) -> LatencyStats {
        let count = self.count.load(Relaxed);
        let histogram = self
            .buckets
            .iter()
            .enumerate()
            .map(|(i, b)| LatencyBucket {
                upper_ms: LATENCY_BUCKETS_MS.get(i).copied(),
                count: b.load(Relaxed),
            })
            .collect();
        if count == 0 {
            return LatencyStats {
                histogram,
                ..LatencyStats::default()
            };
        }

        let written = self.recent_next.load(Relaxed) as usize;
        let filled = written.min(LATENCY_RECENT_WINDOW);
        let mut recent: Vec<u64> = self.recent[..filled]
            .iter()
            .map(|s| s.load(Relaxed))
            .collect();
        recent.sort_unstable();
        // Nearest-rank percentile over the sorted recent window.
        let pct = |p: usize| -> u64 {
            let rank = (p * recent.len()).div_ceil(100).max(1);
            recent[rank - 1] / 1_000_000
        };

        LatencyStats {
            count,
            last_ms: self.last_ns.load(Relaxed) / 1_000_000,
            min_ms: self.min_ns.load(Relaxed) / 1_000_000,
            max_ms: self.max_ns.load(Relaxed) / 1_000_000,
            mean_ms: self.sum_ns.load(Relaxed) / count / 1_000_000,
            p50_ms: pct(50),
            p90_ms: pct(90),
            p99_ms: pct(99),
            histogram,
        }
    }
}

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
    /// Timestamp when user sent text (for text-input latency tracking).
    text_send_ns: AtomicU64,
    awaiting_text_response: AtomicBool,
    /// Per-turn response latency: end of user speech (or text send) to the
    /// model's first output.
    latency: LatencyRecorder,

    // ── Turn timing ──
    turn_complete_count: AtomicU64,
    last_turn_start_ns: AtomicU64,
    turn_duration_sum_ns: AtomicU64,
    turn_duration_count: AtomicU64,

    // ── Token usage (from UsageMetadata) ──
    /// Latest total token count from server.
    total_token_count: AtomicU64,
    /// Latest prompt token count from server.
    prompt_token_count: AtomicU64,
    /// Latest response token count from server.
    response_token_count: AtomicU64,
    /// Latest cached content token count from server.
    cached_content_token_count: AtomicU64,
    /// Latest thoughts token count (thinking models).
    thoughts_token_count: AtomicU64,
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
            text_send_ns: AtomicU64::new(0),
            awaiting_text_response: AtomicBool::new(false),
            latency: LatencyRecorder::new(),
            turn_complete_count: AtomicU64::new(0),
            last_turn_start_ns: AtomicU64::new(0),
            turn_duration_sum_ns: AtomicU64::new(0),
            turn_duration_count: AtomicU64::new(0),
            total_token_count: AtomicU64::new(0),
            prompt_token_count: AtomicU64::new(0),
            response_token_count: AtomicU64::new(0),
            cached_content_token_count: AtomicU64::new(0),
            thoughts_token_count: AtomicU64::new(0),
        }
    }

    // ── Atomic methods (~1ns each) ──

    /// Record an outgoing audio chunk. Called from the telemetry lane.
    ///
    /// Returns the response latency when this chunk is the model's first
    /// output after the user's end of speech (or text send) — once per turn,
    /// via a CAS so only the first chunk wins.
    #[inline]
    pub fn record_audio_out(&self, byte_len: usize) -> Option<Duration> {
        self.audio_chunks_out.fetch_add(1, Relaxed);
        self.audio_bytes_out.fetch_add(byte_len as u64, Relaxed);

        // A text send answered with audio counts as a response too.
        let text = self.record_text_response_latency();

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
                self.latency.record(latency);
                return Some(Duration::from_nanos(latency));
            }
        }
        text
    }

    /// Record VAD end (user stopped speaking).
    #[inline]
    pub fn record_vad_end(&self) {
        self.vad_end_ns.store(self.elapsed_ns(), Relaxed);
        self.awaiting_response.store(true, Relaxed);
    }

    /// Record that user sent a text message (for text-input latency tracking).
    #[inline]
    pub fn record_text_send(&self) {
        self.text_send_ns.store(self.elapsed_ns(), Relaxed);
        self.awaiting_text_response.store(true, Relaxed);
    }

    /// Record first model output for text-input latency.
    /// Call on first TextDelta or AudioData after a text send.
    #[inline]
    fn record_text_response_latency(&self) -> Option<Duration> {
        if self
            .awaiting_text_response
            .compare_exchange(true, false, Relaxed, Relaxed)
            .is_ok()
        {
            let now_ns = self.elapsed_ns();
            let send_ns = self.text_send_ns.load(Relaxed);
            if now_ns > send_ns && send_ns > 0 {
                let latency = now_ns - send_ns;
                self.latency.record(latency);
                return Some(Duration::from_nanos(latency));
            }
        }
        None
    }

    /// Record first model text output (TextDelta). Tracks text-input latency.
    ///
    /// Returns the response latency when this delta is the model's first
    /// output after a text send.
    #[inline]
    pub fn record_text_out(&self) -> Option<Duration> {
        self.record_text_response_latency()
    }

    /// Record an interruption (barge-in).
    #[inline]
    pub fn record_interruption(&self) {
        self.interruptions.fetch_add(1, Relaxed);
    }

    /// Record turn completion for duration tracking.
    #[inline]
    pub fn record_turn_complete(&self) {
        self.turn_complete_count.fetch_add(1, Relaxed);
        let now = self.elapsed_ns();
        let turn_start = self.last_turn_start_ns.swap(now, Relaxed);
        if turn_start > 0 {
            let duration = now.saturating_sub(turn_start);
            self.turn_duration_sum_ns.fetch_add(duration, Relaxed);
            self.turn_duration_count.fetch_add(1, Relaxed);
        }
    }

    /// Record token usage from a `UsageMetadata` event.
    #[inline]
    pub fn record_usage(
        &self,
        total: Option<u32>,
        prompt: Option<u32>,
        response: Option<u32>,
        cached: Option<u32>,
        thoughts: Option<u32>,
    ) {
        if let Some(v) = total {
            self.total_token_count.store(v as u64, Relaxed);
        }
        if let Some(v) = prompt {
            self.prompt_token_count.store(v as u64, Relaxed);
        }
        if let Some(v) = response {
            self.response_token_count.store(v as u64, Relaxed);
        }
        if let Some(v) = cached {
            self.cached_content_token_count.store(v as u64, Relaxed);
        }
        if let Some(v) = thoughts {
            self.thoughts_token_count.store(v as u64, Relaxed);
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

    /// Per-turn response latency: end of user speech (or text send) to the
    /// model's first output, as a distribution over the session so far.
    ///
    /// Cheap enough to call once per turn (it sorts at most
    /// [`LATENCY_RECENT_WINDOW`] samples); not for the audio hot path.
    pub fn latency(&self) -> LatencyStats {
        self.latency.stats()
    }

    /// Snapshot all metrics as a JSON value.
    ///
    /// The flat `*_response_latency_ms` keys are kept for existing dashboards;
    /// `response_latency` carries the full [`LatencyStats`] (percentiles and
    /// histogram included).
    pub fn snapshot(&self) -> serde_json::Value {
        let elapsed = self.start.elapsed();
        let elapsed_secs = elapsed.as_secs_f64();

        let chunks = self.audio_chunks_out.load(Relaxed);
        let bytes = self.audio_bytes_out.load(Relaxed);
        let latency = self.latency.stats();

        let turn_count = self.turn_duration_count.load(Relaxed);
        let turn_complete_count = self.turn_complete_count.load(Relaxed);
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

        let total_tokens = self.total_token_count.load(Relaxed);
        let prompt_tokens = self.prompt_token_count.load(Relaxed);
        let response_tokens = self.response_token_count.load(Relaxed);
        let cached_tokens = self.cached_content_token_count.load(Relaxed);
        let thoughts_tokens = self.thoughts_token_count.load(Relaxed);

        json!({
            "uptime_secs": elapsed.as_secs(),
            "audio_chunks_out": chunks,
            "audio_kbytes_out": bytes / 1024,
            "audio_throughput_kbps": (throughput_kbps * 10.0).round() / 10.0,
            "interruptions": self.interruptions.load(Relaxed),
            "last_response_latency_ms": latency.last_ms,
            "avg_response_latency_ms": latency.mean_ms,
            "min_response_latency_ms": latency.min_ms,
            "max_response_latency_ms": latency.max_ms,
            "response_count": latency.count,
            "response_latency": latency,
            "turn_count": turn_complete_count,
            "avg_turn_duration_ms": avg_turn_ms,
            "total_token_count": total_tokens,
            "prompt_token_count": prompt_tokens,
            "response_token_count": response_tokens,
            "cached_content_token_count": cached_tokens,
            "thoughts_token_count": thoughts_tokens,
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
        assert_eq!(snap["turn_count"], 0);
        assert_eq!(snap["response_latency"]["count"], 0);
        assert_eq!(
            t.latency(),
            LatencyStats {
                histogram: t.latency().histogram.clone(),
                ..LatencyStats::default()
            }
        );
        assert_eq!(
            t.latency().to_string(),
            "turns=0 (no response measured yet)"
        );
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
    fn turn_complete_counter_is_independent_of_latency() {
        let t = SessionTelemetry::new();
        t.record_turn_complete();
        t.record_turn_complete();

        let snap = t.snapshot();
        assert_eq!(snap["turn_count"], 2);
        assert_eq!(snap["response_count"], 0);
    }

    #[test]
    fn latency_tracking() {
        let t = SessionTelemetry::new();
        // Simulate: VAD end → short delay → first audio chunk
        t.record_vad_end();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let first = t.record_audio_out(480);
        assert!(
            first.is_some(),
            "first chunk after VAD end reports the latency"
        );
        // Subsequent chunks should not re-record latency
        assert!(t.record_audio_out(480).is_none());
        assert!(t.record_audio_out(480).is_none());

        let snap = t.snapshot();
        assert_eq!(snap["response_count"], 1);
        // Latency should be >= 10ms (we slept 10ms)
        assert!(snap["last_response_latency_ms"].as_u64().unwrap() >= 5);
        assert_eq!(snap["response_latency"]["count"], 1);
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

    #[test]
    fn text_input_latency_via_text_out() {
        let t = SessionTelemetry::new();
        // Simulate: user sends text → delay → model responds with text
        t.record_text_send();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(t.record_text_out().is_some());
        // Subsequent text outputs should not re-record
        assert!(t.record_text_out().is_none());

        let snap = t.snapshot();
        assert_eq!(snap["response_count"], 1);
        assert!(snap["last_response_latency_ms"].as_u64().unwrap() >= 5);
    }

    #[test]
    fn text_input_latency_via_audio_out() {
        let t = SessionTelemetry::new();
        // Simulate: user sends text → delay → model responds with audio
        t.record_text_send();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(t.record_audio_out(480).is_some());

        let snap = t.snapshot();
        // Should record text-send latency (response_count = 1)
        assert_eq!(snap["response_count"], 1);
        assert!(snap["last_response_latency_ms"].as_u64().unwrap() >= 5);
    }

    #[test]
    fn mixed_voice_and_text_turns() {
        let t = SessionTelemetry::new();

        // Voice turn
        t.record_vad_end();
        std::thread::sleep(std::time::Duration::from_millis(10));
        t.record_audio_out(480);

        // Text turn
        t.record_text_send();
        std::thread::sleep(std::time::Duration::from_millis(10));
        t.record_text_out();

        let snap = t.snapshot();
        assert_eq!(snap["response_count"], 2);
    }

    // ── LatencyRecorder: distribution semantics, fed directly in nanos ──

    fn ms(n: u64) -> u64 {
        n * 1_000_000
    }

    #[test]
    fn recorder_scalars_and_percentiles() {
        let r = LatencyRecorder::new();
        for v in [300, 100, 500, 200, 400] {
            r.record(ms(v));
        }
        let s = r.stats();
        assert_eq!(s.count, 5);
        assert_eq!(s.last_ms, 400);
        assert_eq!(s.min_ms, 100);
        assert_eq!(s.max_ms, 500);
        assert_eq!(s.mean_ms, 300);
        // Nearest rank over [100,200,300,400,500]: p50 → rank 3 → 300,
        // p90 → rank 5 → 500, p99 → rank 5 → 500.
        assert_eq!(s.p50_ms, 300);
        assert_eq!(s.p90_ms, 500);
        assert_eq!(s.p99_ms, 500);
        assert_eq!(
            s.to_string(),
            "turns=5 last=400ms p50=300ms p90=500ms p99=500ms min=100ms max=500ms"
        );
    }

    #[test]
    fn recorder_histogram_buckets_by_upper_bound() {
        let r = LatencyRecorder::new();
        r.record(ms(50)); // ≤ 50 → bucket 0 (inclusive bound)
        r.record(ms(51)); // ≤ 100 → bucket 1
        r.record(ms(999)); // ≤ 1000 → bucket 9
        r.record(ms(9_000)); // > 5000 → open top bucket
        let h = r.stats().histogram;
        assert_eq!(h.len(), LATENCY_BUCKETS_MS.len() + 1);
        assert_eq!(
            h[0],
            LatencyBucket {
                upper_ms: Some(50),
                count: 1
            }
        );
        assert_eq!(
            h[1],
            LatencyBucket {
                upper_ms: Some(100),
                count: 1
            }
        );
        assert_eq!(
            h[9],
            LatencyBucket {
                upper_ms: Some(1000),
                count: 1
            }
        );
        assert_eq!(
            h[h.len() - 1],
            LatencyBucket {
                upper_ms: None,
                count: 1
            }
        );
        assert_eq!(h.iter().map(|b| b.count).sum::<u64>(), 4);
    }

    #[test]
    fn recorder_percentiles_use_recent_window_only() {
        let r = LatencyRecorder::new();
        // Fill the window with slow turns, then overwrite it entirely with
        // fast ones: the percentiles follow the recent window, min/max and
        // the histogram keep the whole session.
        for _ in 0..LATENCY_RECENT_WINDOW {
            r.record(ms(2_000));
        }
        for _ in 0..LATENCY_RECENT_WINDOW {
            r.record(ms(200));
        }
        let s = r.stats();
        assert_eq!(s.count, 2 * LATENCY_RECENT_WINDOW as u64);
        assert_eq!(s.p50_ms, 200);
        assert_eq!(s.p99_ms, 200);
        assert_eq!(s.max_ms, 2_000);
        assert_eq!(s.min_ms, 200);
        let slow: u64 = s
            .histogram
            .iter()
            .filter(|b| b.upper_ms == Some(2000))
            .map(|b| b.count)
            .sum();
        assert_eq!(slow, LATENCY_RECENT_WINDOW as u64);
    }

    #[test]
    fn stats_round_trip_through_json() {
        let r = LatencyRecorder::new();
        r.record(ms(420));
        let s = r.stats();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["p50_ms"], 420);
        let back: LatencyStats = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }
}
