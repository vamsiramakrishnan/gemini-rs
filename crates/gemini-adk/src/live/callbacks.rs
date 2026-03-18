//! Typed callback registry for Live session events.
//!
//! Fast lane callbacks (sync, < 1ms): audio, text, transcripts, VAD.
//! Control lane callbacks (async, can block): tool calls, lifecycle, interruptions.
//! Outbound interceptors: transform tool responses, inject context at turn boundaries.
//!
//! # Callback Modes
//!
//! Each control-lane callback has an associated [`CallbackMode`]:
//!
//! - [`Blocking`](CallbackMode::Blocking) — awaited inline. The event loop
//!   waits for completion before processing the next event. Guarantees
//!   ordering and state consistency.
//! - [`Concurrent`](CallbackMode::Concurrent) — spawned as a detached tokio
//!   task. The event loop continues immediately. Use for fire-and-forget
//!   work (logging, background agent dispatch, analytics).
//!
//! Fast-lane callbacks (audio, text, VAD) are always sync and inline.
//! Interceptors (`before_tool_response`, `on_turn_boundary`) are always blocking.
//!
//! Some control-lane callbacks are forced-blocking (no concurrent variant):
//! `on_interrupted` (must clear state before audio resumes),
//! `on_tool_call` (return value is the tool response).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gemini_live::prelude::{FunctionCall, FunctionResponse, SessionPhase, UsageMetadata};
use gemini_live::session::SessionWriter;

use super::BoxFuture;
use crate::state::State;

/// Controls how a control-lane callback is executed relative to the event loop.
///
/// Each control-lane callback in [`EventCallbacks`] has a companion `_mode` field
/// (e.g., `on_turn_complete_mode`) that determines execution semantics.
///
/// At the L2 fluent API level, use `_concurrent` suffixed methods (e.g.,
/// `on_turn_complete_concurrent()`) to set both the callback and its mode
/// in a single call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallbackMode {
    /// Callback is awaited inline — the event loop waits for completion.
    ///
    /// Use when subsequent events depend on the callback's side effects,
    /// or when ordering guarantees are required.
    #[default]
    Blocking,
    /// Callback is spawned as a concurrent task — the event loop continues immediately.
    ///
    /// Use for fire-and-forget work: logging, analytics, webhook dispatch,
    /// background agent triggering. The callback runs in a detached tokio task.
    Concurrent,
}

/// Typed callback registry for Live session events.
///
/// Callbacks are divided into two lanes:
/// - **Fast lane** (sync): Called inline, must be < 1ms. For audio, text, transcripts, VAD.
/// - **Control lane** (async): Awaited on a dedicated task. For tool calls, lifecycle, interruptions.
pub struct EventCallbacks {
    // -- Fast lane (sync callbacks) --
    /// Called for each audio chunk from the model (PCM16 24kHz).
    pub on_audio: Option<Box<dyn Fn(&Bytes) + Send + Sync>>,
    /// Called for each incremental text delta from the model.
    pub on_text: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called when the model completes a text response.
    pub on_text_complete: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called for input (user speech) transcription updates.
    pub on_input_transcript: Option<Box<dyn Fn(&str, bool) + Send + Sync>>,
    /// Called for output (model speech) transcription updates.
    pub on_output_transcript: Option<Box<dyn Fn(&str, bool) + Send + Sync>>,
    /// Called when the model emits a thought/reasoning summary (when includeThoughts is enabled).
    pub on_thought: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called when server-side VAD detects voice activity start.
    pub on_vad_start: Option<Box<dyn Fn() + Send + Sync>>,
    /// Called when server-side VAD detects voice activity end.
    pub on_vad_end: Option<Box<dyn Fn() + Send + Sync>>,
    /// Called on session phase transitions.
    pub on_phase: Option<Box<dyn Fn(SessionPhase) + Send + Sync>>,
    /// Called when server sends token usage metadata.
    pub on_usage: Option<Box<dyn Fn(&UsageMetadata) + Send + Sync>>,

