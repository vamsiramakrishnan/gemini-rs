# LiveEvent Stream — Semantic Observation Infrastructure

**Date**: 2026-03-07
**Status**: Approved
**Scope**: L1 (gemini-adk), L2 (gemini-adk-fluent), Examples

## Problem

The SDK's only observation mechanism is callbacks. Callbacks conflate two concerns:

- **Action**: modify processor behavior (tool dispatch, PII redaction, context injection)
- **Observation**: report processor behavior (forward audio to browser, log phase transitions)

Every demo app registers 12+ callbacks that do nothing but forward events — ~150 lines of identical ceremony per app, ~1,200 lines total across 10 apps. This is structural: the SDK offers no other way to observe a session.

## Solution

Add `LiveEvent` as a first-class output of the L1 processor. The processor emits semantic events to a `broadcast::Sender<LiveEvent>` as a natural byproduct of its work. Subscribers consume via `LiveHandle::events()`. Zero-cost when no subscribers exist.

```
                        +---------------+
                        |   Processor   |
                        |   (3 lanes)   |
                        +-------+-------+
                                |
                   +------------+------------+
                   |            |            |
             Callbacks     LiveEvent      State
             (ACTION)    (OBSERVATION)    (DATA)
                   |            |            |
             modify         forward       query
             behavior       to sinks      at will
```

**Callbacks** remain for interceptors: `on_tool_call`, `before_tool_response`, `on_turn_boundary`, `instruction_template`, `instruction_amendment`. These modify behavior.

**LiveEvent** is the new observation channel. Any number of subscribers. Zero-cost when unused. This reports behavior.

**State** remains the queryable data store.

## LiveEvent Enum (L1)

```rust
// crates/gemini-adk/src/live/events.rs

#[derive(Debug, Clone)]
pub enum LiveEvent {
    // -- Fast-lane events (high frequency, sync emission) --
    Audio(Bytes),                                    // Refcounted, clone = pointer increment
    TextDelta(String),
    TextComplete(String),
    InputTranscript { text: String, is_final: bool },
    OutputTranscript { text: String, is_final: bool },
    Thought(String),
    VadStart,
    VadEnd,

    // -- Control-lane events (lower frequency, async emission) --
    Extraction { name: String, value: serde_json::Value },      // Top-level + flattened keys
    ExtractionError { name: String, error: String },
    PhaseTransition { from: String, to: String, reason: String },
    ToolExecution { name: String, args: serde_json::Value, result: serde_json::Value },
    TurnComplete,
    Interrupted,
    Connected,
    Disconnected { reason: Option<String> },
    Error(String),
    GoAway { time_left: Duration },

    // -- Periodic events (from telemetry infrastructure) --
    Telemetry(serde_json::Value),
    TurnMetrics { turn: u32, latency_ms: u32, prompt_tokens: u32, response_tokens: u32 },
}
```

### Why Bytes for Audio

`Bytes` is refcounted. Clone is ~2ns (pointer increment), not ~500ns (4KB memcpy). The fast lane already receives `Bytes` from L0 transport. The event passes it through with zero allocation.

### Why Flattened Extraction Events

The processor already auto-flattens extraction objects to individual State keys. Emitting flattened events (`"order.items"`, `"order.phase"`) means consumers get granular updates without manual iteration.

## Processor Emission Architecture (L1)

### Channel Creation

```rust
// In spawn_event_processor or builder.connect():
let (event_tx, _) = broadcast::channel::<LiveEvent>(4096);
```

Channel size 4096: at ~45 events/sec (40 audio + 5 lifecycle), this is ~90 seconds of buffer. Lagged receivers get `RecvError::Lagged` and skip forward (same pattern L0 uses for `SessionEvent`).

### Fast Lane Emission

One line per event, emitted AFTER the callback:

```rust
FastEvent::Audio(data) => {
    if !shared.interrupted.load(Ordering::Acquire) {
        if let Some(cb) = &callbacks.on_audio { cb(&data); }
        let _ = event_tx.send(LiveEvent::Audio(data));  // refcount increment only
    }
}
```

Audio suppressed during interruption — event stream sees the same world as callbacks.

### Control Lane Emission

| File | Point | Event |
|------|-------|-------|
| `extractors.rs` | After `state.set()` | `Extraction { name, value }` + flattened keys |
| `extractors.rs` | On failure | `ExtractionError { name, error }` |
| `lifecycle.rs` | After `machine.transition()` | `PhaseTransition { from, to, reason }` |
| `lifecycle.rs` | End of `handle_turn_complete` | `TurnMetrics { ... }` |
| `tool_handler.rs` | After `dispatcher.call_function()` | `ToolExecution { name, args, result }` |
| `main_loop.rs` | After handling each `ControlEvent` | `TurnComplete`, `Interrupted`, `Connected`, `Disconnected`, `Error`, `GoAway` |

Events emitted AFTER callbacks, so action precedes observation.

### Periodic Telemetry

New field on `ControlPlaneConfig`:

```rust
pub telemetry_interval: Option<Duration>,
```

When set, spawns a lightweight task that periodically emits `LiveEvent::Telemetry(snapshot)` and `LiveEvent::TurnMetrics` when the turn count advances.

## LiveHandle::events() (L1)

```rust
impl LiveHandle {
    pub fn events(&self) -> broadcast::Receiver<LiveEvent> {
        self.event_tx.subscribe()
    }
}
```

Mirrors existing `LiveHandle::subscribe()` which returns L0 `SessionEvent`s. Same API pattern, one abstraction level up.

