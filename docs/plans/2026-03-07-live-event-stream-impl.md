# LiveEvent Stream Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `LiveEvent` broadcast channel to the L1 processor so applications observe sessions via event stream instead of 12+ manual callbacks.

**Architecture:** The processor creates a `broadcast::Sender<LiveEvent>` at construction. Fast lane and control lane each receive a clone and emit events after processing. `LiveHandle::events()` returns a receiver. L2 `Live` builder exposes `events()` and `telemetry_interval()` fluent methods. The `SessionBridge` gains a `run()` method that subscribes to the event stream, maps events to `ServerMessage`, and handles the full session lifecycle.

**Tech Stack:** Rust, tokio broadcast channels, bytes::Bytes (refcounted audio), serde_json

---

### Task 1: Create LiveEvent enum (L1)

**Files:**
- Create: `crates/gemini-adk-rs/src/live/events.rs`
- Modify: `crates/gemini-adk-rs/src/live/mod.rs` (add module + re-export)

**Step 1: Create the LiveEvent enum**

```rust
// crates/gemini-adk-rs/src/live/events.rs
//! Semantic events emitted by the L1 processor.
//!
//! Subscribe via [`LiveHandle::events()`]. Zero-cost when no subscribers.

use std::time::Duration;

use bytes::Bytes;

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
    InputTranscript { text: String, is_final: bool },
    /// Model speech transcription.
    OutputTranscript { text: String, is_final: bool },
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
        name: String,
        value: serde_json::Value,
    },
    /// Extraction failed.
    ExtractionError {
        name: String,
        error: String,
    },
    /// Phase machine transitioned.
    PhaseTransition {
        from: String,
        to: String,
        reason: String,
    },
    /// Tool dispatched and result obtained.
    ToolExecution {
        name: String,
        args: serde_json::Value,
        result: serde_json::Value,
    },
    /// Model completed a conversational turn.
    TurnComplete,
    /// Model output interrupted by user speech.
    Interrupted,
    /// Session connected to Gemini.
    Connected,
    /// Session disconnected.
    Disconnected { reason: Option<String> },
    /// Unrecoverable error.
    Error(String),
    /// Server requesting session wind-down.
    GoAway { time_left: Duration },

    // -- Periodic events --

    /// Aggregated session telemetry snapshot.
    Telemetry(serde_json::Value),
    /// Per-turn latency and token metrics.
    TurnMetrics {
        turn: u32,
        latency_ms: u32,
        prompt_tokens: u32,
        response_tokens: u32,
    },
}
```

**Step 2: Register module and re-export in `crates/gemini-adk-rs/src/live/mod.rs`**

Add after line 16 (`pub(crate) mod control_plane;`):
```rust
pub mod events;
```

Add to the re-exports at the bottom (after line 55):
```rust
pub use events::LiveEvent;
```

**Step 3: Verify it compiles**

Run: `cargo check -p gemini-adk-rs`

**Step 4: Commit**

```
feat(adk): add LiveEvent semantic event enum
```

---

### Task 2: Add event_tx to processor and fast lane (L1)

**Files:**
- Modify: `crates/gemini-adk-rs/src/live/processor.rs`

**Step 1: Write test for LiveEvent emission from fast lane**

Add to `crates/gemini-adk-rs/src/live/processor.rs` in the `#[cfg(test)] mod tests` block (after the existing tests):

