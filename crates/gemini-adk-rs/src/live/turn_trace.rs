//! One tracing span per conversation turn.
//!
//! Every lane of the processor (router, control, telemetry) sees the same
//! ordered event stream, so each keeps its own [`TurnTrace`] and advances it
//! when *it* observes `TurnComplete`. The lanes agree on the numbering by
//! construction — turn `n` is everything before the `n`-th `TurnComplete` —
//! without sharing state, and each lane's events land on the right turn even
//! when the lanes run at different speeds. The number matches the
//! `session:turn_count` state key plus one.
//!
//! The span is named `turn` with an `id` field, so
//! `RUST_LOG=gemini_adk_rs::live=debug` shows, per turn, the VAD edges, the
//! response latency, each tool call with its duration, interruptions and the
//! turn boundary — the whole story of why a turn was slow, in one filter.

use tracing::Span;

/// The current turn's span and number for one processor lane.
pub(crate) struct TurnTrace {
    id: u64,
    span: Span,
}

impl TurnTrace {
    /// Start at turn 1.
    pub(crate) fn new() -> Self {
        Self {
            id: 1,
            span: Self::make_span(1),
        }
    }

    fn make_span(id: u64) -> Span {
        tracing::info_span!("turn", id)
    }

    /// A handle to the current turn's span, for `Instrument` / `in_scope`.
    pub(crate) fn span(&self) -> Span {
        self.span.clone()
    }

    /// Move to the next turn. Call after handling `TurnComplete`, so the
    /// boundary event itself lands on the turn it closes.
    pub(crate) fn advance(&mut self) -> u64 {
        self.id += 1;
        self.span = Self::make_span(self.id);
        self.id
    }
}

impl Default for TurnTrace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_one_and_advances() {
        let mut t = TurnTrace::new();
        assert_eq!(t.id, 1);
        assert_eq!(t.advance(), 2);
        assert_eq!(t.advance(), 3);
        assert_eq!(t.id, 3);
    }
}