    // -- Control lane (async callbacks) --
    /// Called when the model is interrupted by barge-in.
    pub on_interrupted: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called when model requests tool execution.
    /// Return `None` to use auto-dispatch (ToolDispatcher), `Some` to override.
    /// Receives State for natural state promotion from tool results.
    pub on_tool_call: Option<
        Arc<
            dyn Fn(Vec<FunctionCall>, State) -> BoxFuture<Option<Vec<FunctionResponse>>>
                + Send
                + Sync,
        >,
    >,
    /// Called when server cancels pending tool calls.
    pub on_tool_cancelled: Option<Arc<dyn Fn(Vec<String>) -> BoxFuture<()> + Send + Sync>>,
    /// Called when the model completes its turn.
    pub on_turn_complete: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called when server sends GoAway (session ending soon).
    pub on_go_away: Option<Arc<dyn Fn(Duration) -> BoxFuture<()> + Send + Sync>>,
    /// Called when session setup completes (connected).
    ///
    /// Receives a `SessionWriter` for sending messages on connect (e.g. greeting prompts).
    pub on_connected: Option<Arc<dyn Fn(Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    /// Called when session disconnects.
    pub on_disconnected: Option<Arc<dyn Fn(Option<String>) -> BoxFuture<()> + Send + Sync>>,
    /// Called after session resumes from GoAway.
    pub on_resumed: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called on non-fatal errors.
    pub on_error: Option<Arc<dyn Fn(String) -> BoxFuture<()> + Send + Sync>>,
    /// Called when agent transfer occurs (from, to).
    pub on_transfer: Option<Arc<dyn Fn(String, String) -> BoxFuture<()> + Send + Sync>>,
    /// Called when a TurnExtractor produces a result (extractor_name, value).
    pub on_extracted: Option<Arc<dyn Fn(String, serde_json::Value) -> BoxFuture<()> + Send + Sync>>,
    /// Called when a TurnExtractor fails (extractor_name, error_message).
    ///
    /// By default, extraction failures are logged via `tracing::warn!`.
    /// Register this callback to implement custom error handling (retry, alert, etc.).
    pub on_extraction_error: Option<Arc<dyn Fn(String, String) -> BoxFuture<()> + Send + Sync>>,

    // -- Callback modes (control-lane only) --
    /// Execution mode for [`on_turn_complete`](Self::on_turn_complete).
    pub on_turn_complete_mode: CallbackMode,
    /// Execution mode for [`on_connected`](Self::on_connected).
    pub on_connected_mode: CallbackMode,
    /// Execution mode for [`on_disconnected`](Self::on_disconnected).
    pub on_disconnected_mode: CallbackMode,
    /// Execution mode for [`on_error`](Self::on_error).
    pub on_error_mode: CallbackMode,
    /// Execution mode for [`on_go_away`](Self::on_go_away).
    pub on_go_away_mode: CallbackMode,
    /// Execution mode for [`on_extracted`](Self::on_extracted).
    pub on_extracted_mode: CallbackMode,
    /// Execution mode for [`on_extraction_error`](Self::on_extraction_error).
    pub on_extraction_error_mode: CallbackMode,
    /// Execution mode for [`on_tool_cancelled`](Self::on_tool_cancelled).
    pub on_tool_cancelled_mode: CallbackMode,
    /// Execution mode for [`on_transfer`](Self::on_transfer).
    pub on_transfer_mode: CallbackMode,
    /// Execution mode for [`on_resumed`](Self::on_resumed).
    pub on_resumed_mode: CallbackMode,

    // -- Outbound interceptors (transform data going to Gemini) --
    /// Intercept tool responses before sending to Gemini.
    ///
    /// Receives the tool responses and shared State. Returns (potentially modified)
    /// responses. Use this to rewrite, augment, or filter tool results based on
    /// conversation state.
    pub before_tool_response: Option<
        Arc<dyn Fn(Vec<FunctionResponse>, State) -> BoxFuture<Vec<FunctionResponse>> + Send + Sync>,
    >,

    /// Called at turn boundaries (after extractors, before `on_turn_complete`).
    ///
    /// Receives shared State and a SessionWriter for injecting content into
    /// the conversation. Use this for context stuffing, K/V injection, condensed
    /// state summaries, or any outbound content interleaving.
    pub on_turn_boundary:
        Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,

    /// State-reactive system instruction template (full replacement).
    ///
    /// Called after extractors run on each TurnComplete. If it returns
    /// `Some(instruction)`, the system instruction is updated mid-session.
    /// Returns `None` to leave the instruction unchanged.
    ///
    /// This is sync (no async) because instruction generation should be fast.
    pub instruction_template: Option<Arc<dyn Fn(&State) -> Option<String> + Send + Sync>>,

    /// State-reactive instruction amendment (additive, not replacement).
    ///
    /// Called after extractors and phase transitions on each TurnComplete.
    /// If it returns `Some(text)`, the text is appended to the current phase
    /// instruction (separated by `\n\n`). Returns `None` to skip amendment.
    ///
    /// Unlike `instruction_template` (which replaces the entire instruction),
    /// this only adds to the phase instruction — the developer never needs to
    /// know or repeat the base instruction.
    pub instruction_amendment: Option<Arc<dyn Fn(&State) -> Option<String> + Send + Sync>>,
}

impl Default for EventCallbacks {
    fn default() -> Self {
        Self {
            on_audio: None,
            on_text: None,
            on_text_complete: None,
            on_input_transcript: None,
            on_output_transcript: None,
            on_thought: None,
            on_vad_start: None,
            on_vad_end: None,
            on_phase: None,
            on_usage: None,
            on_interrupted: None,
            on_tool_call: None,
            on_tool_cancelled: None,
            on_turn_complete: None,
            on_go_away: None,
            on_connected: None,
            on_disconnected: None,
            on_resumed: None,
            on_error: None,
            on_transfer: None,
            on_extracted: None,
            on_extraction_error: None,
            on_turn_complete_mode: CallbackMode::Blocking,
            on_connected_mode: CallbackMode::Blocking,
            on_disconnected_mode: CallbackMode::Blocking,
            on_error_mode: CallbackMode::Blocking,
            on_go_away_mode: CallbackMode::Blocking,
            on_extracted_mode: CallbackMode::Blocking,
            on_extraction_error_mode: CallbackMode::Blocking,
            on_tool_cancelled_mode: CallbackMode::Blocking,
            on_transfer_mode: CallbackMode::Blocking,
            on_resumed_mode: CallbackMode::Blocking,
            before_tool_response: None,
            on_turn_boundary: None,
            instruction_template: None,
            instruction_amendment: None,
        }
    }
}

impl std::fmt::Debug for EventCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventCallbacks")
            .field("on_audio", &self.on_audio.is_some())
            .field("on_text", &self.on_text.is_some())
            .field("on_text_complete", &self.on_text_complete.is_some())
            .field("on_input_transcript", &self.on_input_transcript.is_some())
            .field("on_output_transcript", &self.on_output_transcript.is_some())
            .field("on_thought", &self.on_thought.is_some())
            .field("on_vad_start", &self.on_vad_start.is_some())
            .field("on_vad_end", &self.on_vad_end.is_some())
            .field("on_phase", &self.on_phase.is_some())
            .field("on_usage", &self.on_usage.is_some())
            .field("on_interrupted", &self.on_interrupted.is_some())
            .field("on_tool_call", &self.on_tool_call.is_some())
            .field("on_tool_cancelled", &self.on_tool_cancelled.is_some())
            .field("on_turn_complete", &self.on_turn_complete.is_some())
            .field("on_go_away", &self.on_go_away.is_some())
            .field("on_connected", &self.on_connected.is_some())
            .field("on_disconnected", &self.on_disconnected.is_some())
            .field("on_resumed", &self.on_resumed.is_some())
            .field("on_error", &self.on_error.is_some())
            .field("on_transfer", &self.on_transfer.is_some())
            .field("on_extracted", &self.on_extracted.is_some())
            .field("on_extraction_error", &self.on_extraction_error.is_some())
            .field("on_turn_complete_mode", &self.on_turn_complete_mode)
            .field("on_connected_mode", &self.on_connected_mode)
            .field("on_disconnected_mode", &self.on_disconnected_mode)
            .field("on_error_mode", &self.on_error_mode)
            .field("on_go_away_mode", &self.on_go_away_mode)
            .field("on_extracted_mode", &self.on_extracted_mode)
            .field("on_extraction_error_mode", &self.on_extraction_error_mode)
            .field("on_tool_cancelled_mode", &self.on_tool_cancelled_mode)
            .field("on_transfer_mode", &self.on_transfer_mode)
            .field("on_resumed_mode", &self.on_resumed_mode)
            .field("before_tool_response", &self.before_tool_response.is_some())
            .field("on_turn_boundary", &self.on_turn_boundary.is_some())
            .field("instruction_template", &self.instruction_template.is_some())
            .field(
                "instruction_amendment",
                &self.instruction_amendment.is_some(),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_callbacks_all_none() {
        let cb = EventCallbacks::default();
        assert!(cb.on_audio.is_none());
        assert!(cb.on_text.is_none());
        assert!(cb.on_interrupted.is_none());
        assert!(cb.on_tool_call.is_none());
    }

    #[test]
    fn sync_callback_callable() {
        let mut cb = EventCallbacks::default();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        cb.on_text = Some(Box::new(move |_text| {
            called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        if let Some(f) = &cb.on_text {
            f("hello");
        }
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn callback_mode_defaults_to_blocking() {
        let cb = EventCallbacks::default();
        assert_eq!(cb.on_turn_complete_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_connected_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_disconnected_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_error_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_go_away_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_extracted_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_extraction_error_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_tool_cancelled_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_transfer_mode, CallbackMode::Blocking);
        assert_eq!(cb.on_resumed_mode, CallbackMode::Blocking);
    }

    #[test]
    fn debug_shows_registered() {
        let mut cb = EventCallbacks::default();
        cb.on_audio = Some(Box::new(|_| {}));
        let debug = format!("{:?}", cb);
        assert!(debug.contains("on_audio: true"));
        assert!(debug.contains("on_text: false"));
    }
}
