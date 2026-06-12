//! Semantic events emitted by the L1 processor.
//!
//! Subscribe via `LiveHandle::events()` (broadcast receiver) or
//! `LiveHandle::stream()` (a [`futures::Stream`]). Zero-cost when no
//! subscribers.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use tokio::sync::broadcast;

/// Semantic events emitted by the Live session processor.
///
/// The L1 equivalent of L0's [`SessionEvent`](gemini_genai_rs::prelude::SessionEvent).
/// L0 events are wire-level; LiveEvents are semantic (extractions completed,
/// phases transitioned, tools executed).
///
/// Subscribe via [`LiveHandle::events()`](super::handle::LiveHandle::events).
/// Multiple independent subscribers supported. Zero-cost when no subscribers
/// exist (`broadcast::send` with 0 receivers is a no-op).
#[derive(Debug, Clone)]
pub enum LiveEvent {
    // -- Fast-lane events (high frequency, sync emission) --
    /// Raw PCM audio from model. Uses `Bytes` (refcounted) — clone is
    /// a pointer increment (~2ns), not a deep copy.
    Audio(Bytes),
    /// Incremental text token from model.
    TextDelta(String),
    /// Complete text response (all deltas concatenated).
    TextComplete(String),
    /// User speech transcription.
    InputTranscript {
        /// The transcribed text content.
        text: String,
        /// Whether this is the final transcription for the utterance.
        is_final: bool,
    },
    /// Model speech transcription.
    OutputTranscript {
        /// The transcribed text content.
        text: String,
        /// Whether this is the final transcription for the utterance.
        is_final: bool,
    },
    /// Model reasoning/thinking content.
    Thought(String),
    /// Voice activity detected — user started speaking.
    VadStart,
    /// Voice activity ended — user stopped speaking.
    VadEnd,

    // -- Control-lane events (lower frequency, async emission) --
    /// Extraction completed. Emitted for both the top-level result
    /// AND each flattened key (e.g., "order.items", "order.phase").
    Extraction {
        /// Extractor name, or `"extractor.field"` for flattened keys.
        name: String,
        /// The extracted JSON value.
        value: serde_json::Value,
    },
    /// Extraction failed.
    ExtractionError {
        /// Name of the extractor that failed.
        name: String,
        /// Human-readable error description.
        error: String,
    },
    /// A raw extraction field was considered for promotion into authoritative state.
    StatePromotion {
        /// Extractor name that produced the field.
        extractor: String,
        /// Field name inside the extractor result.
        field: String,
        /// State key targeted by the promotion rule.
        state_key: String,
        /// Whether the promotion was accepted and written.
        accepted: bool,
        /// Human-readable reason for the decision.
        reason: String,
        /// Extracted value that was considered.
        value: serde_json::Value,
    },
    /// Phase machine transitioned.
    PhaseTransition {
        /// Phase the machine transitioned from.
        from: String,
        /// Phase the machine transitioned to.
        to: String,
        /// Human-readable reason for the transition.
        reason: String,
    },
    /// Tool dispatched and result obtained.
    ToolExecution {
        /// Name of the tool that was called.
        name: String,
        /// Arguments passed to the tool.
        args: serde_json::Value,
        /// Result returned by the tool.
        result: serde_json::Value,
    },
    /// Tool calls cancelled — either by the server (a `ToolCallCancelled`
    /// wire event) or locally when a user barge-in interrupted an in-flight
    /// inline tool. No response is sent for a cancelled call, and a cancelled
    /// call never advances the governed flow.
    ToolCancelled {
        /// IDs of the cancelled tool calls.
        ids: Vec<String>,
    },
    /// Model completed a conversational turn.
    TurnComplete,
    /// Model output interrupted by user speech.
    Interrupted,
    /// Session connected to Gemini.
    Connected,
    /// Session disconnected.
    Disconnected {
        /// Optional reason for disconnection (server-provided or error message).
        reason: Option<String>,
    },
    /// Unrecoverable error.
    Error(String),
    /// Server requesting session wind-down.
    GoAway {
        /// Time remaining before the server closes the connection.
        time_left: Duration,
    },

