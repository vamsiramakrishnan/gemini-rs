//! In-memory trace store backing the `GET /debug/trace/:id` endpoint.
//!
//! The server records a lightweight span tree for each agent run so the debug
//! endpoint can return real execution traces instead of an empty placeholder.
//! This is a self-contained recorder of what the *server* does (agent run timing
//! and outcome); deep L1 spans still flow to OTLP / Cloud Trace when the agent
//! runtime is built with the `tracing-support` + exporter features.
//!
//! Traces are capped at [`MAX_TRACES`] with FIFO eviction so the store cannot
//! grow without bound in a long-lived server.

use std::collections::VecDeque;
use std::time::Instant;

use parking_lot::RwLock;
use serde::Serialize;

use crate::types::now_iso8601;

/// Maximum number of traces retained in memory before FIFO eviction.
pub const MAX_TRACES: usize = 256;

/// A single timed span within a trace.
#[derive(Debug, Clone, Serialize)]
pub struct SpanRecord {
    /// Span name (e.g. `agent.run`).
    pub name: String,
    /// Offset from trace start, in milliseconds.
    pub start_ms: u128,
    /// Span duration, in milliseconds.
    pub duration_ms: u128,
    /// Structured span attributes.
    pub attributes: serde_json::Value,
}

/// A recorded execution trace: a root operation and its timed spans.
#[derive(Debug, Clone, Serialize)]
pub struct TraceRecord {
    /// Unique trace identifier (returned to clients to fetch this trace).
    pub trace_id: String,
    /// Root operation name.
    pub root: String,
    /// ISO-8601 timestamp of when the trace started.
    pub started_at: String,
    /// Total wall-clock duration, in milliseconds.
    pub duration_ms: u128,
    /// Whether the traced operation completed successfully.
    pub ok: bool,
    /// The spans recorded during this trace, in start order.
    pub spans: Vec<SpanRecord>,
}

/// Thread-safe, bounded store of recent [`TraceRecord`]s.
#[derive(Default)]
pub struct TraceStore {
    traces: RwLock<VecDeque<TraceRecord>>,
}

impl TraceStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            traces: RwLock::new(VecDeque::new()),
        }
    }

    /// Record a trace, evicting the oldest if at capacity.
    pub fn record(&self, trace: TraceRecord) {
        let mut traces = self.traces.write();
        traces.push_back(trace);
        while traces.len() > MAX_TRACES {
            traces.pop_front();
        }
    }

    /// Fetch a trace by id (newest match wins).
    pub fn get(&self, trace_id: &str) -> Option<TraceRecord> {
        self.traces
            .read()
            .iter()
            .rev()
            .find(|t| t.trace_id == trace_id)
            .cloned()
    }

    /// List all retained traces, oldest first.
    pub fn list(&self) -> Vec<TraceRecord> {
        self.traces.read().iter().cloned().collect()
    }
}

/// Builder that accumulates timed spans for a single trace.
///
/// Construct at the start of an operation, add spans as work completes, then
/// [`finish`](TraceBuilder::finish) into a [`TraceRecord`] for the store.
pub struct TraceBuilder {
    trace_id: String,
    root: String,
    started_at: String,
    start: Instant,
    ok: bool,
    spans: Vec<SpanRecord>,
}

impl TraceBuilder {
    /// Start a new trace with a generated id and the given root operation name.
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            trace_id: uuid::Uuid::new_v4().to_string(),
            root: root.into(),
            started_at: now_iso8601(),
            start: Instant::now(),
            ok: true,
            spans: Vec::new(),
        }
    }

    /// The id assigned to this trace.
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Record a span that started at `since` and ran for `duration`.
    pub fn span(
        &mut self,
        name: impl Into<String>,
        since: Instant,
        duration: std::time::Duration,
        attributes: serde_json::Value,
    ) {
        self.spans.push(SpanRecord {
            name: name.into(),
            start_ms: since.duration_since(self.start).as_millis(),
            duration_ms: duration.as_millis(),
            attributes,
        });
    }

    /// Mark the traced operation as failed.
    pub fn fail(&mut self) {
        self.ok = false;
    }

    /// Finalize into a [`TraceRecord`].
    pub fn finish(self) -> TraceRecord {
        TraceRecord {
            trace_id: self.trace_id,
            root: self.root,
            started_at: self.started_at,
            duration_ms: self.start.elapsed().as_millis(),
            ok: self.ok,
            spans: self.spans,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(id: &str) -> TraceRecord {
        TraceRecord {
            trace_id: id.into(),
            root: "agent.run".into(),
            started_at: now_iso8601(),
            duration_ms: 1,
            ok: true,
            spans: vec![],
        }
    }

    #[test]
    fn record_and_get() {
        let store = TraceStore::new();
        store.record(trace("abc"));
        assert!(store.get("abc").is_some());
        assert!(store.get("missing").is_none());
    }

    #[test]
    fn evicts_oldest_past_capacity() {
        let store = TraceStore::new();
        for i in 0..(MAX_TRACES + 10) {
            store.record(trace(&format!("t{i}")));
        }
        assert_eq!(store.list().len(), MAX_TRACES);
        // The first 10 should have been evicted.
        assert!(store.get("t0").is_none());
        assert!(store.get(&format!("t{}", MAX_TRACES + 9)).is_some());
    }

    #[test]
    fn builder_records_spans_and_timing() {
        let mut b = TraceBuilder::new("agent.run");
        let id = b.trace_id().to_string();
        let started = Instant::now();
        b.span(
            "child",
            started,
            std::time::Duration::from_millis(5),
            serde_json::json!({"k": "v"}),
        );
        let rec = b.finish();
        assert_eq!(rec.trace_id, id);
        assert_eq!(rec.spans.len(), 1);
        assert_eq!(rec.spans[0].name, "child");
        assert_eq!(rec.spans[0].duration_ms, 5);
    }
}
