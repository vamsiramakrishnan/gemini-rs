//! Transcript accumulation buffer for Live sessions.
//!
//! Automatically accumulates input/output transcripts per-turn with windowing
//! support for OOB extraction pipelines.

use std::fmt::Write;
use std::time::Instant;

/// A single completed conversation turn with accumulated transcripts.
#[derive(Debug, Clone)]
pub struct TranscriptTurn {
    /// Sequential turn number (0-based).
    pub turn_number: u32,
    /// Accumulated user (input) transcript for this turn.
    pub user: String,
    /// Accumulated model (output) transcript for this turn.
    pub model: String,
    /// When this turn was finalized.
    pub timestamp: Instant,
}

/// Accumulates input/output transcripts and segments them by turn boundaries.
///
/// Thread safety: wrap in `Arc<parking_lot::Mutex<TranscriptBuffer>>` when
/// sharing between fast lane (push) and control lane (end_turn / window).
#[derive(Debug)]
pub struct TranscriptBuffer {
    turns: Vec<TranscriptTurn>,
    current_user: String,
    current_model: String,
    turn_count: u32,
}

impl TranscriptBuffer {
    pub fn new() -> Self {
        Self {
            turns: Vec::new(),
            current_user: String::new(),
            current_model: String::new(),
            turn_count: 0,
        }
    }

    /// Append input (user speech) transcript text.
    pub fn push_input(&mut self, text: &str) {
        self.current_user.push_str(text);
    }

    /// Append output (model speech) transcript text.
    pub fn push_output(&mut self, text: &str) {
        self.current_model.push_str(text);
    }

    /// Finalize the current turn and return it.
    ///
    /// Resets the current accumulators for the next turn.
    /// Only creates a turn if there is any transcript content.
    pub fn end_turn(&mut self) -> Option<TranscriptTurn> {
        if self.current_user.is_empty() && self.current_model.is_empty() {
            return None;
        }

        let turn = TranscriptTurn {
            turn_number: self.turn_count,
            user: std::mem::take(&mut self.current_user),
            model: std::mem::take(&mut self.current_model),
            timestamp: Instant::now(),
        };
        self.turn_count += 1;
        self.turns.push(turn.clone());
        Some(turn)
    }

    /// Get the last `n` completed turns.
    pub fn window(&self, n: usize) -> &[TranscriptTurn] {
        let start = self.turns.len().saturating_sub(n);
        &self.turns[start..]
    }

    /// All completed turns.
    pub fn all_turns(&self) -> &[TranscriptTurn] {
        &self.turns
    }

    /// Number of completed turns.
    pub fn turn_count(&self) -> u32 {
        self.turn_count
    }

    /// Format the last `n` turns as a human-readable transcript for LLM consumption.
    pub fn format_window(&self, n: usize) -> String {
        let window = self.window(n);
        let mut out = String::new();
        for turn in window {
            if !turn.user.is_empty() {
                let _ = writeln!(out, "User: {}", turn.user.trim());
            }
            if !turn.model.is_empty() {
                let _ = writeln!(out, "Assistant: {}", turn.model.trim());
            }
            let _ = writeln!(out);
        }
        out
    }

    /// Set server-provided input transcription for current turn.
    /// Overwrites client-accumulated input if server transcription is available.
    pub fn set_input_transcription(&mut self, text: &str) {
        self.current_user = text.to_string();
    }

    /// Set server-provided output transcription for current turn.
    pub fn set_output_transcription(&mut self, text: &str) {
        self.current_model = text.to_string();
    }

    /// Truncate the current model turn in progress. Called on interruption.
    /// Only what was already delivered to the client is retained.
    pub fn truncate_current_model_turn(&mut self) {
        self.current_model.clear();
    }

    /// Whether there is any pending (un-finalized) transcript content.
    pub fn has_pending(&self) -> bool {
        !self.current_user.is_empty() || !self.current_model.is_empty()
    }
}

impl Default for TranscriptBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_and_end_turn() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("Hello ");
        buf.push_input("there");
        buf.push_output("Hi! How can I help?");
        let turn = buf.end_turn().expect("should produce a turn");
        assert_eq!(turn.turn_number, 0);
        assert_eq!(turn.user, "Hello there");
        assert_eq!(turn.model, "Hi! How can I help?");
        assert_eq!(buf.turn_count(), 1);
    }

    #[test]
    fn end_turn_empty_returns_none() {
        let mut buf = TranscriptBuffer::new();
        assert!(buf.end_turn().is_none());
    }

    #[test]
    fn window_returns_last_n() {
        let mut buf = TranscriptBuffer::new();
        for i in 0..5 {
            buf.push_input(&format!("user-{i}"));
            buf.push_output(&format!("model-{i}"));
            buf.end_turn();
        }
        let w = buf.window(3);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].turn_number, 2);
        assert_eq!(w[1].turn_number, 3);
        assert_eq!(w[2].turn_number, 4);
    }

    #[test]
    fn window_larger_than_turns() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("only turn");
        buf.end_turn();
        let w = buf.window(10);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn format_window_produces_readable_text() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("What's the weather?");
        buf.push_output("It's sunny and 22 degrees.");
        buf.end_turn();
        buf.push_input("And tomorrow?");
        buf.push_output("Rain expected.");
        buf.end_turn();

        let formatted = buf.format_window(2);
        assert!(formatted.contains("User: What's the weather?"));
        assert!(formatted.contains("Assistant: It's sunny and 22 degrees."));
        assert!(formatted.contains("User: And tomorrow?"));
        assert!(formatted.contains("Assistant: Rain expected."));
    }

    #[test]
    fn has_pending() {
        let mut buf = TranscriptBuffer::new();
        assert!(!buf.has_pending());
        buf.push_input("hello");
        assert!(buf.has_pending());
        buf.end_turn();
        assert!(!buf.has_pending());
    }

    #[test]
    fn set_input_transcription_overwrites_accumulated() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("partial ");
        buf.push_input("input");
        // Server provides authoritative transcription
        buf.set_input_transcription("server transcription");
        let turn = buf.end_turn().expect("should produce a turn");
        assert_eq!(turn.user, "server transcription");
    }

    #[test]
    fn set_output_transcription_overwrites_accumulated() {
        let mut buf = TranscriptBuffer::new();
        buf.push_output("partial ");
        buf.push_output("output");
        // Server provides authoritative transcription
        buf.set_output_transcription("server output");
        let turn = buf.end_turn().expect("should produce a turn");
        assert_eq!(turn.model, "server output");
    }

    #[test]
    fn truncate_current_model_turn_clears_model_text() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("user said something");
        buf.push_output("model was saying something but got");
        // Interruption happens
        buf.truncate_current_model_turn();
        assert!(buf.has_pending()); // user text is still there
        let turn = buf.end_turn().expect("should produce a turn");
        assert_eq!(turn.user, "user said something");
        assert_eq!(turn.model, ""); // model output was truncated
    }

    #[test]
    fn multiple_turns_all_tracked() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("a");
        buf.end_turn();
        buf.push_output("b");
        buf.end_turn();
        buf.push_input("c");
        buf.push_output("d");
        buf.end_turn();
        assert_eq!(buf.all_turns().len(), 3);
        assert_eq!(buf.turn_count(), 3);
    }
}
