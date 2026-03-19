# Live Session Fluent API — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a two-lane callback system and fluent `Live::builder()` API for full-duplex Gemini Live sessions, eliminating ~100 lines of boilerplate per demo app.

**Architecture:** L0 gets `send_video()` + `update_instruction()` convenience methods (built on existing `send_client_content` + `SendAudio` with video field). L1 gets `EventCallbacks` (typed callback registry), two-lane event processor (fast lane for audio/text, control lane for tools/lifecycle), and `LiveSessionBuilder`. L2 gets `Live::builder()` fluent wrapper.

**Tech Stack:** Rust, tokio (mpsc/broadcast channels, spawn, select), async-trait, bytes, arc-swap, parking_lot

---

### Task 1: L0 — Add send_video and update_instruction to SessionHandle

**Files:**
- Modify: `crates/gemini-genai-rs/src/session/mod.rs` (SessionCommand enum + SessionWriter trait + SessionHandle impl)
- Modify: `crates/gemini-genai-rs/src/transport/codec.rs` (encode new commands)

**Step 1: Add SessionCommand variants**

In `crates/gemini-genai-rs/src/session/mod.rs`, add to the `SessionCommand` enum:

```rust
/// Send video/image data (raw JPEG bytes, will be base64-encoded).
SendVideo(Vec<u8>),
/// Update system instruction mid-session (sends client_content with role=system).
UpdateInstruction(String),
```

**Step 2: Add methods to SessionWriter trait**

```rust
async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError>;
async fn update_instruction(&self, instruction: String) -> Result<(), SessionError>;
```

**Step 3: Implement on SessionHandle**

```rust
async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
    self.send_command(SessionCommand::SendVideo(jpeg_data)).await
}

async fn update_instruction(&self, instruction: String) -> Result<(), SessionError> {
    self.send_command(SessionCommand::UpdateInstruction(instruction)).await
}
```

Also add convenience methods (non-trait, on SessionHandle directly):

```rust
/// Send a video/image frame (raw JPEG bytes).
pub async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
    self.send_command(SessionCommand::SendVideo(jpeg_data)).await
}

/// Update the system instruction mid-session.
pub async fn update_instruction(&self, instruction: impl Into<String>) -> Result<(), SessionError> {
    self.send_command(SessionCommand::UpdateInstruction(instruction.into())).await
}
```

**Step 4: Encode in JsonCodec**

In `crates/gemini-genai-rs/src/transport/codec.rs`, add to `encode_command` match:

```rust
SessionCommand::SendVideo(data) => {
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    let msg = RealtimeInputMessage {
        realtime_input: RealtimeInputPayload {
            media_chunks: Vec::new(),
            audio: None,
            video: Some(Blob {
                mime_type: "image/jpeg".to_string(),
                data: encoded,
            }),
            audio_stream_end: None,
            text: None,
        },
    };
    serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
}
SessionCommand::UpdateInstruction(instruction) => {
    let msg = ClientContentMessage {
        client_content: ClientContentPayload {
            turns: vec![Content {
                role: Some(Role::System),
                parts: vec![Part::Text { text: instruction.clone() }],
            }],
            turn_complete: Some(false),
        },
    };
    serde_json::to_vec(&msg).map_err(|e| CodecError::Serialize(e.to_string()))
}
```

**Step 5: Update AgentSession in gemini-adk-rs**

In `crates/gemini-adk-rs/src/agent_session.rs`, add:

```rust
pub async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), AgentError> {
    self.writer.send_video(jpeg_data).await.map_err(AgentError::Session)
}

pub async fn update_instruction(&self, instruction: impl Into<String>) -> Result<(), AgentError> {
    self.writer.update_instruction(instruction.into()).await.map_err(AgentError::Session)
}
```

**Step 6: Add tests**

```rust
#[test]
fn send_video_command_encodes() {
    let codec = JsonCodec;
    let config = SessionConfig::new("test-key");
    let cmd = SessionCommand::SendVideo(vec![0xFF, 0xD8, 0xFF]); // JPEG magic bytes
    let bytes = codec.encode_command(&cmd, &config).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["realtime_input"]["video"]["mime_type"].as_str().unwrap() == "image/jpeg");
}

#[test]
fn update_instruction_command_encodes() {
    let codec = JsonCodec;
    let config = SessionConfig::new("test-key");
    let cmd = SessionCommand::UpdateInstruction("New instruction".into());
    let bytes = codec.encode_command(&cmd, &config).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let turns = &json["client_content"]["turns"];
    assert_eq!(turns[0]["role"], "system");
    assert_eq!(turns[0]["parts"][0]["text"], "New instruction");
}
```

**Step 7: Run tests**

Run: `cargo test -p gemini-genai-rs`

**Step 8: Commit**

```
feat(gemini-genai-rs): add send_video and update_instruction to SessionHandle
```

---

### Task 2: L1 — EventCallbacks struct