## Builder Integration (L1)

```rust
impl LiveSessionBuilder {
    pub fn event_sender(mut self, tx: broadcast::Sender<LiveEvent>) -> Self {
        self.external_event_tx = Some(tx);
        self
    }

    pub fn telemetry_interval(mut self, interval: Duration) -> Self {
        self.telemetry_interval = Some(interval);
        self
    }
}
```

## L2 Fluent Integration

```rust
impl Live {
    pub fn events(mut self, tx: broadcast::Sender<LiveEvent>) -> Self {
        self.external_event_tx = Some(tx);
        self
    }

    pub fn telemetry_interval(mut self, interval: Duration) -> Self {
        self.telemetry_interval = Some(interval);
        self
    }
}
```

In `build_and_connect()`:

```rust
if let Some(tx) = self.external_event_tx {
    builder = builder.event_sender(tx);
}
if let Some(interval) = self.telemetry_interval {
    builder = builder.telemetry_interval(interval);
}
```

`LiveEvent` re-exported in prelude.

## Demo App: SessionBridge Rewrite

### Event Mapper

Written ONCE, used by ALL 13 apps:

```rust
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
        LiveEvent::PhaseTransition { from, to, reason } => Some(ServerMessage::PhaseChange { from, to, reason }),
        LiveEvent::ToolExecution { name, args, result } => Some(ServerMessage::ToolCallEvent {
            name, args: args.to_string(), result: result.to_string(),
        }),
        LiveEvent::Telemetry(stats) => Some(ServerMessage::Telemetry { stats }),
        LiveEvent::TurnMetrics { turn, latency_ms, prompt_tokens, response_tokens } =>
            Some(ServerMessage::TurnMetrics { turn, latency_ms, prompt_tokens, response_tokens }),
        _ => None,
    }
}
```

### Session Runner

```rust
impl SessionBridge {
    pub async fn run<F>(
        &self,
        app: &dyn CookbookApp,
        rx: &mut mpsc::UnboundedReceiver<ClientMessage>,
        configure: F,
    ) -> Result<(), AppError>
    where
        F: FnOnce(Live, &ClientMessage) -> Live,
    {
        let start = wait_for_start(rx).await?;
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let builder = configure(
            Live::builder().telemetry_interval(Duration::from_secs(2)),
            &start,
        );

        let handle = builder.connect(config).await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        self.send_connected();
        self.send_meta(app);

        let mut events = handle.events();
        let tx = self.tx.clone();
        let event_task = tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if let Some(msg) = map_event(event) {
                            if tx.send(msg).is_err() { break; }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        self.recv_loop(&handle, rx).await;
        event_task.abort();
        Ok(())
    }
}
```

## After: What Demo Apps Look Like

### Simple app (voice_chat.rs): ~15 lines

```rust
async fn handle_session(&self, tx: WsSender, mut rx: UnboundedReceiver<ClientMessage>) -> Result<(), AppError> {
    SessionBridge::new(tx).run(self, &mut rx, |live, start| {
        live.voice(resolve_voice(start.voice()))
            .instruction("You are a helpful voice assistant.")
            .transcription(true, true)
    }).await
}
```

### Complex app (restaurant.rs): ~500 lines (down from 1,418)

Everything remaining is domain logic: tool definitions, phase instructions, transition predicates, extraction schemas, computed state, watchers.

## Performance

| Metric | Value |
|--------|-------|
| broadcast::send with 0 receivers | ~10ns |
| broadcast::send with 1 receiver | ~30ns |
| Audio Bytes::clone | ~2ns (refcount, not deep copy) |
| Total overhead at 40 audio/sec | ~1.2us/sec = 0.00012% of one core |
| Memory: channel buffer | ~256KB fixed |

The event stream is FASTER than 12 separate callback dispatches because there's one emission point instead of 12 vtable dispatch + indirect call sequences.

## Backwards Compatibility

Zero breaking changes. All existing callbacks, builder methods, and patterns continue to work. Migration is incremental.

## Files Changed

| Layer | File | Change | Lines |
|-------|------|--------|-------|
| L1 | `live/events.rs` (NEW) | `LiveEvent` enum | ~80 |
| L1 | `live/processor.rs` | Create broadcast, pass to lanes | ~10 |
| L1 | `live/processor.rs` run_fast_lane | Emit 8 events | ~8 |
| L1 | `live/control_plane/main_loop.rs` | Accept event_tx, emit 6 events | ~10 |
| L1 | `live/control_plane/extractors.rs` | Accept event_tx, emit Extraction | ~15 |
| L1 | `live/control_plane/lifecycle.rs` | Accept event_tx+telemetry, emit Phase+TurnMetrics | ~10 |
| L1 | `live/control_plane/tool_handler.rs` | Accept event_tx, emit ToolExecution | ~5 |
| L1 | `live/handle.rs` | Add event_tx field, events() method | ~8 |
| L1 | `live/builder.rs` | event_sender(), telemetry_interval() | ~15 |
| L1 | `live/mod.rs` | Re-export LiveEvent | ~1 |
| L2 | `live/mod.rs` | fields + builder methods | ~15 |
| L2 | `live/connect.rs` | Pass to L1 builder | ~6 |
| L2 | `prelude.rs` | Re-export LiveEvent | ~1 |
| Demo App | `bridge.rs` | Rewrite with map_event + run() | ~80 |
| Demo App | 10 app files | Migrate to bridge.run() | ~-1,200 |
| **Total SDK** | | | **~185 added** |
| **Total Demo App** | | | **~1,200 deleted** |
