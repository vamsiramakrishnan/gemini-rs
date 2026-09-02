//! Typed callback registry for Live session events.
//!
//! Fast lane callbacks (sync, < 1ms): audio, text, transcripts, VAD.
//! Control lane callbacks (async, can block): tool calls, lifecycle, interruptions.
//! Outbound interceptors: transform tool responses, inject context at turn boundaries.
//!
//! # Callback Modes
//!
//! Each control-lane callback has an associated [`ExecutionMode`]:
//!
//! - [`Blocking`](ExecutionMode::Blocking) — awaited inline. The event loop
//!   waits for completion before processing the next event. Guarantees
//!   ordering and state consistency.
//! - [`Concurrent`](ExecutionMode::Concurrent) — spawned as a detached tokio
//!   task. The event loop continues immediately. Use for fire-and-forget
//!   work (logging, background agent dispatch, analytics).
//!
//! Fast-lane callbacks (audio, text, VAD) are always sync and inline.
//! Interceptors (`before_tool_response`, `on_turn_boundary`) are always blocking.
//!
//! `on_interrupted`, `on_turn_boundary`, and the `on_teardown` hooks default to
//! blocking for a reason — audio forwarding resumes only after `on_interrupted`
//! returns, the next turn proceeds only after `on_turn_boundary`, and disconnect
//! completes only after teardown — but each can be made concurrent when the
//! body is pure bookkeeping (see the `_mode` fields and `on_teardown_concurrent`).
//! `on_tool_call` and `before_tool_response` are always blocking: their return
//! value is the tool response.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gemini_genai_rs::prelude::{FunctionCall, FunctionResponse, SessionPhase, UsageMetadata};
use gemini_genai_rs::session::SessionWriter;

use super::{BoxFuture, ExecutionMode};
use crate::state::State;

// ── Named callback types ──────────────────────────────────────────────────
// These aliases are the vocabulary of the callback registry: every field
// below (and the corresponding L2 setters) is one of these shapes.

/// Fast-lane sync callback over a raw audio chunk.
pub type AudioCallback = Box<dyn Fn(&Bytes) + Send + Sync>;
/// Fast-lane sync callback over a text payload (delta, accumulated text, thought).
pub type TextCallback = Box<dyn Fn(&str) + Send + Sync>;
/// Fast-lane sync callback over a transcript chunk with its `is_final` flag.
pub type TranscriptCallback = Box<dyn Fn(&str, bool) + Send + Sync>;
/// Fast-lane sync callback with no payload (VAD start/end).
pub type SignalCallback = Box<dyn Fn() + Send + Sync>;
/// Fast-lane sync callback over a wire-level [`SessionPhase`] change
/// (connecting → active → disconnecting …). Not the `PhaseMachine`.
pub type SessionPhaseCallback = Box<dyn Fn(SessionPhase) + Send + Sync>;
/// Fast-lane sync callback over usage metadata.
pub type UsageCallback = Box<dyn Fn(&UsageMetadata) + Send + Sync>;

/// Control-lane async callback with no payload.
pub type AsyncCallback = Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>;
/// Control-lane async callback over one payload value.
pub type AsyncCallbackWith<T> = Arc<dyn Fn(T) -> BoxFuture<()> + Send + Sync>;
/// Control-lane async callback over two payload values.
pub type AsyncCallbackWith2<A, B> = Arc<dyn Fn(A, B) -> BoxFuture<()> + Send + Sync>;
/// Tool-call override: return `Some(responses)` to reply, `None` to defer to
/// auto-dispatch via the registered `ToolDispatcher`.
pub type ToolCallCallback =
    Arc<dyn Fn(Vec<FunctionCall>, State) -> BoxFuture<Option<Vec<FunctionResponse>>> + Send + Sync>;
/// Middleware over outgoing tool responses (inspect/rewrite before send).
pub type BeforeToolResponseCallback =
    Arc<dyn Fn(Vec<FunctionResponse>, State) -> BoxFuture<Vec<FunctionResponse>> + Send + Sync>;
/// Sync state-reactive instruction generator (`None` = leave unchanged).
pub type InstructionFn = Arc<dyn Fn(&State) -> Option<String> + Send + Sync>;

/// Typed callback registry for Live session events.
///
/// Callbacks are divided into two lanes:
/// - **Fast lane** (sync): Called inline, must be < 1ms. For audio, text, transcripts, VAD.
/// - **Control lane** (async): Awaited on a dedicated task. For tool calls, lifecycle, interruptions.
pub struct EventCallbacks {
    // -- Fast lane (sync callbacks) --
    /// Called for each audio chunk from the model (PCM16 24kHz).
    pub on_audio: Option<AudioCallback>,
    /// Called for each incremental text delta from the model.
    pub on_text: Option<TextCallback>,
    /// Called when the model completes a text response.
    pub on_text_complete: Option<TextCallback>,
    /// Called for input (user speech) transcription updates.
    pub on_input_transcript: Option<TranscriptCallback>,
    /// Called for output (model speech) transcription updates.
    pub on_output_transcript: Option<TranscriptCallback>,
    /// Called when the model emits a thought/reasoning summary (when includeThoughts is enabled).
    pub on_thought: Option<TextCallback>,
    /// Called when server-side VAD detects voice activity start.
    pub on_vad_start: Option<SignalCallback>,
    /// Called when server-side VAD detects voice activity end.
    pub on_vad_end: Option<SignalCallback>,
    /// Called on session phase transitions.
    pub on_session_phase: Option<SessionPhaseCallback>,
    /// Called when server sends token usage metadata.
    pub on_usage: Option<UsageCallback>,