**Files:**
- Create: `crates/gemini-adk-rs/src/live/mod.rs`
- Create: `crates/gemini-adk-rs/src/live/callbacks.rs`
- Modify: `crates/gemini-adk-rs/src/lib.rs` (add `pub mod live;`)

**Step 1: Create module structure**

Create `crates/gemini-adk-rs/src/live/mod.rs`:
```rust
//! Live session management — callback-driven full-duplex event handling.

pub mod callbacks;

pub use callbacks::EventCallbacks;
```

**Step 2: Create EventCallbacks**

Create `crates/gemini-adk-rs/src/live/callbacks.rs`:

```rust
//! Typed callback registry for Live session events.
//!
//! Fast lane callbacks (sync, < 1ms): audio, text, transcripts, VAD.
//! Control lane callbacks (async, can block): tool calls, lifecycle, interruptions.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gemini_genai_rs::prelude::{FunctionCall, FunctionResponse, SessionPhase};

/// A boxed future for async callbacks.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Typed callback registry for Live session events.
///
/// Callbacks are divided into two lanes:
/// - **Fast lane** (sync): Called inline, must be < 1ms. For audio, text, transcripts, VAD.
/// - **Control lane** (async): Awaited on a dedicated task. For tool calls, lifecycle, interruptions.
pub struct EventCallbacks {
    // ── Fast lane (sync callbacks) ──────────────────────────────────────
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

    // ── Control lane (async callbacks) ──────────────────────────────────
    /// Called when the model is interrupted by barge-in. BLOCKING.
    pub on_interrupted: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called when model requests tool execution.
    /// Return `None` to use auto-dispatch (ToolDispatcher), `Some` to override.
    pub on_tool_call: Option<Arc<dyn Fn(Vec<FunctionCall>) -> BoxFuture<Option<Vec<FunctionResponse>>> + Send + Sync>>,
    /// Called when server cancels pending tool calls.
    pub on_tool_cancelled: Option<Arc<dyn Fn(Vec<String>) -> BoxFuture<()> + Send + Sync>>,
    /// Called when the model completes its turn. BLOCKING.
    pub on_turn_complete: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called when server sends GoAway (session ending soon). BLOCKING.
    pub on_go_away: Option<Arc<dyn Fn(Duration) -> BoxFuture<()> + Send + Sync>>,
    /// Called when session setup completes (connected). BLOCKING.
    pub on_connected: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called when session disconnects. BLOCKING.
    pub on_disconnected: Option<Arc<dyn Fn(Option<String>) -> BoxFuture<()> + Send + Sync>>,
    /// Called after session resumes from GoAway. BLOCKING.
    pub on_resumed: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    /// Called on non-fatal errors.
    pub on_error: Option<Arc<dyn Fn(String) -> BoxFuture<()> + Send + Sync>>,
    /// Called when agent transfer occurs (from, to).
    pub on_transfer: Option<Arc<dyn Fn(String, String) -> BoxFuture<()> + Send + Sync>>,
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
```

**Step 3: Wire into lib.rs**

Add to `crates/gemini-adk-rs/src/lib.rs`:
```rust
pub mod live;
```

**Step 4: Add `bytes` dependency to gemini-adk-rs Cargo.toml**

Check if `bytes` is already a dependency; if not, add it.

**Step 5: Run tests**

Run: `cargo test -p gemini-adk-rs`

**Step 6: Commit**

```
feat(gemini-adk-rs): add EventCallbacks typed callback registry for Live sessions
```

---

### Task 3: L1 — Two-Lane Event Processor

**Files:**
- Create: `crates/gemini-adk-rs/src/live/processor.rs`
- Modify: `crates/gemini-adk-rs/src/live/mod.rs`

**Step 1: Create the two-lane processor**

Create `crates/gemini-adk-rs/src/live/processor.rs`:

This implements `LiveEventProcessor` which:
1. Subscribes to `SessionEvent` broadcast
2. Routes events to fast lane or control lane channels
3. Spawns fast consumer task (sync callbacks)
4. Spawns control processor task (async callbacks + tool dispatch)
5. Manages shared interrupted state between lanes

