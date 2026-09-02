//! Event callback registration methods for `Live`.
//!
//! # The lane rule
//!
//! Every callback runs on one of two lanes, and the lane decides what the
//! body may do:
//!
//! - **Fast lane** (`on_audio`, `on_text`, `on_text_complete`,
//!   `on_input_transcript`, `on_output_transcript`, `on_thought`,
//!   `on_vad_start`, `on_vad_end`, `on_session_phase`, `on_usage`): a sync
//!   `Fn` invoked inline on the event-dispatch path. It must return in well
//!   under a millisecond — no allocation, no locks, no I/O. A channel
//!   `try_send` is the right shape; anything heavier goes to the other lane
//!   through that channel.
//! - **Control lane** (everything returning a future): async, may block, runs
//!   in the processor's control loop. By default each is **awaited inline**
//!   ([`ExecutionMode::Blocking`]) so ordering and state are consistent. A
//!   `_concurrent` twin — `on_turn_complete_concurrent`, `on_connected_concurrent`,
//!   … — registers the same body as a **detached task**
//!   ([`ExecutionMode::Concurrent`]) for fire-and-forget work (logging,
//!   analytics, webhooks). Twins exist only where fire-and-forget is
//!   meaningful; hooks whose return value feeds the session (`on_tool_call`,
//!   `before_tool_response`) have none.
//!
//! Registering a callback twice keeps the last registration, except
//! [`on_teardown`](Live::on_teardown), which accumulates.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use gemini_adk_rs::State;
use gemini_adk_rs::live::ExecutionMode;
use gemini_genai_rs::prelude::*;

use super::Live;

impl Live {
    // -- Outbound Interceptors --

