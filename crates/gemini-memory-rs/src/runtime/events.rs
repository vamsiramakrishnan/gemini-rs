//! The bridge from Gemini Live's fast lane to the memory control lane.
//!
//! Live callbacks like `on_input_transcript` run synchronously on the
//! event-dispatch hot path with a sub-millisecond budget. Everything this
//! module exposes to those callbacks is a bounded `try_send` and nothing else —
//! no locks, no parsing, no async, and never a blocking send. A dropped
//! speculative event costs a little retrieval quality; a blocked audio callback
//! costs the conversation.

use std::sync::Arc;
use tokio::sync::mpsc;

use crate::core::TurnId;

/// Default depth of the transcript channel.
///
/// Deep enough to absorb a burst of partial-transcript revisions, shallow
/// enough that a stalled consumer is noticed rather than silently buffered.
pub const DEFAULT_CHANNEL_DEPTH: usize = 256;

/// Something the memory control loop needs to react to.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryRuntimeEvent {
    /// The user started speaking; freeze the prepared snapshot for this turn.
    UserActivityStarted {
        /// The turn beginning.
        turn_id: TurnId,
    },
    /// A revised partial transcript. Speculative input only.
    PartialTranscript {
        /// The turn it belongs to.
        turn_id: TurnId,
        /// The text as currently recognised.
        text: Arc<str>,
    },
    /// The finalized user transcript. The only admissible evidence.
    FinalTranscript {
        /// The turn it belongs to.
        turn_id: TurnId,
        /// The finalized text.
        text: Arc<str>,
    },
    /// The user stopped speaking.
    UserActivityEnded {
        /// The turn that ended.
        turn_id: TurnId,
    },
    /// The model finished its response for a turn.
    TurnCompleted {
        /// The turn that completed.
        turn_id: TurnId,
    },
    /// The model asked to recall context.
    RecallRequested {
        /// The query it asked with.
        query: Arc<str>,
        /// The turn it asked on.
        turn_id: TurnId,
    },
    /// Nothing has happened for a while; consider sealing.
    IdleTick,
    /// The conversation is over; seal and reconcile.
    SessionEnded,
}

impl MemoryRuntimeEvent {
    /// A stable label for tracing and metrics.
    pub fn label(&self) -> &'static str {
        match self {
            Self::UserActivityStarted { .. } => "user_activity_started",
            Self::PartialTranscript { .. } => "partial_transcript",
            Self::FinalTranscript { .. } => "final_transcript",
            Self::UserActivityEnded { .. } => "user_activity_ended",
            Self::TurnCompleted { .. } => "turn_completed",
            Self::RecallRequested { .. } => "recall_requested",
            Self::IdleTick => "idle_tick",
            Self::SessionEnded => "session_ended",
        }
    }

    /// Whether dropping this event under backpressure is acceptable.
    ///
    /// Partial transcripts are speculative and safe to drop. Everything else
    /// changes durable state or turn bookkeeping and must not be lost.
    pub fn is_droppable(&self) -> bool {
        matches!(self, Self::PartialTranscript { .. })
    }
}

/// The fast-lane handle handed to Live callbacks.
///
/// Cloneable, cheap, and sync. Every method is non-blocking.
#[derive(Debug, Clone)]
pub struct MemoryEventSender {
    tx: mpsc::Sender<MemoryRuntimeEvent>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
}

