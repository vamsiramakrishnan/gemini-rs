//! Typed callback registry for Live session events.
//!
//! Fast lane callbacks (sync, < 1ms): audio, text, transcripts, VAD.
//! Control lane callbacks (async, can block): tool calls, lifecycle, interruptions.
//! Outbound interceptors: transform tool responses, inject context at turn boundaries.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rs_genai::prelude::{FunctionCall, FunctionResponse, SessionPhase};
use rs_genai::session::SessionWriter;

use super::BoxFuture;
use crate::state::State;

/// Controls how a callback is executed relative to the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallbackMode {
    /// Callback is awaited inline — the event loop waits for completion.
    #[default]
    Blocking,
    /// Callback is spawned as a concurrent task — the event loop continues immediately.
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
    /// Called when server-side VAD detects voice activity start.
    pub on_vad_start: Option<Box<dyn Fn() + Send + Sync>>,
    /// Called when server-side VAD detects voice activity end.
    pub on_vad_end: Option<Box<dyn Fn() + Send + Sync>>,
    /// Called on session phase transitions.
    pub on_phase: Option<Box<dyn Fn(SessionPhase) + Send + Sync>>,

    // -- Control lane (async callbacks) --

    /// Called when the model is interrupted by barge-in.
    pub on_interrupted: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called when model requests tool execution.
    /// Return `None` to use auto-dispatch (ToolDispatcher), `Some` to override.
    pub on_tool_call:
        Option<Arc<dyn Fn(Vec<FunctionCall>) -> BoxFuture<Option<Vec<FunctionResponse>>> + Send + Sync>>,
    /// Called when server cancels pending tool calls.
    pub on_tool_cancelled: Option<Arc<dyn Fn(Vec<String>) -> BoxFuture<()> + Send + Sync>>,
    /// Called when the model completes its turn.
    pub on_turn_complete: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called when server sends GoAway (session ending soon).
    pub on_go_away: Option<Arc<dyn Fn(Duration) -> BoxFuture<()> + Send + Sync>>,
    /// Called when session setup completes (connected).
    pub on_connected: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called when session disconnects.
    pub on_disconnected: Option<Arc<dyn Fn(Option<String>) -> BoxFuture<()> + Send + Sync>>,
    /// Called after session resumes from GoAway.
    pub on_resumed: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called on non-fatal errors.
    pub on_error: Option<Arc<dyn Fn(String) -> BoxFuture<()> + Send + Sync>>,
    /// Called when agent transfer occurs (from, to).
    pub on_transfer: Option<Arc<dyn Fn(String, String) -> BoxFuture<()> + Send + Sync>>,
    /// Called when a TurnExtractor produces a result (extractor_name, value).
    pub on_extracted:
        Option<Arc<dyn Fn(String, serde_json::Value) -> BoxFuture<()> + Send + Sync>>,

    // -- Outbound interceptors (transform data going to Gemini) --

    /// Intercept tool responses before sending to Gemini.
    ///
    /// Receives the tool responses and shared State. Returns (potentially modified)
    /// responses. Use this to rewrite, augment, or filter tool results based on
    /// conversation state.
    pub before_tool_response:
        Option<Arc<dyn Fn(Vec<FunctionResponse>, State) -> BoxFuture<Vec<FunctionResponse>> + Send + Sync>>,

    /// Called at turn boundaries (after extractors, before `on_turn_complete`).
    ///
    /// Receives shared State and a SessionWriter for injecting content into
    /// the conversation. Use this for context stuffing, K/V injection, condensed
    /// state summaries, or any outbound content interleaving.
    pub on_turn_boundary:
        Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,

    /// State-reactive system instruction template.
    ///
    /// Called after extractors run on each TurnComplete. If it returns
    /// `Some(instruction)`, the system instruction is updated mid-session.
    /// Returns `None` to leave the instruction unchanged.
    ///
    /// This is sync (no async) because instruction generation should be fast.
    pub instruction_template: Option<Arc<dyn Fn(&State) -> Option<String> + Send + Sync>>,
}

impl Default for EventCallbacks {
    fn default() -> Self {
        Self {
            on_audio: None,
            on_text: None,
            on_text_complete: None,
            on_input_transcript: None,
            on_output_transcript: None,
            on_vad_start: None,
            on_vad_end: None,
            on_phase: None,
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
            before_tool_response: None,
            on_turn_boundary: None,
            instruction_template: None,
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
            .field("on_vad_start", &self.on_vad_start.is_some())
            .field("on_vad_end", &self.on_vad_end.is_some())
            .field("on_phase", &self.on_phase.is_some())
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
            .field("before_tool_response", &self.before_tool_response.is_some())
            .field("on_turn_boundary", &self.on_turn_boundary.is_some())
            .field("instruction_template", &self.instruction_template.is_some())
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
    fn debug_shows_registered() {
        let mut cb = EventCallbacks::default();
        cb.on_audio = Some(Box::new(|_| {}));
        let debug = format!("{:?}", cb);
        assert!(debug.contains("on_audio: true"));
        assert!(debug.contains("on_text: false"));
    }
}