```rust
//! Two-lane event processor for Live sessions.
//!
//! Fast lane: audio, text, transcripts, VAD (sync callbacks, never blocks)
//! Control lane: tool calls, interruptions, lifecycle (async callbacks, can block)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

use gemini_genai_rs::prelude::{FunctionResponse, SessionEvent, SessionPhase};
use gemini_genai_rs::session::SessionWriter;

use crate::error::AgentError;
use crate::tool::ToolDispatcher;

use super::callbacks::EventCallbacks;

/// Events routed to the fast lane (sync processing).
pub(crate) enum FastEvent {
    Audio(Bytes),
    Text(String),
    TextComplete(String),
    InputTranscript(String, bool),
    OutputTranscript(String, bool),
    VadStart,
    VadEnd,
    Phase(SessionPhase),
    /// Interruption flag — tells fast lane to stop forwarding audio.
    Interrupted,
    /// Resume forwarding after interruption processed.
    ResumeAfterInterrupt,
}

/// Events routed to the control lane (async processing).
pub(crate) enum ControlEvent {
    ToolCall(Vec<gemini_genai_rs::prelude::FunctionCall>),
    ToolCallCancelled(Vec<String>),
    Interrupted,
    TurnComplete,
    GoAway(Option<String>),
    Connected,
    Disconnected(Option<String>),
    SessionResumeHandle(String),
    Error(String),
}

/// Shared state between the two lanes.
pub(crate) struct SharedState {
    /// When true, fast lane suppresses audio callbacks.
    pub interrupted: AtomicBool,
    /// Latest resume handle from server.
    pub resume_handle: parking_lot::Mutex<Option<String>>,
}

/// Runs the two-lane event processor.
///
/// Returns JoinHandles for the fast consumer and control processor tasks.
pub(crate) fn spawn_event_processor(
    mut event_rx: broadcast::Receiver<SessionEvent>,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    writer: Arc<dyn SessionWriter>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let shared = Arc::new(SharedState {
        interrupted: AtomicBool::new(false),
        resume_handle: parking_lot::Mutex::new(None),
    });

    // Channels between router and lanes
    let (fast_tx, fast_rx) = mpsc::unbounded_channel::<FastEvent>();
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlEvent>(64);

    // Spawn the router task (reads broadcast, routes to lanes)
    let fast_tx_clone = fast_tx.clone();
    let ctrl_tx_clone = ctrl_tx.clone();
    let shared_clone = shared.clone();
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    route_event(event, &fast_tx_clone, &ctrl_tx_clone, &shared_clone).await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "Event processor lagged, skipped events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Spawn fast consumer
    let fast_callbacks = callbacks.clone();
    let fast_shared = shared.clone();
    let fast_handle = tokio::spawn(async move {
        run_fast_lane(fast_rx, fast_callbacks, fast_shared).await;
    });

    // Spawn control processor
    let ctrl_callbacks = callbacks.clone();
    let ctrl_shared = shared;
    let ctrl_handle = tokio::spawn(async move {
        run_control_lane(ctrl_rx, ctrl_callbacks, dispatcher, writer, ctrl_shared).await;
    });

    (fast_handle, ctrl_handle)
}

/// Routes a SessionEvent to the appropriate lane.
async fn route_event(
    event: SessionEvent,
    fast_tx: &mpsc::UnboundedSender<FastEvent>,
    ctrl_tx: &mpsc::Sender<ControlEvent>,
    shared: &SharedState,
) {
    match event {
        // Fast lane events
        SessionEvent::AudioData(data) => {
            let _ = fast_tx.send(FastEvent::Audio(data));
        }
        SessionEvent::TextDelta(text) => {
            let _ = fast_tx.send(FastEvent::Text(text));
        }
        SessionEvent::TextComplete(text) => {
            let _ = fast_tx.send(FastEvent::TextComplete(text));
        }
        SessionEvent::InputTranscription(text) => {
            let _ = fast_tx.send(FastEvent::InputTranscript(text, false));
        }
        SessionEvent::OutputTranscription(text) => {
            let _ = fast_tx.send(FastEvent::OutputTranscript(text, false));
        }
        SessionEvent::VoiceActivityStart => {
            let _ = fast_tx.send(FastEvent::VadStart);
        }
        SessionEvent::VoiceActivityEnd => {
            let _ = fast_tx.send(FastEvent::VadEnd);
        }
        SessionEvent::PhaseChanged(phase) => {
            let _ = fast_tx.send(FastEvent::Phase(phase));
        }
        SessionEvent::SessionResumeHandle(handle) => {
            *shared.resume_handle.lock() = Some(handle.clone());
            let _ = ctrl_tx.send(ControlEvent::SessionResumeHandle(handle)).await;
        }

        // Control lane events
        SessionEvent::ToolCall(calls) => {
            let _ = ctrl_tx.send(ControlEvent::ToolCall(calls)).await;
        }
        SessionEvent::ToolCallCancelled(ids) => {
            let _ = ctrl_tx.send(ControlEvent::ToolCallCancelled(ids)).await;
        }
        SessionEvent::Interrupted => {
            // Signal BOTH lanes
            shared.interrupted.store(true, Ordering::Release);
            let _ = fast_tx.send(FastEvent::Interrupted);
            let _ = ctrl_tx.send(ControlEvent::Interrupted).await;
        }
        SessionEvent::TurnComplete => {
            let _ = ctrl_tx.send(ControlEvent::TurnComplete).await;
        }
        SessionEvent::GoAway(time_left) => {
            let _ = ctrl_tx.send(ControlEvent::GoAway(time_left)).await;
        }
        SessionEvent::Connected => {
            let _ = ctrl_tx.send(ControlEvent::Connected).await;
        }
        SessionEvent::Disconnected(reason) => {
            let _ = ctrl_tx.send(ControlEvent::Disconnected(reason)).await;
        }
        SessionEvent::Error(err) => {
            let _ = ctrl_tx.send(ControlEvent::Error(err)).await;
        }
    }
}

/// Fast lane consumer — processes high-frequency events with sync callbacks.
async fn run_fast_lane(
    mut rx: mpsc::UnboundedReceiver<FastEvent>,
    callbacks: Arc<EventCallbacks>,
    shared: Arc<SharedState>,
) {
    while let Some(event) = rx.recv().await {
        match event {
            FastEvent::Audio(data) => {
                // Suppress audio during interruption
                if !shared.interrupted.load(Ordering::Acquire) {
                    if let Some(cb) = &callbacks.on_audio {
                        cb(&data);
                    }
                }
            }
            FastEvent::Text(delta) => {
                if let Some(cb) = &callbacks.on_text {
                    cb(&delta);
                }
            }
            FastEvent::TextComplete(text) => {
                if let Some(cb) = &callbacks.on_text_complete {
                    cb(&text);
                }
            }
            FastEvent::InputTranscript(text, finished) => {
                if let Some(cb) = &callbacks.on_input_transcript {
                    cb(&text, finished);
                }
            }
            FastEvent::OutputTranscript(text, finished) => {
                if let Some(cb) = &callbacks.on_output_transcript {
                    cb(&text, finished);
                }
            }
            FastEvent::VadStart => {
                if let Some(cb) = &callbacks.on_vad_start {
                    cb();
                }
            }
            FastEvent::VadEnd => {
                if let Some(cb) = &callbacks.on_vad_end {
                    cb();
                }
            }
            FastEvent::Phase(phase) => {
                if let Some(cb) = &callbacks.on_phase {
                    cb(phase);
                }
            }
            FastEvent::Interrupted => {
                // Audio already suppressed via shared.interrupted flag
            }
            FastEvent::ResumeAfterInterrupt => {
                shared.interrupted.store(false, Ordering::Release);
            }
        }
    }
}

/// Control lane processor — handles lifecycle events and tool dispatch.
async fn run_control_lane(
    mut rx: mpsc::Receiver<ControlEvent>,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    writer: Arc<dyn SessionWriter>,
    shared: Arc<SharedState>,
) {
    while let Some(event) = rx.recv().await {
        match event {
            ControlEvent::ToolCall(calls) => {
                // 1. Check user callback for override
                let responses = if let Some(cb) = &callbacks.on_tool_call {
                    cb(calls.clone()).await
                } else {
                    None
                };

                // 2. If no override, auto-dispatch via ToolDispatcher
                let responses = match responses {
                    Some(r) => r,
                    None => {
                        if let Some(ref disp) = dispatcher {
                            let mut results = Vec::new();
                            for call in &calls {
                                match disp.call_function(&call.name, call.args.clone()).await {
                                    Ok(result) => results.push(FunctionResponse {
                                        name: call.name.clone(),
                                        response: result,
                                        id: call.id.clone(),
                                    }),
                                    Err(e) => results.push(FunctionResponse {
                                        name: call.name.clone(),
                                        response: serde_json::json!({"error": e.to_string()}),
                                        id: call.id.clone(),
                                    }),
                                }
                            }
                            results
                        } else {
                            tracing::warn!("Tool call received but no dispatcher or callback registered");
                            Vec::new()
                        }
                    }
                };

                // 3. Send tool responses back to Gemini
                if !responses.is_empty() {
                    if let Err(e) = writer.send_tool_response(responses).await {
                        tracing::error!("Failed to send tool response: {e}");
                    }
                }
            }
            ControlEvent::ToolCallCancelled(ids) => {
                if let Some(ref disp) = dispatcher {
                    disp.cancel_by_ids(&ids);
                }
                if let Some(cb) = &callbacks.on_tool_cancelled {
                    cb(ids).await;
                }
            }
            ControlEvent::Interrupted => {
                if let Some(cb) = &callbacks.on_interrupted {
                    cb().await;
                }
                // Resume audio forwarding after interrupt callback completes
                shared.interrupted.store(false, Ordering::Release);
            }
            ControlEvent::TurnComplete => {
                if let Some(cb) = &callbacks.on_turn_complete {
                    cb().await;
                }
            }
            ControlEvent::GoAway(time_left) => {
                let duration = time_left
                    .as_deref()
                    .and_then(|s| s.trim_end_matches('s').parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(60));
                if let Some(cb) = &callbacks.on_go_away {
                    cb(duration).await;
                }
            }
            ControlEvent::Connected => {
                if let Some(cb) = &callbacks.on_connected {
                    cb().await;
                }
            }
            ControlEvent::Disconnected(reason) => {
                if let Some(cb) = &callbacks.on_disconnected {
                    cb(reason).await;
                }
            }
            ControlEvent::SessionResumeHandle(_handle) => {
                // Already stored in shared state by the router
            }
            ControlEvent::Error(err) => {
                if let Some(cb) = &callbacks.on_error {
                    cb(err).await;
                }
            }
        }
    }
}
```