impl MemoryEventSender {
    /// Wrap a channel sender.
    pub fn new(tx: mpsc::Sender<MemoryRuntimeEvent>) -> Self {
        Self {
            tx,
            dropped: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Offer an event without ever blocking.
    ///
    /// Returns `false` when the event was dropped, which the caller may report
    /// but must not retry from a fast-lane callback.
    pub fn offer(&self, event: MemoryRuntimeEvent) -> bool {
        match self.tx.try_send(event) {
            Ok(()) => true,
            Err(_) => {
                self.dropped
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            }
        }
    }

    /// How many events have been dropped under backpressure.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Offer a partial transcript revision.
    pub fn partial_transcript(&self, turn_id: TurnId, text: &str) -> bool {
        self.offer(MemoryRuntimeEvent::PartialTranscript {
            turn_id,
            text: Arc::from(text),
        })
    }

    /// Offer a finalized transcript.
    pub fn final_transcript(&self, turn_id: TurnId, text: &str) -> bool {
        self.offer(MemoryRuntimeEvent::FinalTranscript {
            turn_id,
            text: Arc::from(text),
        })
    }

    /// Offer the transcript callback's `(text, is_final)` pair directly.
    ///
    /// Shaped to match `Live::on_input_transcript` so the callback body is one
    /// line and cannot accidentally grow.
    pub fn input_transcript(&self, turn_id: TurnId, text: &str, is_final: bool) -> bool {
        if is_final {
            self.final_transcript(turn_id, text)
        } else {
            self.partial_transcript(turn_id, text)
        }
    }

    /// Signal that the user started speaking.
    pub fn user_activity_started(&self, turn_id: TurnId) -> bool {
        self.offer(MemoryRuntimeEvent::UserActivityStarted { turn_id })
    }

    /// Signal that the user stopped speaking.
    pub fn user_activity_ended(&self, turn_id: TurnId) -> bool {
        self.offer(MemoryRuntimeEvent::UserActivityEnded { turn_id })
    }

    /// Signal that a turn completed.
    pub fn turn_completed(&self, turn_id: TurnId) -> bool {
        self.offer(MemoryRuntimeEvent::TurnCompleted { turn_id })
    }

    /// Signal that the conversation is over.
    pub fn session_ended(&self) -> bool {
        self.offer(MemoryRuntimeEvent::SessionEnded)
    }
}

/// Create the fast-lane sender and the control-lane receiver.
pub fn channel(depth: usize) -> (MemoryEventSender, mpsc::Receiver<MemoryRuntimeEvent>) {
    let (tx, rx) = mpsc::channel(depth.max(1));
    (MemoryEventSender::new(tx), rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn events_reach_the_control_lane_in_order() {
        let (sender, mut rx) = channel(8);
        sender.user_activity_started(TurnId(1));
        sender.partial_transcript(TurnId(1), "I am");
        sender.final_transcript(TurnId(1), "I am pescatarian");
        sender.turn_completed(TurnId(1));

        assert_eq!(
            rx.recv().await.unwrap(),
            MemoryRuntimeEvent::UserActivityStarted { turn_id: TurnId(1) }
        );
        assert_eq!(rx.recv().await.unwrap().label(), "partial_transcript");
        assert_eq!(rx.recv().await.unwrap().label(), "final_transcript");
        assert_eq!(rx.recv().await.unwrap().label(), "turn_completed");
    }

    #[tokio::test]
    async fn a_full_channel_drops_rather_than_blocking_the_fast_lane() {
        let (sender, _rx) = channel(2);
        assert!(sender.partial_transcript(TurnId(1), "one"));
        assert!(sender.partial_transcript(TurnId(1), "one two"));
        // Third offer finds the channel full and is dropped, not awaited.
        assert!(!sender.partial_transcript(TurnId(1), "one two three"));
        assert_eq!(sender.dropped_count(), 1);
    }

    #[tokio::test]
    async fn a_closed_channel_does_not_panic_the_callback() {
        let (sender, rx) = channel(4);
        drop(rx);
        assert!(!sender.final_transcript(TurnId(1), "I am pescatarian"));
    }

    #[test]
    fn only_speculative_events_are_droppable() {
        assert!(MemoryRuntimeEvent::PartialTranscript {
            turn_id: TurnId(1),
            text: Arc::from("x"),
        }
        .is_droppable());

        for event in [
            MemoryRuntimeEvent::FinalTranscript {
                turn_id: TurnId(1),
                text: Arc::from("x"),
            },
            MemoryRuntimeEvent::TurnCompleted { turn_id: TurnId(1) },
            MemoryRuntimeEvent::SessionEnded,
        ] {
            assert!(
                !event.is_droppable(),
                "{} must not be dropped",
                event.label()
            );
        }
    }

    #[tokio::test]
    async fn the_transcript_callback_shape_routes_by_finality() {
        let (sender, mut rx) = channel(4);
        sender.input_transcript(TurnId(1), "I am", false);
        sender.input_transcript(TurnId(1), "I am pescatarian", true);
        assert_eq!(rx.recv().await.unwrap().label(), "partial_transcript");
        assert_eq!(rx.recv().await.unwrap().label(), "final_transcript");
    }
}
