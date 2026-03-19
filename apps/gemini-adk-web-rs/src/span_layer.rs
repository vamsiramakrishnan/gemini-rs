//! WebSocketSpanLayer — bridges tracing spans from gemini-live/gemini-adk-rs to the browser.
//!
//! Only captures spans with targets starting with `gemini_genai_rs` or `gemini`
//! (the ~13 span types defined in the two spans.rs files). All other tracing
//! output is ignored.
//!
//! Uses a broadcast channel so multiple WebSocket clients can each subscribe
//! to span events independently. If a subscriber falls behind, it skips
//! lagged messages (the primary OTLP export path is unaffected).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::app::ServerMessage;

/// Internal record for an in-flight span.
struct SpanRecord {
    name: String,
    span_id: u64,
    parent_id: Option<u64>,
    start_ns: u64,
    attributes: serde_json::Value,
}

/// Tracing Layer that forwards span close events to a broadcast channel.
///
/// Only captures spans whose target starts with `gemini_genai_rs` or `gemini`.
/// On span close, sends a `ServerMessage::SpanEvent` with the span's name,
/// duration, and attributes.
pub struct WebSocketSpanLayer {
    tx: broadcast::Sender<ServerMessage>,
    spans: Mutex<HashMap<u64, SpanRecord>>,
    next_id: AtomicU64,
    epoch: std::time::Instant,
}

impl WebSocketSpanLayer {
    /// Create a new span layer that broadcasts span events.
    ///
    /// `capacity` controls the broadcast channel buffer (e.g., 256).
    /// Returns the layer and a `broadcast::Sender` that callers can
    /// `.subscribe()` on to receive span events.
    pub fn new(capacity: usize) -> (Self, broadcast::Sender<ServerMessage>) {
        let (tx, _) = broadcast::channel(capacity);
        let sender = tx.clone();
        (
            Self {
                tx,
                spans: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                epoch: std::time::Instant::now(),
            },
            sender,
        )
    }
}

/// Visitor that extracts span attributes into a JSON map.
struct AttrVisitor {
    map: serde_json::Map<String, serde_json::Value>,
}

impl Visit for AttrVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.map
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.map
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.map
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::String(format!("{:?}", value)),
        );
    }
}

impl<S: Subscriber> Layer<S> for WebSocketSpanLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _ctx: Context<'_, S>) {
        let target = attrs.metadata().target();
        // Only capture gemini_genai_rs.* and gemini.agent.* spans
        if !target.starts_with("gemini_genai_rs") && !target.starts_with("gemini") {
            return;
        }

        let mut visitor = AttrVisitor {
            map: serde_json::Map::new(),
        };
        attrs.record(&mut visitor);

        let span_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let record = SpanRecord {
            name: attrs.metadata().name().to_string(),
            span_id,
            parent_id: None,
            start_ns: self.epoch.elapsed().as_nanos() as u64,
            attributes: serde_json::Value::Object(visitor.map),
        };

        if let Ok(mut spans) = self.spans.lock() {
            spans.insert(id.into_u64(), record);
        }
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        let record = {
            let mut spans = match self.spans.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            spans.remove(&id.into_u64())
        };

        if let Some(record) = record {
            let now_ns = self.epoch.elapsed().as_nanos() as u64;
            let duration_us = (now_ns.saturating_sub(record.start_ns)) / 1000;

            let msg = ServerMessage::SpanEvent {
                name: record.name,
                span_id: format!("{:016x}", record.span_id),
                parent_id: record.parent_id.map(|id| format!("{:016x}", id)),
                duration_us,
                attributes: record.attributes,
                status: "ok".to_string(),
            };

            // Non-blocking send — drop if no subscribers or channel full
            let _ = self.tx.send(msg);
        }
    }
}
