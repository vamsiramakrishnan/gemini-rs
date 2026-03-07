//! Transcript accumulation buffer for Live sessions.
//!
//! Automatically accumulates input/output transcripts per-turn with windowing
//! support for OOB extraction pipelines.

use std::collections::VecDeque;
use std::fmt::Write;
use std::time::Instant;

/// Summary of a tool call within a conversation turn.
#[derive(Debug, Clone)]
pub struct ToolCallSummary {
    /// Name of the tool that was called.
    pub name: String,
    /// First 200 chars of JSON args.
    pub args_summary: String,
    /// First 200 chars of JSON result.
    pub result_summary: String,
}

/// A single completed conversation turn with accumulated transcripts.
#[derive(Debug, Clone)]
pub struct TranscriptTurn {
    /// Sequential turn number (0-based).
    pub turn_number: u32,
    /// Accumulated user (input) transcript for this turn.
    pub user: String,
    /// Accumulated model (output) transcript for this turn.
    pub model: String,
    /// Tool calls that occurred during this turn.
    pub tool_calls: Vec<ToolCallSummary>,
    /// When this turn was finalized.
    pub timestamp: Instant,
}

/// Default maximum number of completed turns retained in the ring buffer.
const DEFAULT_MAX_TURNS: usize = 50;

/// Accumulates input/output transcripts and segments them by turn boundaries.
///
/// Uses a ring buffer (`VecDeque`) that evicts the oldest turns when
/// `max_turns` is reached. This prevents unbounded memory growth in
/// long-running voice sessions.
///
/// Thread safety: wrap in `Arc<parking_lot::Mutex<TranscriptBuffer>>` when
/// sharing between fast lane (push) and control lane (end_turn / window).
#[derive(Debug)]
pub struct TranscriptBuffer {
    turns: VecDeque<TranscriptTurn>,
    current_user: String,
    current_model: String,
    tool_calls_pending: Vec<ToolCallSummary>,
    turn_count: u32,
    max_turns: usize,
}

/// Truncate a string to at most `max_chars` characters, reusing the original when possible.
fn truncate_string(mut s: String, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s; // fast path: ASCII strings under limit
    }
    // Find the byte index of the max_chars-th char boundary
    if let Some((idx, _)) = s.char_indices().nth(max_chars) {
        s.truncate(idx);
    }
    s
}