    // -- Control lane (async callbacks) --
    /// Called when the model is interrupted by barge-in.
    pub on_interrupted: Option<AsyncCallback>,
    /// Called when model requests tool execution.
    /// Return `None` to use auto-dispatch (ToolDispatcher), `Some` to override.
    /// Receives State for natural state promotion from tool results.
    pub on_tool_call: Option<ToolCallCallback>,
    /// Called when server cancels pending tool calls.
    pub on_tool_cancelled: Option<AsyncCallbackWith<Vec<String>>>,
    /// Called when the model completes its turn.
    pub on_turn_complete: Option<AsyncCallback>,
    /// Called when the model finishes generating its full intended response,
    /// before any interruption truncation (the wire `GenerationComplete`).
    pub on_generation_complete: Option<AsyncCallback>,
    /// Called when server sends GoAway (session ending soon).
    pub on_go_away: Option<AsyncCallbackWith<Duration>>,
    /// Called when session setup completes (connected).
    ///
    /// Receives a `SessionWriter` for sending messages on connect (e.g. greeting prompts).
    pub on_connected: Option<AsyncCallbackWith<Arc<dyn SessionWriter>>>,
    /// Called when session disconnects.
    pub on_disconnected: Option<AsyncCallbackWith<Option<String>>>,
    /// Teardown hooks run on disconnect, **before** `on_disconnected`.
    ///
    /// Additive, unlike every other callback here. Each of those is a single
    /// `Option` that the last registration silently replaces, which is fine for
    /// an application — it has one place to write its handler — and unusable for
    /// an extension: `.with_memory(s).on_disconnected(f)` would drop the
    /// extension's hook, and the reverse order would drop the application's,
    /// with nothing reporting either.
    ///
    /// Extensions that must flush durable state at end of session register here
    /// (`gemini-memory-rs` reconciles its session ledger this way). Hooks run in
    /// registration order and are awaited before `on_disconnected` fires, so the
    /// application's own handler observes a settled world. A hook that panics or
    /// hangs delays disconnect — keep them bounded.
    pub on_teardown: Vec<AsyncCallback>,
    /// Teardown hooks that are spawned detached on disconnect rather than
    /// awaited — for bookkeeping that must not delay the disconnect (metrics,
    /// a final log line). Anything that flushes durable state belongs in
    /// [`on_teardown`](Self::on_teardown).
    pub on_teardown_concurrent: Vec<AsyncCallback>,
    /// Called after session resumes from GoAway.
    pub on_resumed: Option<AsyncCallback>,
    /// Called on non-fatal errors.
    pub on_error: Option<AsyncCallbackWith<String>>,
    /// Called when agent transfer occurs (from, to).
    pub on_transfer: Option<AsyncCallbackWith2<String, String>>,
    /// Called when a TurnExtractor produces a result (extractor_name, value).
    pub on_extracted: Option<AsyncCallbackWith2<String, serde_json::Value>>,
    /// Called when a TurnExtractor fails (extractor_name, error_message).
    ///
    /// By default, extraction failures are logged via `tracing::warn!`.
    /// Register this callback to implement custom error handling (retry, alert, etc.).
    pub on_extraction_error: Option<AsyncCallbackWith2<String, String>>,