**Step 2: Add to live/mod.rs**

```rust
pub mod processor;
```

**Step 3: Add dependencies to gemini-adk-rs Cargo.toml**

Add `bytes`, `parking_lot`, `arc-swap`, `tracing` (if not already present).

**Step 4: Run tests**

Run: `cargo test -p gemini-adk-rs && cargo build -p gemini-adk-rs`

**Step 5: Commit**

```
feat(gemini-adk-rs): add two-lane event processor for Live sessions
```

---

### Task 4: L1 — LiveSessionBuilder and LiveHandle

**Files:**
- Create: `crates/gemini-adk-rs/src/live/builder.rs`
- Create: `crates/gemini-adk-rs/src/live/handle.rs`
- Modify: `crates/gemini-adk-rs/src/live/mod.rs`

**Step 1: Create LiveHandle**

`crates/gemini-adk-rs/src/live/handle.rs` — runtime interaction with a live session:

```rust
//! LiveHandle — runtime interaction with a Live session.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use gemini_genai_rs::prelude::{Content, FunctionResponse, SessionEvent, SessionPhase};
use gemini_genai_rs::session::{SessionError, SessionHandle, SessionWriter};

use crate::error::AgentError;

/// Handle for interacting with a running Live session.
///
/// Provides send methods for audio/text/video, system instruction updates,
/// event subscription, and graceful shutdown.
#[derive(Clone)]
pub struct LiveHandle {
    session: SessionHandle,
    _fast_task: Arc<JoinHandle<()>>,
    _ctrl_task: Arc<JoinHandle<()>>,
}

impl LiveHandle {
    pub(crate) fn new(
        session: SessionHandle,
        fast_task: JoinHandle<()>,
        ctrl_task: JoinHandle<()>,
    ) -> Self {
        Self {
            session,
            _fast_task: Arc::new(fast_task),
            _ctrl_task: Arc::new(ctrl_task),
        }
    }

    /// Send audio data (raw PCM16 16kHz bytes).
    pub async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError> {
        self.session.send_audio(data).await
    }

    /// Send a text message.
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), SessionError> {
        self.session.send_text(text.into()).await
    }

    /// Send a video/image frame (raw JPEG bytes).
    pub async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        self.session.send_video(jpeg_data).await
    }

    /// Update the system instruction mid-session.
    pub async fn update_instruction(&self, instruction: impl Into<String>) -> Result<(), SessionError> {
        self.session.update_instruction(instruction.into()).await
    }

    /// Send tool responses manually (if not using auto-dispatch).
    pub async fn send_tool_response(&self, responses: Vec<FunctionResponse>) -> Result<(), SessionError> {
        self.session.send_tool_response(responses).await
    }

    /// Subscribe to raw session events (for custom processing).
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.session.subscribe()
    }

    /// Get the current session phase.
    pub fn phase(&self) -> SessionPhase {
        self.session.phase()
    }

    /// Gracefully disconnect the session.
    pub async fn disconnect(&self) -> Result<(), SessionError> {
        self.session.disconnect().await
    }

    /// Wait for the session to end (disconnect, GoAway, or error).
    pub async fn done(&self) -> Result<(), SessionError> {
        self.session.join().await.map_err(|_| SessionError::ChannelClosed)
    }

    /// Get the underlying SessionHandle for advanced usage.
    pub fn session(&self) -> &SessionHandle {
        &self.session
    }
}
```