impl TranscriptBuffer {
    /// Create a new transcript buffer with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_TURNS)
    }

    /// Create a buffer with a custom maximum turn capacity.
    ///
    /// When the buffer reaches `max_turns` completed turns, the oldest
    /// turn is evicted on each new `end_turn()`.
    pub fn with_capacity(max_turns: usize) -> Self {
        Self {
            turns: VecDeque::with_capacity(max_turns.min(64)),
            current_user: String::new(),
            current_model: String::new(),
            tool_calls_pending: Vec::new(),
            turn_count: 0,
            max_turns,
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

    /// Record a tool call summary for the current turn.
    ///
    /// Args and result are truncated to 200 characters of their JSON representation.
    pub fn push_tool_call(
        &mut self,
        name: String,
        args: &serde_json::Value,
        result: &serde_json::Value,
    ) {
        let args_str = serde_json::to_string(args).unwrap_or_default();
        let result_str = serde_json::to_string(result).unwrap_or_default();
        self.tool_calls_pending.push(ToolCallSummary {
            name,
            args_summary: truncate_string(args_str, 200),
            result_summary: truncate_string(result_str, 200),
        });
    }

    /// Finalize the current turn and return it.
    ///
    /// Resets the current accumulators for the next turn.
    /// Only creates a turn if there is any transcript content.
    pub fn end_turn(&mut self) -> Option<TranscriptTurn> {
        if self.current_user.is_empty()
            && self.current_model.is_empty()
            && self.tool_calls_pending.is_empty()
        {
            return None;
        }

        let turn = TranscriptTurn {
            turn_number: self.turn_count,
            user: std::mem::take(&mut self.current_user),
            model: std::mem::take(&mut self.current_model),
            tool_calls: std::mem::take(&mut self.tool_calls_pending),
            timestamp: Instant::now(),
        };
        self.turn_count += 1;
        // Evict oldest turn if at capacity
        if self.turns.len() >= self.max_turns {
            self.turns.pop_front();
        }
        self.turns.push_back(turn);
        Some(self.turns.back().unwrap().clone())
    }

    /// Get the last `n` completed turns as a contiguous slice.
    ///
    /// Requires `&mut self` to ensure VecDeque contiguity.
    pub fn window(&mut self, n: usize) -> &[TranscriptTurn] {
        let slice = self.turns.make_contiguous();
        let start = slice.len().saturating_sub(n);
        &slice[start..]
    }

    /// All completed turns as a contiguous slice.
    ///
    /// Requires `&mut self` to ensure VecDeque contiguity.
    pub fn all_turns(&mut self) -> &[TranscriptTurn] {
        self.turns.make_contiguous()
    }

    /// Number of retained turns (may be less than `turn_count` due to eviction).
    pub fn retained_count(&self) -> usize {
        self.turns.len()
    }

    /// Number of completed turns.
    pub fn turn_count(&self) -> u32 {
        self.turn_count
    }

    /// Format the last `n` turns as a human-readable transcript for LLM consumption.
    pub fn format_window(&mut self, n: usize) -> String {
        let window = self.window(n);
        let mut out = String::new();
        for turn in window {
            if !turn.user.is_empty() {
                let _ = writeln!(out, "User: {}", turn.user.trim());
            }
            for tc in &turn.tool_calls {
                let _ = writeln!(
                    out,
                    "[Tool: {}({}) \u{2192} {}]",
                    tc.name, tc.args_summary, tc.result_summary
                );
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
        self.current_user.clear();
        self.current_user.push_str(text);
    }

    /// Set server-provided output transcription for current turn.
    pub fn set_output_transcription(&mut self, text: &str) {
        self.current_model.clear();
        self.current_model.push_str(text);
    }

    /// Truncate the current model turn in progress. Called on interruption.
    /// Only what was already delivered to the client is retained.
    pub fn truncate_current_model_turn(&mut self) {
        self.current_model.clear();
    }

    /// Whether there is any pending (un-finalized) transcript content.
    pub fn has_pending(&self) -> bool {
        !self.current_user.is_empty()
            || !self.current_model.is_empty()
            || !self.tool_calls_pending.is_empty()
    }

    /// Create a `TranscriptWindow` snapshot of the last `n` completed turns.
    ///
    /// This is a cheap clone operation designed for passing to phase callbacks.
    pub fn snapshot_window(&mut self, n: usize) -> TranscriptWindow {
        TranscriptWindow::new(self.window(n).to_vec())
    }

    /// Snapshot including the current in-progress turn (not yet finalized).
    ///
    /// Used by `GenerationComplete` extractors to see the model's full output
    /// before interruption truncation clears `current_model`.
    pub fn snapshot_window_with_current(&mut self, n: usize) -> TranscriptWindow {
        let mut turns: Vec<TranscriptTurn> = self.window(n).to_vec();
        if self.has_pending() {
            turns.push(TranscriptTurn {
                turn_number: self.turn_count,
                user: self.current_user.clone(),
                model: self.current_model.clone(),
                tool_calls: self.tool_calls_pending.clone(),
                timestamp: std::time::Instant::now(),
            });
        }
        TranscriptWindow::new(turns)
    }
}

/// A read-only snapshot of recent transcript turns for context construction.
///
/// Cheap to create (clone of ~5 small structs). Used by `on_enter_context`
/// callbacks to reference recent conversation without holding the buffer lock.
#[derive(Debug, Clone)]
pub struct TranscriptWindow {
    turns: Vec<TranscriptTurn>,
}

impl TranscriptWindow {
    /// Create a window from a vec of turns.
    pub fn new(turns: Vec<TranscriptTurn>) -> Self {
        Self { turns }
    }

    /// The turns in this window.
    pub fn turns(&self) -> &[TranscriptTurn] {
        &self.turns
    }

    /// Format all turns as human-readable text for LLM consumption.
    pub fn formatted(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for turn in &self.turns {
            if !turn.user.is_empty() {
                let _ = writeln!(out, "User: {}", turn.user.trim());
            }
            for tc in &turn.tool_calls {
                let _ = writeln!(
                    out,
                    "[Tool: {}({}) \u{2192} {}]",
                    tc.name, tc.args_summary, tc.result_summary
                );
            }
            if !turn.model.is_empty() {
                let _ = writeln!(out, "Assistant: {}", turn.model.trim());
            }
            let _ = writeln!(out);
        }
        out
    }

    /// Last user utterance, if any.
    pub fn last_user(&self) -> Option<&str> {
        self.turns
            .iter()
            .rev()
            .find(|t| !t.user.is_empty())
            .map(|t| t.user.as_str())
    }

    /// Last model utterance, if any.
    pub fn last_model(&self) -> Option<&str> {
        self.turns
            .iter()
            .rev()
            .find(|t| !t.model.is_empty())
            .map(|t| t.model.as_str())
    }

    /// Number of turns in this window.
    pub fn len(&self) -> usize {
        self.turns.len()
    }

    /// Whether the window is empty.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
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

    #[test]
    fn push_tool_call_records_summary() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("check weather");
        buf.push_tool_call(
            "get_weather".to_string(),
            &serde_json::json!({"city": "London"}),
            &serde_json::json!({"temp": 22, "condition": "sunny"}),
        );
        buf.push_output("It's sunny in London.");
        let turn = buf.end_turn().expect("should produce a turn");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "get_weather");
        assert!(turn.tool_calls[0].args_summary.contains("London"));
        assert!(turn.tool_calls[0].result_summary.contains("sunny"));
    }

    #[test]
    fn push_tool_call_truncates_long_args() {
        let mut buf = TranscriptBuffer::new();
        let long_value = "x".repeat(500);
        buf.push_input("do something");
        buf.push_tool_call(
            "big_tool".to_string(),
            &serde_json::json!({"data": long_value}),
            &serde_json::json!({"ok": true}),
        );
        let turn = buf.end_turn().expect("should produce a turn");
        assert!(turn.tool_calls[0].args_summary.chars().count() <= 200);
    }

    #[test]
    fn multiple_tool_calls_in_one_turn() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("plan my trip");
        buf.push_tool_call(
            "get_weather".to_string(),
            &serde_json::json!({"city": "Paris"}),
            &serde_json::json!({"temp": 18}),
        );
        buf.push_tool_call(
            "get_flights".to_string(),
            &serde_json::json!({"from": "NYC", "to": "Paris"}),
            &serde_json::json!({"price": 450}),
        );
        buf.push_output("Here's your trip plan.");
        let turn = buf.end_turn().expect("should produce a turn");
        assert_eq!(turn.tool_calls.len(), 2);
        assert_eq!(turn.tool_calls[0].name, "get_weather");
        assert_eq!(turn.tool_calls[1].name, "get_flights");
    }

    #[test]
    fn tool_calls_appear_in_format_window() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("What's the weather?");
        buf.push_tool_call(
            "get_weather".to_string(),
            &serde_json::json!({"city": "London"}),
            &serde_json::json!({"temp": 22}),
        );
        buf.push_output("It's 22 degrees in London.");
        buf.end_turn();

        let formatted = buf.format_window(1);
        assert!(formatted.contains("User: What's the weather?"));
        assert!(formatted.contains("[Tool: get_weather("));
        assert!(formatted.contains("London"));
        assert!(formatted.contains("\u{2192}"));
        assert!(formatted.contains("22"));
        assert!(formatted.contains("Assistant: It's 22 degrees in London."));
    }

    #[test]
    fn tool_call_only_turn_creates_turn() {
        let mut buf = TranscriptBuffer::new();
        // A turn with only a tool call and no user/model text
        buf.push_tool_call(
            "ping".to_string(),
            &serde_json::json!({}),
            &serde_json::json!({"pong": true}),
        );
        assert!(buf.has_pending());
        let turn = buf
            .end_turn()
            .expect("tool-call-only turn should be created");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.user, "");
        assert_eq!(turn.model, "");
    }

    #[test]
    fn snapshot_window_creates_window() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("Hello");
        buf.push_output("Hi there!");
        buf.end_turn();
        buf.push_input("How are you?");
        buf.push_output("I'm good!");
        buf.end_turn();

        let window = buf.snapshot_window(5);
        assert_eq!(window.len(), 2);
        assert_eq!(window.last_user(), Some("How are you?"));
        assert_eq!(window.last_model(), Some("I'm good!"));
        assert!(!window.is_empty());
    }

    #[test]
    fn transcript_window_formatted() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("What's the weather?");
        buf.push_output("It's sunny.");
        buf.end_turn();

        let window = buf.snapshot_window(1);
        let formatted = window.formatted();
        assert!(formatted.contains("User: What's the weather?"));
        assert!(formatted.contains("Assistant: It's sunny."));
    }

    #[test]
    fn transcript_window_empty() {
        let mut buf = TranscriptBuffer::new();
        let window = buf.snapshot_window(5);
        assert!(window.is_empty());
        assert_eq!(window.len(), 0);
        assert_eq!(window.last_user(), None);
        assert_eq!(window.last_model(), None);
    }

    #[test]
    fn ring_cap_evicts_oldest() {
        let mut buf = TranscriptBuffer::with_capacity(3);
        for i in 0..5 {
            buf.push_input(&format!("user-{i}"));
            buf.push_output(&format!("model-{i}"));
            buf.end_turn();
        }
        // Only last 3 retained
        assert_eq!(buf.retained_count(), 3);
        assert_eq!(buf.turn_count(), 5);
        let turns = buf.all_turns();
        assert_eq!(turns[0].turn_number, 2);
        assert_eq!(turns[1].turn_number, 3);
        assert_eq!(turns[2].turn_number, 4);
    }

    #[test]
    fn ring_cap_window_within_retained() {
        let mut buf = TranscriptBuffer::with_capacity(4);
        for i in 0..10 {
            buf.push_input(&format!("u{i}"));
            buf.end_turn();
        }
        let w = buf.window(2);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].turn_number, 8);
        assert_eq!(w[1].turn_number, 9);
    }

    #[test]
    fn default_capacity_is_50() {
        let buf = TranscriptBuffer::new();
        assert_eq!(buf.max_turns, DEFAULT_MAX_TURNS);
    }

    #[test]
    fn tool_calls_reset_after_end_turn() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("turn 1");
        buf.push_tool_call(
            "tool_a".to_string(),
            &serde_json::json!({"x": 1}),
            &serde_json::json!({"y": 2}),
        );
        buf.end_turn();

        buf.push_input("turn 2");
        buf.push_output("no tools this time");
        let turn2 = buf.end_turn().expect("should produce turn 2");
        assert!(turn2.tool_calls.is_empty());

        // Verify turn 1 still has its tool call
        assert_eq!(buf.all_turns()[0].tool_calls.len(), 1);
    }
}