    /// Intercept tool responses before they are sent back to Gemini.
    ///
    /// Use this to rewrite, augment, or filter tool results based on
    /// conversation state. The callback receives the tool responses and the
    /// shared `State`, and returns (potentially modified) responses.
    ///
    /// # Example
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// # #[derive(Default, serde::Serialize, serde::Deserialize)]
    /// # struct OrderState { items: Vec<String> }
    /// Live::builder().before_tool_response(|responses, state| async move {
    ///     let order: OrderState = state.get("OrderState").unwrap_or_default();
    ///     responses.into_iter().map(|mut r| {
    ///         r.response["current_order"] = serde_json::to_value(&order).unwrap();
    ///         r
    ///     }).collect()
    /// });
    /// ```
    pub fn before_tool_response<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<FunctionResponse>, gemini_adk_rs::State) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Vec<FunctionResponse>> + Send + 'static,
    {
        self.callbacks.before_tool_response = Some(Arc::new(move |responses, state| {
            Box::pin(f(responses, state))
        }));
        self
    }

    /// Hook called at turn boundaries — after extractors run, before `on_turn_complete`.
    ///
    /// Receives the shared `State` and a `SessionWriter` for injecting content
    /// into the conversation. Use for context stuffing, K/V data injection,
    /// condensed state summaries, or any outbound content interleaving.
    ///
    /// # Example
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// Live::builder().on_turn_boundary(|state, writer| async move {
    ///     let summary = state.get::<String>("summary").unwrap_or_default();
    ///     writer.send_client_content(
    ///         vec![Content::user(format!("[Context: {summary}]"))],
    ///         false,
    ///     ).await.ok();
    /// });
    /// ```
    pub fn on_turn_boundary<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(gemini_adk_rs::State, Arc<dyn gemini_genai_rs::session::SessionWriter>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_turn_boundary =
            Some(Arc::new(move |state, writer| Box::pin(f(state, writer))));
        self
    }

    // -- Fast Lane Callbacks (sync, < 1ms) --

    /// Called for each audio chunk from the model (PCM16 24kHz).
    pub fn on_audio(mut self, f: impl Fn(&Bytes) + Send + Sync + 'static) -> Self {
        self.callbacks.on_audio = Some(Box::new(f));
        self
    }

    /// Called for each incremental text delta.
    pub fn on_text(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.callbacks.on_text = Some(Box::new(f));
        self
    }

    /// Called when model completes a text response.
    pub fn on_text_complete(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.callbacks.on_text_complete = Some(Box::new(f));
        self
    }

    /// Called for input (user speech) transcription: `f(text, is_final)`.
    ///
    /// While the user speaks, `is_final` is `false` and `text` is the latest
    /// partial recognition, which later calls may revise. At the turn boundary
    /// one call arrives with `is_final == true` carrying the complete
    /// transcript for the turn — the only value suitable for storage. Requires
    /// [`transcription`](Self::transcription) or
    /// [`input_transcription`](Self::input_transcription).
    pub fn on_input_transcript(
        mut self,
        f: impl Fn(&str, /* is_final */ bool) + Send + Sync + 'static,
    ) -> Self {
        self.callbacks.on_input_transcript = Some(Box::new(f));
        self
    }

    /// Called for output (model speech) transcription: `f(text, is_final)`.
    ///
    /// Same partial/final contract as
    /// [`on_input_transcript`](Self::on_input_transcript): `is_final` is
    /// `false` for revisable partials and `true` once for the turn's complete
    /// transcript. Requires [`transcription`](Self::transcription) or
    /// [`output_transcription`](Self::output_transcription).
    pub fn on_output_transcript(
        mut self,
        f: impl Fn(&str, /* is_final */ bool) + Send + Sync + 'static,
    ) -> Self {
        self.callbacks.on_output_transcript = Some(Box::new(f));
        self
    }

    /// Called when the model emits a thought/reasoning summary.
    ///
    /// Requires `.include_thoughts()` on the session config. Fast lane callback
    /// (sync, must complete in < 1ms).
    pub fn on_thought(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.callbacks.on_thought = Some(Box::new(f));
        self
    }

    /// Called when server VAD detects voice activity start.
    pub fn on_vad_start(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.callbacks.on_vad_start = Some(Box::new(f));
        self
    }

    /// Called when server VAD detects voice activity end.
    pub fn on_vad_end(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.callbacks.on_vad_end = Some(Box::new(f));
        self
    }

    /// Called when server sends token usage metadata.
    ///
    /// Receives a reference to the full [`UsageMetadata`] including prompt,
    /// response, cached, tool-use, and thoughts token counts plus per-modality
    /// breakdowns. Fires on the telemetry lane (not the fast lane).
    pub fn on_usage(mut self, f: impl Fn(&UsageMetadata) + Send + Sync + 'static) -> Self {
        self.callbacks.on_usage = Some(Box::new(f));
        self
    }

    /// Called on wire-level session phase transitions (connecting → active →
    /// disconnecting …). This is the transport lifecycle, not the
    /// `PhaseMachine` (see `.phase(..)`).
    ///
    /// Receives the new [`SessionPhase`]. Fast lane callback (sync, must
    /// complete in < 1ms). Use for lightweight UI state updates or metrics.
    pub fn on_session_phase(mut self, f: impl Fn(SessionPhase) + Send + Sync + 'static) -> Self {
        self.callbacks.on_session_phase = Some(Box::new(f));
        self
    }

    // -- Control Lane Callbacks (async, can block) --

    /// Called when model is interrupted by barge-in.
    ///
    /// Awaited before audio forwarding resumes, so a playback flush here is
    /// guaranteed to land before the next chunk.
    pub fn on_interrupted<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_interrupted = Some(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called when model requests tool execution.
    /// Return `None` to auto-dispatch, `Some(responses)` to override.
    /// Receives State for natural state promotion from tool results.
    pub fn on_tool_call<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<FunctionCall>, State) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<Vec<FunctionResponse>>> + Send + 'static,
    {
        self.callbacks.on_tool_call = Some(Arc::new(move |calls, state| Box::pin(f(calls, state))));
        self
    }

    /// Called when the server cancels pending tool calls.
    ///
    /// Receives the list of cancelled tool call IDs. Use to clean up any
    /// in-flight async work associated with those calls.
    pub fn on_tool_cancelled<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_tool_cancelled = Some(Arc::new(move |ids| Box::pin(f(ids))));
        self
    }

    /// Called when model turn completes.
    pub fn on_turn_complete<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_turn_complete = Some(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called when the model finishes generating its full intended response.
    ///
    /// Fires on the wire `GenerationComplete` event, before any interruption
    /// truncation. Use this to capture the model's complete output even when
    /// the user barges in. Paired with `.extract_on_generation()` for structured
    /// extraction of the pre-truncation response.
    pub fn on_generation_complete<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_generation_complete = Some(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called when server sends GoAway.
    pub fn on_go_away<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_go_away = Some(Arc::new(move |d| Box::pin(f(d))));
        self
    }

    /// Called when session connects (setup complete).
    ///
    /// Receives a `SessionWriter` for sending messages on connect.
    pub fn on_connected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Arc<dyn gemini_genai_rs::session::SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_connected = Some(Arc::new(move |w| Box::pin(f(w))));
        self
    }

    /// Called when session disconnects.
    pub fn on_disconnected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_disconnected = Some(Arc::new(move |r| Box::pin(f(r))));
        self
    }

    /// Register an **additive** teardown hook, run on disconnect before
    /// [`on_disconnected`](Self::on_disconnected).
    ///
    /// Every other callback setter replaces: calling `.on_disconnected(..)`
    /// twice keeps only the second, silently. That is workable for an
    /// application and unusable for an extension, which cannot know whether the
    /// application will register a handler after it. Hooks registered here
    /// accumulate instead, so `with_memory(..)` and the application's own
    /// `on_disconnected` both run regardless of the order they were written in.
    ///
    /// Hooks are awaited in registration order before the session finishes
    /// tearing down, so this is the seam for flushing durable state. Keep them
    /// bounded — a hook that hangs delays disconnect.
    ///
    /// ```no_run
    /// # use gemini_adk_fluent_rs::live::Live;
    /// Live::builder().on_teardown(|| async { /* flush */ });
    /// ```
    pub fn on_teardown<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks
            .on_teardown
            .push(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called after the session resumes following a GoAway disconnect.
    ///
    /// Use to re-subscribe to external streams, reset UI state, or log
    /// resume events. Paired with `.session_resume()` on the builder.
    pub fn on_resumed<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_resumed = Some(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called on non-fatal errors with the error's message.
    ///
    /// The argument is a `String`, not a typed error: the runtime funnels
    /// server errors, codec failures, and processor faults into one
    /// human-readable message here, and the session keeps running. Fatal
    /// errors end the session and arrive through
    /// [`on_disconnected`](Self::on_disconnected) instead.
    pub fn on_error<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_error = Some(Arc::new(move |e| Box::pin(f(e))));
        self
    }

    // -- Concurrent callback variants --
    // These set ExecutionMode::Concurrent so the callback is spawned as a
    // detached tokio task instead of being awaited inline.

    /// Called when model is interrupted by barge-in (spawned concurrently).
    ///
    /// Audio forwarding resumes without waiting for the body, so this is for
    /// bookkeeping (metrics, a log line) — a playback flush must use the
    /// blocking [`on_interrupted`](Self::on_interrupted).
    pub fn on_interrupted_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_interrupted = Some(Arc::new(move || Box::pin(f())));
        self.callbacks.on_interrupted_mode = ExecutionMode::Concurrent;
        self
    }

    /// Turn-boundary hook spawned concurrently — for observation only. The
    /// next turn proceeds without waiting, so context injected from here is
    /// not guaranteed to precede it; use [`on_turn_boundary`](Self::on_turn_boundary)
    /// for that.
    pub fn on_turn_boundary_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(gemini_adk_rs::State, Arc<dyn gemini_genai_rs::session::SessionWriter>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_turn_boundary =
            Some(Arc::new(move |state, writer| Box::pin(f(state, writer))));
        self.callbacks.on_turn_boundary_mode = ExecutionMode::Concurrent;
        self
    }

    /// An **additive** teardown hook spawned detached on disconnect rather
    /// than awaited — the disconnect does not wait for it. For a final metric
    /// or log line; anything that flushes durable state belongs in
    /// [`on_teardown`](Self::on_teardown).
    pub fn on_teardown_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks
            .on_teardown_concurrent
            .push(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called when model turn completes (spawned concurrently).
    pub fn on_turn_complete_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_turn_complete = Some(Arc::new(move || Box::pin(f())));
        self.callbacks.on_turn_complete_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called when the model finishes generating its full intended response (spawned concurrently).
    pub fn on_generation_complete_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_generation_complete = Some(Arc::new(move || Box::pin(f())));
        self.callbacks.on_generation_complete_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called when session connects (spawned concurrently).
    pub fn on_connected_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Arc<dyn gemini_genai_rs::session::SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_connected = Some(Arc::new(move |w| Box::pin(f(w))));
        self.callbacks.on_connected_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called when session disconnects (spawned concurrently).
    pub fn on_disconnected_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_disconnected = Some(Arc::new(move |r| Box::pin(f(r))));
        self.callbacks.on_disconnected_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called after session resumes from GoAway (spawned concurrently).
    pub fn on_resumed_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_resumed = Some(Arc::new(move || Box::pin(f())));
        self.callbacks.on_resumed_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called on non-fatal errors (spawned concurrently).
    pub fn on_error_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_error = Some(Arc::new(move |e| Box::pin(f(e))));
        self.callbacks.on_error_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called when server sends GoAway (spawned concurrently).
    pub fn on_go_away_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_go_away = Some(Arc::new(move |d| Box::pin(f(d))));
        self.callbacks.on_go_away_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called when the server cancels pending tool calls (spawned concurrently).
    pub fn on_tool_cancelled_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_tool_cancelled = Some(Arc::new(move |ids| Box::pin(f(ids))));
        self.callbacks.on_tool_cancelled_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called when a TurnExtractor produces a result (spawned concurrently).
    pub fn on_extracted_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extracted = Some(Arc::new(move |name, value| Box::pin(f(name, value))));
        self.callbacks.on_extracted_mode = ExecutionMode::Concurrent;
        self
    }

    /// Called when a TurnExtractor fails (spawned concurrently).
    pub fn on_extraction_error_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extraction_error =
            Some(Arc::new(move |name, error| Box::pin(f(name, error))));
        self.callbacks.on_extraction_error_mode = ExecutionMode::Concurrent;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that all four new callback setters are accepted by the builder
    /// and that the chain returns `Self` (i.e., the type-system accepts them).
    #[test]
    fn builder_accepts_new_callbacks() {
        let _live = Live::builder()
            // on_session_phase: sync fast-lane
            .on_session_phase(|_phase| {})
            // on_tool_cancelled: async control-lane
            .on_tool_cancelled(|_ids| async {})
            // on_generation_complete: async control-lane, no args
            .on_generation_complete(|| async {})
            // on_resumed: async control-lane, no args
            .on_resumed(|| async {});
        // Compiles = test passes
    }

    /// Verify that the concurrent variants of the new setters also compile.
    #[test]
    fn builder_accepts_new_callbacks_concurrent() {
        let live = Live::builder()
            .on_tool_cancelled_concurrent(|_ids| async {})
            .on_generation_complete_concurrent(|| async {})
            .on_resumed_concurrent(|| async {})
            .on_interrupted_concurrent(|| async {})
            .on_turn_boundary_concurrent(|_state, _writer| async {})
            .on_teardown_concurrent(|| async {});
        assert_eq!(
            live.callbacks.on_interrupted_mode,
            ExecutionMode::Concurrent
        );
        assert_eq!(
            live.callbacks.on_turn_boundary_mode,
            ExecutionMode::Concurrent
        );
        assert_eq!(live.callbacks.on_teardown_concurrent.len(), 1);
    }
}