```rust
#[tokio::test]
async fn fast_lane_emits_live_events() {
    use super::super::events::LiveEvent;

    let callbacks = Arc::new(EventCallbacks::default());
    let (event_tx, _) = broadcast::channel(16);
    let event_rx = event_tx.subscribe();
    let writer: Arc<dyn SessionWriter> = Arc::new(crate::agent_session::NoOpSessionWriter);

    let (live_event_tx, mut live_event_rx) =
        broadcast::channel::<LiveEvent>(256);

    let (fast_handle, ctrl_handle) = spawn_event_processor(
        event_rx,
        callbacks,
        None,
        writer,
        vec![],
        State::new(),
        None,
        None,
        None,
        None,
        None,
        std::collections::HashMap::new(),
        ControlPlaneConfig::default(),
        live_event_tx,
    );

    // Send audio + text events
    let _ = event_tx.send(SessionEvent::AudioData(Bytes::from_static(b"pcm")));
    let _ = event_tx.send(SessionEvent::TextDelta("hello".into()));
    let _ = event_tx.send(SessionEvent::VoiceActivityStart);

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Collect emitted LiveEvents
    let mut events = Vec::new();
    while let Ok(ev) = live_event_rx.try_recv() {
        events.push(ev);
    }

    assert!(events.iter().any(|e| matches!(e, LiveEvent::Audio(_))));
    assert!(events.iter().any(|e| matches!(e, LiveEvent::TextDelta(t) if t == "hello")));
    assert!(events.iter().any(|e| matches!(e, LiveEvent::VadStart)));

    drop(event_tx);
    let _ = fast_handle.await;
    let _ = ctrl_handle.await;
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p gemini-adk-rs fast_lane_emits_live_events -- --nocapture`
Expected: FAIL — `spawn_event_processor` doesn't accept `live_event_tx` parameter yet.

**Step 3: Add `live_event_tx` parameter to `spawn_event_processor` and emit in fast lane**

In `crates/gemini-adk-rs/src/live/processor.rs`:

1. Add import at top (after line 13):
   ```rust
   use super::events::LiveEvent;
   ```

2. Add `live_event_tx: broadcast::Sender<LiveEvent>` as the last parameter of `spawn_event_processor` (after line 127, `control_plane: ControlPlaneConfig,`):
   ```rust
   live_event_tx: broadcast::Sender<LiveEvent>,
   ```

3. Pass `live_event_tx.clone()` to `run_fast_lane` (line 166):
   ```rust
   // Change:
   run_fast_lane(fast_rx, fast_callbacks, fast_shared).await;
   // To:
   run_fast_lane(fast_rx, fast_callbacks, fast_shared, fast_event_tx).await;
   ```
   And before the spawn (line 164), add:
   ```rust
   let fast_event_tx = live_event_tx.clone();
   ```

4. Pass `live_event_tx` to `run_control_lane` — add it after `control_plane` in the call at line 193:
   ```rust
   live_event_tx,
   ```

5. Update `run_fast_lane` signature (line 381) to accept the event sender:
   ```rust
   async fn run_fast_lane(
       mut rx: mpsc::Receiver<FastEvent>,
       callbacks: Arc<EventCallbacks>,
       shared: Arc<SharedState>,
       event_tx: broadcast::Sender<LiveEvent>,
   ) {
   ```

6. Add event emissions after each callback dispatch in `run_fast_lane`:

   After Audio callback (line 393, after the `if let Some(cb)` block, still inside the `!interrupted` check):
   ```rust
   let _ = event_tx.send(LiveEvent::Audio(data));
   ```

   After Text callback (line 399):
   ```rust
   let _ = event_tx.send(LiveEvent::TextDelta(delta));
   ```
   Note: `delta` is moved, so emit BEFORE the string is consumed, or change the match to borrow. Actually the match moves `delta` into the arm. We need to clone for the event or restructure. Simplest: clone for the callback, send original to event:
   ```rust
   FastEvent::Text(delta) => {
       if let Some(cb) = &callbacks.on_text {
           cb(&delta);
       }
       let _ = event_tx.send(LiveEvent::TextDelta(delta));
   }
   ```
   This works because `cb` borrows `&delta`, then `delta` is moved into the event.

   Same pattern for all other fast-lane events:
   ```rust
   FastEvent::TextComplete(text) => {
       if let Some(cb) = &callbacks.on_text_complete { cb(&text); }
       let _ = event_tx.send(LiveEvent::TextComplete(text));
   }
   FastEvent::InputTranscript(text) => {
       if let Some(cb) = &callbacks.on_input_transcript { cb(&text, false); }
       let _ = event_tx.send(LiveEvent::InputTranscript { text, is_final: false });
   }
   FastEvent::OutputTranscript(text) => {
       if let Some(cb) = &callbacks.on_output_transcript { cb(&text, false); }
       let _ = event_tx.send(LiveEvent::OutputTranscript { text, is_final: false });
   }
   FastEvent::Thought(text) => {
       if let Some(cb) = &callbacks.on_thought { cb(&text); }
       let _ = event_tx.send(LiveEvent::Thought(text));
   }
   FastEvent::VadStart => {
       if let Some(cb) = &callbacks.on_vad_start { cb(); }
       let _ = event_tx.send(LiveEvent::VadStart);
   }
   FastEvent::VadEnd => {
       if let Some(cb) = &callbacks.on_vad_end { cb(); }
       let _ = event_tx.send(LiveEvent::VadEnd);
   }
   ```

   `FastEvent::Phase` and `FastEvent::Interrupted` do NOT emit LiveEvents (Phase is L0-level, Interrupted is emitted from control lane).