**Step 2: Create LiveSessionBuilder**

`crates/gemini-adk-rs/src/live/builder.rs` — builds and connects a Live session:

```rust
//! LiveSessionBuilder — combines SessionConfig + callbacks + tools into one setup.

use std::sync::Arc;

use gemini_genai_rs::prelude::SessionConfig;
use gemini_genai_rs::session::SessionHandle;

use crate::error::AgentError;
use crate::tool::ToolDispatcher;

use super::callbacks::EventCallbacks;
use super::handle::LiveHandle;
use super::processor::spawn_event_processor;

/// Builder for a callback-driven Live session.
pub struct LiveSessionBuilder {
    config: SessionConfig,
    callbacks: EventCallbacks,
    dispatcher: Option<Arc<ToolDispatcher>>,
}

impl LiveSessionBuilder {
    /// Create a new builder with the given session config.
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            callbacks: EventCallbacks::default(),
            dispatcher: None,
        }
    }

    /// Set the tool dispatcher for auto-dispatch of tool calls.
    pub fn dispatcher(mut self, dispatcher: ToolDispatcher) -> Self {
        // Add tool declarations to session config
        for tool in dispatcher.to_tool_declarations() {
            self.config = self.config.add_tool(tool);
        }
        self.dispatcher = Some(Arc::new(dispatcher));
        self
    }

    /// Set the event callbacks.
    pub fn callbacks(mut self, callbacks: EventCallbacks) -> Self {
        self.callbacks = callbacks;
        self
    }

    /// Connect to Gemini and start the event processor.
    pub async fn connect(self) -> Result<LiveHandle, AgentError> {
        // Connect via L0
        let session: SessionHandle = gemini_genai_rs::connect(self.config)
            .await
            .map_err(AgentError::Session)?;

        // Wait for Active phase
        session
            .wait_for_phase(gemini_genai_rs::prelude::SessionPhase::Active)
            .await;

        let callbacks = Arc::new(self.callbacks);
        let writer: Arc<dyn gemini_genai_rs::session::SessionWriter> = Arc::new(session.clone());
        let event_rx = session.subscribe();

        // Spawn two-lane processor
        let (fast_handle, ctrl_handle) =
            spawn_event_processor(event_rx, callbacks, self.dispatcher, writer);

        Ok(LiveHandle::new(session, fast_handle, ctrl_handle))
    }
}
```