    // -- Callback modes (control-lane only) --
    /// Execution mode for [`on_interrupted`](Self::on_interrupted). Blocking
    /// by default: audio forwarding resumes only after the callback returns,
    /// which is what a playback flush needs. Concurrent is for bookkeeping only.
    pub on_interrupted_mode: ExecutionMode,
    /// Execution mode for [`on_turn_boundary`](Self::on_turn_boundary).
    /// Blocking by default so injected context lands before the next turn;
    /// concurrent is for observation only.
    pub on_turn_boundary_mode: ExecutionMode,
    /// Execution mode for [`on_turn_complete`](Self::on_turn_complete).
    pub on_turn_complete_mode: ExecutionMode,
    /// Execution mode for [`on_generation_complete`](Self::on_generation_complete).
    pub on_generation_complete_mode: ExecutionMode,
    /// Execution mode for [`on_connected`](Self::on_connected).
    pub on_connected_mode: ExecutionMode,
    /// Execution mode for [`on_disconnected`](Self::on_disconnected).
    pub on_disconnected_mode: ExecutionMode,
    /// Execution mode for [`on_error`](Self::on_error).
    pub on_error_mode: ExecutionMode,
    /// Execution mode for [`on_go_away`](Self::on_go_away).
    pub on_go_away_mode: ExecutionMode,
    /// Execution mode for [`on_extracted`](Self::on_extracted).
    pub on_extracted_mode: ExecutionMode,
    /// Execution mode for [`on_extraction_error`](Self::on_extraction_error).
    pub on_extraction_error_mode: ExecutionMode,
    /// Execution mode for [`on_tool_cancelled`](Self::on_tool_cancelled).
    pub on_tool_cancelled_mode: ExecutionMode,
    /// Execution mode for [`on_transfer`](Self::on_transfer).
    pub on_transfer_mode: ExecutionMode,
    /// Execution mode for [`on_resumed`](Self::on_resumed).
    pub on_resumed_mode: ExecutionMode,

    // -- Outbound interceptors (transform data going to Gemini) --
    /// Intercept tool responses before sending to Gemini.
    ///
    /// Receives the tool responses and shared State. Returns (potentially modified)
    /// responses. Use this to rewrite, augment, or filter tool results based on
    /// conversation state.
    pub before_tool_response: Option<BeforeToolResponseCallback>,

    /// Called at turn boundaries (after extractors, before `on_turn_complete`).
    ///
    /// Receives shared State and a SessionWriter for injecting content into
    /// the conversation. Use this for context stuffing, K/V injection, condensed
    /// state summaries, or any outbound content interleaving.
    pub on_turn_boundary: Option<AsyncCallbackWith2<State, Arc<dyn SessionWriter>>>,

    /// State-reactive system instruction template (full replacement).
    ///
    /// Called after extractors run on each TurnComplete. If it returns
    /// `Some(instruction)`, the system instruction is updated mid-session.
    /// Returns `None` to leave the instruction unchanged.
    ///
    /// This is sync (no async) because instruction generation should be fast.
    pub instruction_template: Option<InstructionFn>,

    /// State-reactive instruction amendment (additive, not replacement).
    ///
    /// Called after extractors and phase transitions on each TurnComplete.
    /// If it returns `Some(text)`, the text is appended to the current phase
    /// instruction (separated by `\n\n`). Returns `None` to skip amendment.
    ///
    /// Unlike `instruction_template` (which replaces the entire instruction),
    /// this only adds to the phase instruction — the developer never needs to
    /// know or repeat the base instruction.
    pub instruction_amendment: Option<InstructionFn>,
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
            on_session_phase: None,
            on_usage: None,
            on_interrupted: None,
            on_tool_call: None,
            on_tool_cancelled: None,
            on_turn_complete: None,
            on_generation_complete: None,
            on_go_away: None,
            on_connected: None,
            on_disconnected: None,
            on_teardown: Vec::new(),
            on_teardown_concurrent: Vec::new(),
            on_resumed: None,
            on_error: None,
            on_transfer: None,
            on_extracted: None,
            on_extraction_error: None,
            on_interrupted_mode: ExecutionMode::Blocking,
            on_turn_boundary_mode: ExecutionMode::Blocking,
            on_turn_complete_mode: ExecutionMode::Blocking,
            on_generation_complete_mode: ExecutionMode::Blocking,
            on_connected_mode: ExecutionMode::Blocking,
            on_disconnected_mode: ExecutionMode::Blocking,
            on_error_mode: ExecutionMode::Blocking,
            on_go_away_mode: ExecutionMode::Blocking,
            on_extracted_mode: ExecutionMode::Blocking,
            on_extraction_error_mode: ExecutionMode::Blocking,
            on_tool_cancelled_mode: ExecutionMode::Blocking,
            on_transfer_mode: ExecutionMode::Blocking,
            on_resumed_mode: ExecutionMode::Blocking,
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
            .field("on_session_phase", &self.on_session_phase.is_some())
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
            .field("on_teardown", &self.on_teardown.len())
            .field("on_teardown_concurrent", &self.on_teardown_concurrent.len())
            .field("on_interrupted_mode", &self.on_interrupted_mode)
            .field("on_turn_boundary_mode", &self.on_turn_boundary_mode)
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
        assert_eq!(cb.on_turn_complete_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_connected_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_disconnected_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_error_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_go_away_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_extracted_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_extraction_error_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_tool_cancelled_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_transfer_mode, ExecutionMode::Blocking);
        assert_eq!(cb.on_resumed_mode, ExecutionMode::Blocking);
    }

    #[test]
    fn debug_shows_registered() {
        let cb = EventCallbacks {
            on_audio: Some(Box::new(|_| {})),
            ..Default::default()
        };
        let debug = format!("{cb:?}");
        assert!(debug.contains("on_audio: true"));
        assert!(debug.contains("on_text: false"));
    }
}