**Step 4: Fix existing tests**

The existing `fast_lane_routes_audio` and `interrupt_suppresses_audio` tests call `spawn_event_processor` without the new `live_event_tx` parameter. Add it:

In each test, before the `spawn_event_processor` call, add:
```rust
let (live_event_tx, _) = broadcast::channel::<super::super::events::LiveEvent>(16);
```
And add `live_event_tx,` as the last argument to `spawn_event_processor`.

**Step 5: Run tests**

Run: `cargo test -p gemini-adk-rs -- fast_lane --nocapture`
Expected: All 3 tests pass.

**Step 6: Commit**

```
feat(adk): emit LiveEvents from fast lane processor
```

---

### Task 3: Emit LiveEvents from control lane (L1)

**Files:**
- Modify: `crates/gemini-adk-rs/src/live/control_plane/main_loop.rs`
- Modify: `crates/gemini-adk-rs/src/live/control_plane/extractors.rs`
- Modify: `crates/gemini-adk-rs/src/live/control_plane/lifecycle.rs`
- Modify: `crates/gemini-adk-rs/src/live/control_plane/tool_handler.rs`

**Step 1: Add `event_tx` parameter to `run_control_lane`**

In `main_loop.rs`, add to the function signature (after `control_plane: ControlPlaneConfig,`):
```rust
event_tx: tokio::sync::broadcast::Sender<crate::live::events::LiveEvent>,
```

Add import at top:
```rust
use crate::live::events::LiveEvent;
```

Add lifecycle event emissions in the match arms:

After `ControlEvent::Interrupted` handler (after line 102, `shared.interrupted.store(false, ...)`):
```rust
let _ = event_tx.send(LiveEvent::Interrupted);
```

After `ControlEvent::TurnComplete` / `handle_turn_complete` call (after line 123):
```rust
let _ = event_tx.send(LiveEvent::TurnComplete);
```

After `ControlEvent::GoAway` handler (after the callback dispatch):
```rust
let _ = event_tx.send(LiveEvent::GoAway { time_left: duration });
```

After `ControlEvent::Connected` handler:
```rust
let _ = event_tx.send(LiveEvent::Connected);
```

After `ControlEvent::Disconnected` handler:
```rust
let _ = event_tx.send(LiveEvent::Disconnected { reason });
```
Note: `reason` is moved into the callback. Clone it before the callback or restructure. Use:
```rust
ControlEvent::Disconnected(reason) => {
    let _ = event_tx.send(LiveEvent::Disconnected { reason: reason.clone() });
    if let Some(cb) = &callbacks.on_disconnected {
        dispatch_callback!(callbacks.on_disconnected_mode, cb(reason));
    }
}
```

After `ControlEvent::Error` handler (same pattern — emit before callback):
```rust
ControlEvent::Error(err) => {
    let _ = event_tx.send(LiveEvent::Error(err.clone()));
    if let Some(cb) = &callbacks.on_error {
        dispatch_callback!(callbacks.on_error_mode, cb(err));
    }
}
```