**Step 3: Update live/mod.rs**

```rust
pub mod builder;
pub mod callbacks;
pub mod handle;
pub mod processor;

pub use builder::LiveSessionBuilder;
pub use callbacks::EventCallbacks;
pub use handle::LiveHandle;
```

**Step 4: Re-export from lib.rs**

In `crates/gemini-adk-rs/src/lib.rs`, add to the public API:
```rust
pub use live::{EventCallbacks, LiveHandle, LiveSessionBuilder};
```

**Step 5: Add tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_with_defaults() {
        let config = SessionConfig::new("test-key");
        let builder = LiveSessionBuilder::new(config);
        // Just verify it compiles and doesn't panic
        assert!(builder.dispatcher.is_none());
    }
}
```

**Step 6: Build and test**

Run: `cargo build -p gemini-adk-rs && cargo test -p gemini-adk-rs`

**Step 7: Commit**

```
feat(gemini-adk-rs): add LiveSessionBuilder and LiveHandle for callback-driven sessions
```

---

### Task 5: L2 — Live::builder() Fluent API

**Files:**
- Create: `crates/gemini-adk-fluent-rs/src/live.rs`
- Modify: `crates/gemini-adk-fluent-rs/src/lib.rs`
- Modify: `crates/gemini-adk-fluent-rs/src/prelude.rs` (via lib.rs prelude block)

**Step 1: Create the fluent Live builder**

`crates/gemini-adk-fluent-rs/src/live.rs`:

```rust
//! `Live` — Fluent builder for callback-driven Gemini Live sessions.
//!
//! Wraps L1's `LiveSessionBuilder` with ergonomic callback registration
//! and integration with composition modules (M, T, P).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use gemini_adk_rs::live::{EventCallbacks, LiveHandle, LiveSessionBuilder};
use gemini_adk_rs::tool::ToolDispatcher;
use gemini_genai_rs::prelude::*;

use crate::compose::middleware::MiddlewareComposite;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Fluent builder for Gemini Live sessions.
///
/// # Example
/// ```ignore
/// let session = Live::builder()
///     .model(GeminiModel::GeminiLive2_5FlashNativeAudio)
///     .voice(Voice::Kore)
///     .instruction("You are a weather assistant")
///     .tools(dispatcher)
///     .on_audio(|data| playback_tx.send(data.clone()).ok())
///     .on_text(|t| print!("{t}"))
///     .on_interrupted(|| async { playback.flush().await; })
///     .connect_vertex("project", "us-central1", token)
///     .await?;
/// ```
pub struct Live {
    config: SessionConfig,
    callbacks: EventCallbacks,
    dispatcher: Option<ToolDispatcher>,
}

impl Live {
    /// Start building a Live session.
    pub fn builder() -> Self {
        Self {
            config: SessionConfig::from_endpoint(ApiEndpoint::google_ai("")),
            callbacks: EventCallbacks::default(),
            dispatcher: None,
        }
    }

    // ── Model & Voice ────────────────────────────────────────────────

    /// Set the Gemini model.
    pub fn model(mut self, model: GeminiModel) -> Self {
        self.config = self.config.model(model);
        self
    }

    /// Set the output voice.
    pub fn voice(mut self, voice: Voice) -> Self {
        self.config = self.config.voice(voice);
        self
    }