    // -- Periodic events --
    /// Aggregated session telemetry snapshot.
    Telemetry(serde_json::Value),
    /// Per-turn latency and token metrics.
    TurnMetrics {
        /// Turn number (1-indexed).
        turn: u32,
        /// End-to-end latency for this turn in milliseconds.
        latency_ms: u32,
        /// Number of prompt tokens consumed.
        prompt_tokens: u32,
        /// Number of response tokens generated.
        response_tokens: u32,
    },
}

/// A [`futures::Stream`] of [`LiveEvent`]s from a Live session.
///
/// Created by [`LiveHandle::stream()`](super::handle::LiveHandle::stream).
/// Wraps the underlying [`broadcast::Receiver`] with stream semantics:
///
/// - **Lagged**: if this subscriber falls behind the broadcast buffer, the
///   missed events are skipped and the stream continues with the next
///   available event (no error item is yielded).
/// - **Closed**: when the session's event channel closes, the stream ends
///   (`next()` returns `None`).
///
/// Composes with all `futures`/`tokio-stream` combinators:
///
/// ```rust,ignore
/// use futures::StreamExt;
///
/// let mut stream = handle.stream();
/// while let Some(ev) = stream.next().await {
///     match ev {
///         LiveEvent::TextDelta(t) => print!("{t}"),
///         LiveEvent::TurnComplete => println!(),
///         _ => {}
///     }
/// }
/// ```
pub struct LiveEventStream {
    inner: Pin<Box<dyn Stream<Item = LiveEvent> + Send>>,
}

impl LiveEventStream {
    /// Wrap a broadcast receiver of [`LiveEvent`]s as a stream.
    pub(crate) fn new(rx: broadcast::Receiver<LiveEvent>) -> Self {
        let inner = futures::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => return Some((ev, rx)),
                    // Skip lagged (missed) events and keep going.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    // Channel closed: end the stream.
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        Self {
            inner: Box::pin(inner),
        }
    }
}

impl Stream for LiveEventStream {
    type Item = LiveEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl std::fmt::Debug for LiveEventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveEventStream").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn stream_yields_events_in_order_and_ends_on_close() {
        let (tx, rx) = broadcast::channel::<LiveEvent>(16);
        let mut stream = LiveEventStream::new(rx);

        tx.send(LiveEvent::VadStart).unwrap();
        tx.send(LiveEvent::TextDelta("hi".into())).unwrap();
        tx.send(LiveEvent::TurnComplete).unwrap();

        assert!(matches!(stream.next().await, Some(LiveEvent::VadStart)));
        match stream.next().await {
            Some(LiveEvent::TextDelta(t)) => assert_eq!(t, "hi"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        assert!(matches!(stream.next().await, Some(LiveEvent::TurnComplete)));

        // Closing the channel ends the stream.
        drop(tx);
        assert!(stream.next().await.is_none(), "stream ends on Closed");
    }

    #[tokio::test]
    async fn stream_skips_lagged_events_and_continues() {
        // Capacity-2 channel: sending 5 events before polling forces a lag.
        let (tx, rx) = broadcast::channel::<LiveEvent>(2);
        let mut stream = LiveEventStream::new(rx);

        for i in 0..5u32 {
            tx.send(LiveEvent::TextDelta(format!("e{i}"))).unwrap();
        }

        // The first poll observes the lag, skips it, and yields the oldest
        // event still buffered (e3), then e4 — no error, no end-of-stream.
        match stream.next().await {
            Some(LiveEvent::TextDelta(t)) => assert_eq!(t, "e3"),
            other => panic!("expected e3 after lag skip, got {other:?}"),
        }
        match stream.next().await {
            Some(LiveEvent::TextDelta(t)) => assert_eq!(t, "e4"),
            other => panic!("expected e4, got {other:?}"),
        }

        // The stream is still alive after the lag.
        tx.send(LiveEvent::TurnComplete).unwrap();
        assert!(matches!(stream.next().await, Some(LiveEvent::TurnComplete)));

        drop(tx);
        assert!(stream.next().await.is_none());
    }
}