Pass `event_tx` to `handle_turn_complete` and `handle_tool_calls`:
```rust
// handle_turn_complete call: add &event_tx as last arg
handle_turn_complete(
    &callbacks, &writer, &shared, &extractors, &state,
    &computed, &phase_machine, &watchers, &temporal,
    &mut transcript_buffer, &mut extraction_turn_tracker,
    &mut control_plane, &event_tx,
).await;

// handle_tool_calls call: add &event_tx as last arg
handle_tool_calls(
    calls, &callbacks, &dispatcher, &writer, &state,
    &phase_machine, &mut transcript_buffer, &execution_modes,
    &background_tracker, &extractors, &event_tx,
).await;
```

Also pass to `run_extractors_with_window` in the `GenerationComplete` arm:
```rust
run_extractors_with_window(
    &gen_extractors, &mut transcript_buffer, &state, &callbacks, true, &event_tx,
).await;
```

**Step 2: Add `event_tx` to extractors.rs**

In both `run_extractors` and `run_extractors_with_window`, add parameter:
```rust
event_tx: &broadcast::Sender<LiveEvent>,
```

Add import:
```rust
use tokio::sync::broadcast;
use crate::live::events::LiveEvent;
```

After `state.set(&name, &value)` and the auto-flatten loop (line 71 area), emit:
```rust
// Emit top-level extraction event
let _ = event_tx.send(LiveEvent::Extraction { name: name.clone(), value: value.clone() });
// Emit flattened key events
if let Some(obj) = value.as_object() {
    for (field, val) in obj {
        if !val.is_null() {
            let _ = event_tx.send(LiveEvent::Extraction {
                name: format!("{name}.{field}"),
                value: val.clone(),
            });
        }
    }
}
```

For errors:
```rust
let _ = event_tx.send(LiveEvent::ExtractionError { name: name.clone(), error: error.clone() });
```

Do the same in `run_extractors_with_window`.

**Step 3: Add `event_tx` to lifecycle.rs**

Add parameter to `handle_turn_complete`:
```rust
event_tx: &broadcast::Sender<crate::live::events::LiveEvent>,
```

After the phase transition block (where `TransitionResult` is obtained), emit:
```rust
if let Some(ref result) = transition_result {
    let _ = event_tx.send(LiveEvent::PhaseTransition {
        from: result.from.clone(),
        to: result.to.clone(),
        reason: result.reason.clone(),
    });
}
```

Pass `event_tx` through to any `run_extractors` calls within lifecycle.rs.

**Step 4: Add `event_tx` to tool_handler.rs**

Add parameter to `handle_tool_calls`:
```rust
event_tx: &broadcast::Sender<crate::live::events::LiveEvent>,
```

After each tool is dispatched (in the auto-dispatch loop, after `disp.call_function` returns), emit:
```rust
let _ = event_tx.send(LiveEvent::ToolExecution {
    name: call.name.clone(),
    args: call.args.clone(),
    result: result.clone(),
});
```
Where `result` is the `serde_json::Value` from `call_function`.

Pass `event_tx` through to `run_extractors` calls within tool_handler.rs.

**Step 5: Verify compilation**

Run: `cargo check -p gemini-adk-rs`

**Step 6: Run all tests**

Run: `cargo test -p gemini-adk-rs --lib`

**Step 7: Commit**

```
feat(adk): emit LiveEvents from control lane — extraction, phases, tools, lifecycle
```

---

### Task 4: Add LiveHandle::events() and builder integration (L1)

**Files:**
- Modify: `crates/gemini-adk-rs/src/live/handle.rs`
- Modify: `crates/gemini-adk-rs/src/live/builder.rs`
- Modify: `crates/gemini-adk-rs/src/live/processor.rs` (ControlPlaneConfig)

**Step 1: Write test**

Add to `crates/gemini-adk-rs/src/live/builder.rs` tests:

```rust
#[test]
fn builder_with_telemetry_interval() {
    let config = SessionConfig::new("test-key");
    let builder = LiveSessionBuilder::new(config)
        .telemetry_interval(std::time::Duration::from_secs(2));
    assert!(builder.telemetry_interval.is_some());
}
```

**Step 2: Add event_tx to LiveHandle**