    /// Set the system instruction.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.config = self.config.system_instruction(instruction);
        self
    }

    /// Set the temperature.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.config = self.config.temperature(temp);
        self
    }

    // ── Tools ────────────────────────────────────────────────────────

    /// Set the tool dispatcher (auto-dispatches tool calls).
    pub fn tools(mut self, dispatcher: ToolDispatcher) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// Enable Google Search built-in tool.
    pub fn google_search(mut self) -> Self {
        self.config = self.config.with_google_search();
        self
    }

    /// Enable code execution built-in tool.
    pub fn code_execution(mut self) -> Self {
        self.config = self.config.with_code_execution();
        self
    }

    /// Enable URL context built-in tool.
    pub fn url_context(mut self) -> Self {
        self.config = self.config.with_url_context();
        self
    }

    // ── Audio/Video Config ───────────────────────────────────────────

    /// Enable input and/or output transcription.
    pub fn transcription(mut self, input: bool, output: bool) -> Self {
        if input {
            self.config = self.config.enable_input_transcription();
        }
        if output {
            self.config = self.config.enable_output_transcription();
        }
        self
    }

    /// Enable affective dialog (emotionally expressive responses).
    pub fn affective_dialog(mut self, enabled: bool) -> Self {
        self.config = self.config.affective_dialog(enabled);
        self
    }

    /// Enable proactive audio.
    pub fn proactive_audio(mut self, enabled: bool) -> Self {
        self.config = self.config.proactive_audio(enabled);
        self
    }

    /// Set media resolution for video/image input.
    pub fn media_resolution(mut self, res: MediaResolution) -> Self {
        self.config = self.config.media_resolution(res);
        self
    }

    // ── VAD & Activity ───────────────────────────────────────────────

    /// Configure server-side VAD.
    pub fn vad(mut self, detection: AutomaticActivityDetection) -> Self {
        self.config = self.config.server_vad(detection);
        self
    }

    /// Set activity handling mode (interrupts vs no-interruption).
    pub fn activity_handling(mut self, handling: ActivityHandling) -> Self {
        self.config = self.config.activity_handling(handling);
        self
    }

    /// Set turn coverage mode.
    pub fn turn_coverage(mut self, coverage: TurnCoverage) -> Self {
        self.config = self.config.turn_coverage(coverage);
        self
    }

    // ── Session Lifecycle ────────────────────────────────────────────

    /// Enable session resumption.
    pub fn session_resume(mut self, enabled: bool) -> Self {
        if enabled {
            self.config = self.config.session_resumption(None);
        }
        self
    }

    /// Enable context window compression.
    pub fn context_compression(mut self, trigger_tokens: u32, target_tokens: u32) -> Self {
        self.config = self.config
            .context_window_compression(target_tokens)
            .context_window_trigger_tokens(trigger_tokens);
        self
    }

    // ── Fast Lane Callbacks (sync, < 1ms) ────────────────────────────

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

    /// Called for input (user speech) transcription.
    pub fn on_input_transcript(mut self, f: impl Fn(&str, bool) + Send + Sync + 'static) -> Self {
        self.callbacks.on_input_transcript = Some(Box::new(f));
        self
    }

    /// Called for output (model speech) transcription.
    pub fn on_output_transcript(mut self, f: impl Fn(&str, bool) + Send + Sync + 'static) -> Self {
        self.callbacks.on_output_transcript = Some(Box::new(f));
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

    // ── Control Lane Callbacks (async, can block) ────────────────────

    /// Called when model is interrupted by barge-in. BLOCKING.
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
    pub fn on_tool_call<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<FunctionCall>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<Vec<FunctionResponse>>> + Send + 'static,
    {
        self.callbacks.on_tool_call = Some(Arc::new(move |calls| Box::pin(f(calls))));
        self
    }

    /// Called when model turn completes. BLOCKING.
    pub fn on_turn_complete<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_turn_complete = Some(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called when server sends GoAway. BLOCKING.
    pub fn on_go_away<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_go_away = Some(Arc::new(move |d| Box::pin(f(d))));
        self
    }

    /// Called when session connects (setup complete). BLOCKING.
    pub fn on_connected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_connected = Some(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called when session disconnects. BLOCKING.
    pub fn on_disconnected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_disconnected = Some(Arc::new(move |r| Box::pin(f(r))));
        self
    }

    /// Called on non-fatal errors.
    pub fn on_error<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_error = Some(Arc::new(move |e| Box::pin(f(e))));
        self
    }

    // ── Connect ──────────────────────────────────────────────────────

    /// Connect using a Google AI API key.
    pub async fn connect_google_ai(
        mut self,
        api_key: impl Into<String>,
    ) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        self.config = SessionConfig::new(api_key).merge_from(self.config);
        self.build_and_connect().await
    }

    /// Connect using Vertex AI credentials.
    pub async fn connect_vertex(
        mut self,
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        self.config = SessionConfig::from_vertex(project, location, access_token)
            .merge_from(self.config);
        self.build_and_connect().await
    }

    /// Connect using a pre-configured SessionConfig.
    pub async fn connect(
        self,
        config: SessionConfig,
    ) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        let mut builder = LiveSessionBuilder::new(config);
        if let Some(dispatcher) = self.dispatcher {
            builder = builder.dispatcher(dispatcher);
        }
        builder = builder.callbacks(self.callbacks);
        builder.connect().await
    }

    async fn build_and_connect(self) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        let mut builder = LiveSessionBuilder::new(self.config);
        if let Some(dispatcher) = self.dispatcher {
            builder = builder.dispatcher(dispatcher);
        }
        builder = builder.callbacks(self.callbacks);
        builder.connect().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_chain_compiles() {
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .voice(Voice::Kore)
            .instruction("Test")
            .temperature(0.7)
            .google_search()
            .transcription(true, true)
            .affective_dialog(true)
            .session_resume(true)
            .context_compression(4000, 2000)
            .on_audio(|_data| {})
            .on_text(|_t| {})
            .on_vad_start(|| {})
            .on_interrupted(|| async {})
            .on_turn_complete(|| async {})
            .on_go_away(|_d| async {})
            .on_connected(|| async {})
            .on_disconnected(|_r| async {})
            .on_error(|_e| async {});
        // Just verify the builder chain compiles
    }
}
```

**Step 2: Add to lib.rs and prelude**

In `crates/gemini-adk-fluent-rs/src/lib.rs`:
```rust
pub mod live;
```

In the prelude block:
```rust
pub use crate::live::Live;
```

**Step 3: Add bytes dependency**

Check if `bytes` is in gemini-adk-fluent-rs's Cargo.toml; if not, add it.

**Step 4: Build and test**

Run: `cargo build -p gemini-adk-fluent-rs && cargo test -p gemini-adk-fluent-rs`

**Step 5: Commit**

```
feat(gemini-adk-fluent-rs): add Live::builder() fluent API for callback-driven sessions
```

---

### Task 6: Integration Tests and Full Build

**Step 1: Run full workspace build**

Run: `cargo build --workspace`

Fix any compilation errors.

**Step 2: Run full workspace tests**

Run: `cargo test --workspace`

Fix any test failures.

**Step 3: Run clippy**

Run: `cargo clippy --workspace`

Fix any warnings.

**Step 4: Commit any fixes**

```
fix: resolve compilation issues in live session API
```

---

### Task 7: Update Research Pipeline Example

**Files:**
- Modify: `examples/agents/src/research_pipeline.rs`

Update the demo to demonstrate all new L2 capabilities including the `Live::builder()` API (showing the builder chain, not connecting since it's a dry-run demo).

Add section:
```rust
// ── Step 7: Live Session Builder (dry-run demo) ──
println!("\n--- Live Session Builder ---");
let _live = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .voice(Voice::Kore)
    .instruction(
        P::persona("research assistant")
        + P::task("Help with research queries")
        + P::guidelines(&["Be thorough", "Cite sources"])
    )
    .google_search()
    .transcription(true, true)
    .session_resume(true)
    .context_compression(4000, 2000)
    .on_audio(|_data| { /* queue to playback buffer */ })
    .on_text(|delta| print!("{delta}"))
    .on_interrupted(|| async { /* flush playback */ })
    .on_turn_complete(|| async { /* save turn */ });