In `crates/gemini-adk-rs/src/live/handle.rs`:

Add field to struct (after `telemetry`):
```rust
event_tx: broadcast::Sender<super::events::LiveEvent>,
```

Update `new()` to accept and store it:
```rust
pub(crate) fn new(
    session: SessionHandle,
    fast_task: JoinHandle<()>,
    ctrl_task: JoinHandle<()>,
    state: State,
    telemetry: Arc<SessionTelemetry>,
    event_tx: broadcast::Sender<super::events::LiveEvent>,
) -> Self {
    Self {
        session,
        _fast_task: Arc::new(fast_task),
        _ctrl_task: Arc::new(ctrl_task),
        state,
        telemetry,
        event_tx,
    }
}
```

Add public method:
```rust
/// Subscribe to semantic events from the processor.
///
/// Returns a broadcast receiver. Call multiple times for independent
/// subscribers. Zero-cost when no subscribers exist.
pub fn events(&self) -> broadcast::Receiver<super::events::LiveEvent> {
    self.event_tx.subscribe()
}
```

**Step 3: Add fields and methods to LiveSessionBuilder**

In `crates/gemini-adk-rs/src/live/builder.rs`:

Add fields to struct (after `tool_advisory: bool,`):
```rust
telemetry_interval: Option<std::time::Duration>,
```

Initialize in `new()`:
```rust
telemetry_interval: None,
```

Add builder method:
```rust
/// Set the periodic telemetry emission interval.
///
/// When set, the processor periodically emits `LiveEvent::Telemetry`
/// and `LiveEvent::TurnMetrics` to the event stream.
pub fn telemetry_interval(mut self, interval: std::time::Duration) -> Self {
    self.telemetry_interval = Some(interval);
    self
}
```

**Step 4: Wire event_tx through `connect()`**

In `builder.rs` `connect()` method:

After creating `telemetry` (line 267), create the LiveEvent broadcast channel:
```rust
use super::events::LiveEvent;

let (live_event_tx, _) = broadcast::channel::<LiveEvent>(4096);
```

Pass `live_event_tx.clone()` to `spawn_event_processor` (add as last arg after `control_plane`):
```rust
let (fast_handle, ctrl_handle) = spawn_event_processor(
    event_rx, callbacks, self.dispatcher, writer,
    self.extractors, state.clone(), self.computed,
    phase_machine_mutex, self.watchers, temporal_arc,
    Some(background_tracker), self.execution_modes,
    control_plane, live_event_tx.clone(),
);
```

Spawn periodic telemetry task if interval is set (after `spawn_event_processor`):
```rust
if let Some(interval) = self.telemetry_interval {
    let telem_tx = live_event_tx.clone();
    let telem_ref = telemetry.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        let mut prev_turns = 0u64;
        loop {
            tick.tick().await;
            let snap = telem_ref.snapshot();
            if let Some(obj) = snap.as_object() {
                let tc = obj.get("turn_count").and_then(|v| v.as_u64()).unwrap_or(0);
                if tc > prev_turns {
                    let latency = obj.get("last_latency_ms").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    let prompt = obj.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    let response = obj.get("response_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    let _ = telem_tx.send(LiveEvent::TurnMetrics {
                        turn: tc as u32, latency_ms: latency,
                        prompt_tokens: prompt, response_tokens: response,
                    });
                    prev_turns = tc;
                }
            }
            if telem_tx.send(LiveEvent::Telemetry(snap)).is_err() { break; }
        }
    });
}
```

Pass `live_event_tx` to `LiveHandle::new()`:
```rust
Ok(LiveHandle::new(
    session, fast_handle, ctrl_handle, state, telemetry, live_event_tx,
))
```

**Step 5: Run tests**

Run: `cargo test -p gemini-adk-rs --lib`

**Step 6: Commit**

```
feat(adk): add LiveHandle::events() and telemetry_interval builder method
```

---

### Task 5: L2 fluent integration

**Files:**
- Modify: `crates/gemini-adk-fluent-rs/src/live/mod.rs`
- Modify: `crates/gemini-adk-fluent-rs/src/live/connect.rs`
- Modify: `crates/gemini-adk-fluent-rs/src/lib.rs` (prelude)

**Step 1: Write compile test**

Add to `crates/gemini-adk-fluent-rs/src/live/mod.rs` tests:

```rust
#[test]
fn builder_with_live_events_compiles() {
    let (tx, _) = tokio::sync::broadcast::channel::<gemini_adk_rs::live::LiveEvent>(16);
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .instruction("Test events")
        .events(tx)
        .telemetry_interval(Duration::from_secs(2));
}
```

**Step 2: Add fields and methods to Live struct**

In `crates/gemini-adk-fluent-rs/src/live/mod.rs`:

Add fields to `Live` struct (after `tool_advisory: bool,` at line 125):
```rust
pub(crate) external_event_tx: Option<tokio::sync::broadcast::Sender<gemini_adk_rs::live::LiveEvent>>,
pub(crate) telemetry_interval: Option<Duration>,
```

Initialize in `builder()` (add after `tool_advisory: true,`):
```rust
external_event_tx: None,
telemetry_interval: None,
```

Add builder methods (in a new `impl Live` block or the existing config.rs):

```rust
/// Subscribe to all semantic events via a broadcast sender.
///
/// The processor emits `LiveEvent`s as it works — audio, text,
/// extractions, phase transitions, tool executions, lifecycle events.
/// Subscribe to this channel to observe session activity without
/// registering individual callbacks.
pub fn events(mut self, tx: tokio::sync::broadcast::Sender<gemini_adk_rs::live::LiveEvent>) -> Self {
    self.external_event_tx = Some(tx);
    self
}

/// Set the periodic telemetry emission interval.
///
/// When set, the processor emits `LiveEvent::Telemetry` snapshots
/// and `LiveEvent::TurnMetrics` at this rate.
pub fn telemetry_interval(mut self, interval: Duration) -> Self {
    self.telemetry_interval = Some(interval);
    self
}
```

**Step 3: Wire through build_and_connect**

In `crates/gemini-adk-fluent-rs/src/live/connect.rs`, add before `builder.connect().await` (line 114):

```rust
if let Some(interval) = self.telemetry_interval {
    builder = builder.telemetry_interval(interval);
}
```

Note: The `external_event_tx` is NOT passed to the L1 builder in this implementation. Instead, after `builder.connect().await` returns the `LiveHandle`, the application subscribes via `handle.events()`. This keeps the L1 API clean. If we later want pre-connect subscription, we add `event_sender()` to L1 builder then.

**Step 4: Add LiveEvent to prelude**

In `crates/gemini-adk-fluent-rs/src/lib.rs`, add `LiveEvent` to the `gemini_adk_rs::live::` import (line 84):

```rust
pub use gemini_adk_rs::live::{
    CallbackMode, DefaultResultFormatter, EventCallbacks, ExtractionTrigger, FsPersistence,
    LiveEvent, LiveHandle, LiveSessionBuilder, LlmExtractor, MemoryPersistence, NeedsFulfillment,
    // ... rest unchanged
};
```

**Step 5: Run tests**

Run: `cargo test -p gemini-adk-fluent-rs --lib`

**Step 6: Run full workspace check**

Run: `just check`

**Step 7: Commit**

```
feat(fluent): wire LiveEvent stream to L2 builder with events() and telemetry_interval()
```

---

### Task 6: Rewrite SessionBridge with run() method

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/bridge.rs`

**Step 1: Add map_event function**

Add at the top of `bridge.rs` (after imports):

```rust
use gemini_adk_fluent_rs::prelude::LiveEvent;