println!("Live session builder configured (not connecting in dry-run).");
```

**Step 2: Build example**

Run: `cargo build -p agents-example`

**Step 3: Commit**

```
feat(examples): demonstrate Live::builder() API in research pipeline
```

---

## Critical File Map

| File | Layer | Action | Purpose |
|------|-------|--------|---------|
| `crates/gemini-genai-rs/src/session/mod.rs` | L0 | Modify | Add SendVideo, UpdateInstruction commands + trait methods |
| `crates/gemini-genai-rs/src/transport/codec.rs` | L0 | Modify | Encode new commands to wire format |
| `crates/gemini-adk-rs/src/agent_session.rs` | L1 | Modify | Add send_video, update_instruction forwarding |
| `crates/gemini-adk-rs/src/live/mod.rs` | L1 | Create | Live session module |
| `crates/gemini-adk-rs/src/live/callbacks.rs` | L1 | Create | EventCallbacks registry |
| `crates/gemini-adk-rs/src/live/processor.rs` | L1 | Create | Two-lane event processor |
| `crates/gemini-adk-rs/src/live/builder.rs` | L1 | Create | LiveSessionBuilder |
| `crates/gemini-adk-rs/src/live/handle.rs` | L1 | Create | LiveHandle |
| `crates/gemini-adk-rs/src/lib.rs` | L1 | Modify | Add `pub mod live` + re-exports |
| `crates/gemini-adk-fluent-rs/src/live.rs` | L2 | Create | Live::builder() fluent API |
| `crates/gemini-adk-fluent-rs/src/lib.rs` | L2 | Modify | Add `pub mod live` + prelude |
| `examples/agents/src/research_pipeline.rs` | - | Modify | Demonstrate Live::builder() |

## Estimated: ~800 LoC across 7 tasks