/// Map a LiveEvent to a ServerMessage for the demo WebSocket transport.
///
/// Written once, used by all demo apps via `SessionBridge::run()`.
fn map_event(event: LiveEvent) -> Option<ServerMessage> {
    match event {
        LiveEvent::Audio(data) => Some(ServerMessage::Audio { data: data.to_vec() }),
        LiveEvent::TextDelta(text) => Some(ServerMessage::TextDelta { text }),
        LiveEvent::TextComplete(text) => Some(ServerMessage::TextComplete { text }),
        LiveEvent::InputTranscript { text, .. } => Some(ServerMessage::InputTranscription { text }),
        LiveEvent::OutputTranscript { text, .. } => Some(ServerMessage::OutputTranscription { text }),
        LiveEvent::Thought(text) => Some(ServerMessage::Thought { text }),
        LiveEvent::VadStart => Some(ServerMessage::VoiceActivityStart),
        LiveEvent::VadEnd => Some(ServerMessage::VoiceActivityEnd),
        LiveEvent::TurnComplete => Some(ServerMessage::TurnComplete),
        LiveEvent::Interrupted => Some(ServerMessage::Interrupted),
        LiveEvent::Error(message) => Some(ServerMessage::Error { message }),
        LiveEvent::Extraction { name, value } => Some(ServerMessage::StateUpdate { key: name, value }),
        LiveEvent::ExtractionError { .. } => None,
        LiveEvent::PhaseTransition { from, to, reason } => Some(ServerMessage::PhaseChange { from, to, reason }),
        LiveEvent::ToolExecution { name, args, result } => Some(ServerMessage::ToolCallEvent {
            name,
            args: args.to_string(),
            result: result.to_string(),
        }),
        LiveEvent::Telemetry(stats) => Some(ServerMessage::Telemetry { stats }),
        LiveEvent::TurnMetrics { turn, latency_ms, prompt_tokens, response_tokens } => {
            Some(ServerMessage::TurnMetrics { turn, latency_ms, prompt_tokens, response_tokens })
        }
        _ => None,
    }
}
```

**Step 2: Add `run()` method to SessionBridge**

```rust
/// Run a complete demo session.
///
/// Waits for Start -> builds config -> connects with domain config ->
/// subscribes to LiveEvent stream -> forwards events to browser ->
/// forwards browser input to session -> cleans up.
///
/// The closure receives a `Live` builder and the Start message.
/// Add domain config (instruction, tools, phases, extraction) and
/// return the builder. Everything else is handled.
pub async fn run<F>(
    &self,
    app: &dyn CookbookApp,
    rx: &mut mpsc::UnboundedReceiver<crate::app::ClientMessage>,
    configure: F,
) -> Result<(), crate::app::AppError>
where
    F: FnOnce(Live, &crate::app::ClientMessage) -> Live,
{
    use crate::app::{AppError, ClientMessage};
    use std::time::Duration;
    use tokio::sync::broadcast;

    // 1. Wait for Start
    let start = crate::apps::wait_for_start(rx)
        .await
        .map_err(|e| AppError::Connection(e.to_string()))?;

    // 2. Build config
    let config = crate::apps::build_session_config(
        match &start {
            ClientMessage::Start { model, .. } => model.as_deref(),
            _ => None,
        },
    )
    .map_err(|e| AppError::Connection(e.to_string()))?;

    // 3. Let app configure builder (DOMAIN ONLY)
    let builder = configure(
        Live::builder().telemetry_interval(Duration::from_secs(2)),
        &start,
    );

    // 4. Connect
    let handle = builder
        .connect(config)
        .await
        .map_err(|e| AppError::Connection(e.to_string()))?;

    // 5. Signal browser
    self.send_connected();
    self.send_meta(app);

    // 6. Spawn event forwarder (LiveEvent -> ServerMessage -> WebSocket)
    let mut events = handle.events();
    let tx = self.tx.clone();
    let event_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Some(msg) = map_event(event) {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // 7. Forward browser -> Gemini (existing recv_loop)
    self.recv_loop(&handle, rx).await;

    // 8. Cleanup
    event_task.abort();
    Ok(())
}
```

**Step 3: Verify compilation**

Run: `cargo check -p gemini-genai-ui`

Note: The `run()` method references `crate::apps::wait_for_start` and `crate::apps::build_session_config`. Verify these are accessible (they may be pub functions in the apps module). If not, make them pub or move them to a shared location.

**Step 4: Commit**

```
feat(examples): add SessionBridge::run() with LiveEvent stream subscription
```

---

### Task 7: Migrate simple demo apps (voice_chat, text_chat, tool_calling)

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/voice_chat.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/text_chat.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/tool_calling.rs`

These 3 apps already use `SessionBridge`. Migrate them to `bridge.run()` to validate the pattern.

**Step 1: Rewrite voice_chat.rs**

The entire `handle_session` body becomes:
```rust
async fn handle_session(
    &self,
    tx: WsSender,
    mut rx: mpsc::UnboundedReceiver<ClientMessage>,
) -> Result<(), AppError> {
    SessionBridge::new(tx).run(self, &mut rx, |live, start| {
        let voice = match start {
            ClientMessage::Start { voice, .. } => resolve_voice(voice.as_deref()),
            _ => Voice::Kore,
        };
        live.voice(voice)
            .instruction("You are a helpful voice assistant. Keep your responses concise and natural.")
            .transcription(true, true)
    }).await
}
```

**Step 2: Rewrite text_chat.rs similarly**

**Step 3: Rewrite tool_calling.rs**

This one has an `on_tool_call` callback — it stays because it's ACTION (custom dispatch), not observation. But all the observation callbacks (audio, text, etc.) are eliminated.

**Step 4: Run and verify**

Run: `cargo check -p gemini-genai-ui`

**Step 5: Commit**

```
refactor(examples): migrate voice_chat, text_chat, tool_calling to bridge.run()
```

---

### Task 8: Migrate complex demo apps (batch 1: guardrails, playbook, all_config)

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/guardrails.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/playbook.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/all_config.rs`

For each app:
1. Remove ALL manual callback wiring (12+ `tx.clone()` + callback registrations)
2. Remove manual recv_loop and telemetry spawning
3. Remove `on_extracted_concurrent` broadcasting (event stream handles this)
4. Remove phase `on_enter` broadcasting (event stream handles this)
5. Keep ACTION callbacks: `on_tool_call`, `before_tool_response`, `instruction_amendment`, `on_turn_boundary`
6. Wrap everything in `bridge.run()`

**Step 1: Rewrite each app** following the voice_chat pattern but preserving domain logic.

**Step 2: Verify compilation**

Run: `cargo check -p gemini-genai-ui`

**Step 3: Run tests**

Run: `cargo test -p gemini-genai-ui --lib`

**Step 4: Commit**

```
refactor(examples): migrate guardrails, playbook, all_config to bridge.run()
```

---

### Task 9: Migrate complex demo apps (batch 2: restaurant, support, debt_collection)

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/restaurant.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/support.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`

Same pattern as Task 8. These are the largest apps (~1,400+ lines each) with the most ceremony to remove.

**Step 1-4: Same as Task 8**

**Step 5: Commit**

```
refactor(examples): migrate restaurant, support, debt_collection to bridge.run()
```

---

### Task 10: Migrate remaining demo apps (clinic, call_screening)

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/clinic.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/call_screening.rs`

Same pattern.

**Step 1: Rewrite both apps**

**Step 2: Verify compilation and tests**

Run: `cargo check -p gemini-genai-ui && cargo test -p gemini-genai-ui --lib`

**Step 3: Commit**

```
refactor(examples): migrate clinic, call_screening to bridge.run()
```

---

### Task 11: Final verification and cleanup

**Files:**
- All modified files

**Step 1: Run full workspace check**

Run: `just check`
Expected: All checks pass (fmt, lint with -D warnings, all tests).

**Step 2: Verify no unused imports or dead code**

The migration should have removed many `use base64::Engine` and `tx.clone()` patterns. Clippy will catch any leftovers.

**Step 3: Remove old wire_live callback code from bridge.rs if no longer used**

If all apps use `bridge.run()`, the old `wire_live()` method may be dead code. Check if any app still uses it. If not, remove it.

**Step 4: Commit**

```
chore: clean up unused imports and dead code after LiveEvent migration
```

**Step 5: Final check**

Run: `just check`
Expected: Clean pass.
