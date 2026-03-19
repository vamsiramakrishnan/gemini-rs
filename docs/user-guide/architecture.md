# Architecture Overview

This guide explains how the gemini-live-rs workspace is structured, how data
flows through the system, and how to decide which layer to build on.

## The Three-Crate Stack

The workspace is organized into three crates, each adding a layer of
abstraction on top of the one below:

```
+--------------------------------------------------+
|  gemini-adk-fluent  (L2)  — Fluent DX                |
|  Live::builder(), operator algebra, composition   |
+--------------------------------------------------+
|  gemini-adk  (L1)  — Agent Runtime                   |
|  LiveSessionBuilder, callbacks, tool dispatch,    |
|  state, phases, watchers, extractors, telemetry   |
+--------------------------------------------------+
|  gemini-live  (L0)  — Wire Protocol                 |
|  SessionHandle, SessionConfig, Transport, Codec,  |
|  AuthProvider, events, commands, VAD, buffers     |
+--------------------------------------------------+
|          Gemini Multimodal Live API               |
|       (WebSocket, full-duplex audio/text)         |
+--------------------------------------------------+
```

### L0: gemini-live

The wire protocol crate. Maps 1:1 to the Gemini API surface. No application
logic, no opinions about how you structure your app.

What it provides:
- `SessionConfig` for building the setup message (model, voice, tools, VAD)
- `SessionHandle` for sending commands and subscribing to events
- `ConnectBuilder` for establishing the WebSocket connection
- `Transport` / `Codec` / `AuthProvider` traits for pluggable I/O
- `SessionEvent` enum (17 variants) for everything the server can send
- `SessionCommand` enum (9 variants) for everything you can send
- `SessionPhase` FSM with validated transitions
- Audio buffers (lock-free SPSC ring buffer, adaptive jitter buffer)
- Client-side VAD (Voice Activity Detection)

### L1: gemini-adk

The agent runtime. Adds the event processing loop, typed callbacks, automatic
tool dispatch, state management, and the phase machine.

What it provides:
- `LiveSessionBuilder` to wire up config + callbacks + tools in one place
- `EventCallbacks` with typed fast-lane (sync) and control-lane (async) hooks
- `ToolDispatcher` with `ToolFunction`, `StreamingTool`, `InputStreamingTool`
- `State` (concurrent key-value store with prefix scoping and delta tracking)
- `PhaseMachine` for multi-step conversation flows
- `WatcherRegistry` for state-reactive triggers
- `TranscriptBuffer` for accumulating conversation history
- `TurnExtractor` / `LlmExtractor` for structured data extraction
- `LiveHandle` as the runtime API surface
- `SessionSignals` + `SessionTelemetry` for observability
- `TextAgent` trait and combinators for text-based LLM pipelines

### L2: gemini-adk-fluent

The fluent developer experience layer. Wraps L1 in a chainable builder API
and adds operator-algebra composition (S, C, T, P, M modules).

What it provides:
- `Live::builder()` with method chaining for the entire configuration surface
- `.phase()` / `.watch()` / `.computed()` sub-builders that flow back naturally
- `.connect_google_ai()` / `.connect_vertex()` for one-line connection
- `T::simple()`, `T::google_search()` for composable tool registration
- `S`, `C`, `T`, `P`, `M` operator modules with `|` composition
- `let_clone!` macro for reducing `Arc::clone` boilerplate in closures
- Test utilities and mock helpers

## Data Flow

Here is how data moves through the system during a live session:

```
  Client App                    gemini-live-rs                   Gemini API
  ----------                    --------------                   ----------

  Microphone
      |
      v
  [PCM16 16kHz] --send_audio()--> SessionHandle --WebSocket--> Gemini Live
                                       |                          |
                                  SessionCommand                  |
                                  (mpsc channel)                  |
                                       |                          |
                                  Transport::send()               |
                                       |                          v
                                       |                    Model processes
                                       |                    audio/text/tools
                                       |                          |
                                  Transport::recv()               |
                                       |                          |
                                  Codec::decode()                 |
                                       |                          |
                                  SessionEvent          <--- WebSocket frames
                                  (broadcast channel)
                                       |
                              +--------+--------+
                              |        |        |
                          Fast Lane  Ctrl Lane  Telemetry Lane
                              |        |        |
                          on_audio  on_tool  SessionSignals
                          on_text   phases   SessionTelemetry
                          on_vad    extract
                              |        |
                              v        v
                          Speaker   State
                          Display   Updates
```

**Outbound path**: Your app calls `send_audio()` / `send_text()` on the
`LiveHandle` (L1/L2) or `SessionHandle` (L0). These become `SessionCommand`
variants sent through an mpsc channel to the transport loop, which encodes
them via the `Codec` and sends them over the WebSocket.

**Inbound path**: The transport loop receives WebSocket frames, decodes them
via the `Codec` into `SessionEvent` variants, and broadcasts them. The
three-lane processor (L1) routes each event to the appropriate lane.

## Three-Lane Processor

Audio arrives at 40-100 events per second. Tool dispatch can take 1-30
seconds. Sharing one processing loop causes audio stutter during tool
execution. The solution: split the event stream into three priority lanes.

### Fast Lane (sync, <1ms)

Handles latency-sensitive events with sync callbacks that must never block:

| Event | Callback |
|-------|----------|
| `AudioData` | `on_audio(&Bytes)` |
| `TextDelta` | `on_text(&str)` |
| `TextComplete` | `on_text_complete(&str)` |
| `InputTranscription` | `on_input_transcript(&str, bool)` |
| `OutputTranscription` | `on_output_transcript(&str, bool)` |
| `VoiceActivityStart` | `on_vad_start()` |
| `VoiceActivityEnd` | `on_vad_end()` |
| `Interrupted` | Sets `interrupted` flag, stops forwarding audio |

Fast lane callbacks are `Fn` (not `FnMut`, not `async`). If your callback
takes longer than 1ms, audio playback will stutter.

### Control Lane (async, can block)

Handles events that require I/O, state mutation, or multi-step processing:

| Event | Callback |
|-------|----------|
| `ToolCall` | `on_tool_call` (auto-dispatch or manual) |
| `ToolCallCancelled` | Cancels pending tool tasks |
| `Interrupted` | `on_interrupted()` |
| `TurnComplete` | Extractors, phase transitions, `on_turn_complete()` |
| `GoAway` | `on_go_away(Duration)` |
| `Connected` | `on_connected()` |
| `Disconnected` | `on_disconnected(Option<String>)` |
| `Error` | `on_error(String)` |

The control lane also owns the `TranscriptBuffer` (no `Arc<Mutex<>>`) and
runs extractors concurrently via `join_all`.

### Telemetry Lane (async, debounced)

Runs on its own broadcast receiver. Collects `SessionSignals` (activity
timestamps, timing, token usage) and `SessionTelemetry` (atomic counters for
audio chunks, tool calls, interruptions, latency tracking, token counts).
Flushes periodically (100ms debounce) with zero overhead on the hot path.

The telemetry lane also handles `UsageMetadata` events from the Gemini API,
recording prompt/response/cached/thoughts token counts in both SessionSignals
(as `session:` state keys) and SessionTelemetry (as atomic counters). The
`.on_usage()` callback fires here for real-time token observation.

### The Router

The router is the zero-work dispatcher that sits between the broadcast
channel and the two processing lanes. It pattern-matches each `SessionEvent`
and sends it to the correct lane(s) via mpsc channels. No session signals,
no telemetry, no allocations on the hot path.

## Key Traits

| Trait | Crate | Purpose |
|-------|-------|---------|
| `Transport` | L0 (`gemini_live::transport::ws`) | Bidirectional byte transport (WebSocket or mock) |
| `Codec` | L0 (`gemini_live::transport::codec`) | Encode commands / decode server messages (JSON default) |
| `AuthProvider` | L0 (`gemini_live::transport::auth`) | URL construction + auth headers (Google AI / Vertex AI) |
| `SessionWriter` | L0 (`gemini_live::session`) | Send audio/text/video/tools/instructions (trait object safe) |
| `SessionReader` | L0 (`gemini_live::session`) | Subscribe to events, observe phase |
| `ToolFunction` | L1 (`gemini_adk::tool`) | One-shot tool: `call(args) -> Result<Value>` |
| `StreamingTool` | L1 (`gemini_adk::tool`) | Background tool yielding multiple results |
| `InputStreamingTool` | L1 (`gemini_adk::tool`) | Tool receiving live input while running |
| `TurnExtractor` | L1 (`gemini_adk::live::extractor`) | Extract structured data from transcript window |
| `TextAgent` | L1 (`gemini_adk::text`) | Text-based LLM agent (`generate()`, not Live) |
| `BaseLlm` | L1 (`gemini_adk::llm`) | LLM abstraction for `generate()` calls |

## Choosing Your Layer

### Use L0 (`gemini-live`) if you need:

- Raw WebSocket access with no abstraction overhead
- Custom event loop logic that does not fit the callback model
- A custom transport (e.g., Unix domain socket, QUIC)
- To build your own agent runtime
- Maximum control over every message sent and received

```rust,ignore
use gemini_live::prelude::*;

let config = SessionConfig::from_endpoint(ApiEndpoint::google_ai("YOUR_KEY"))
    .model(GeminiModel::Gemini2_0FlashLive);

let handle = ConnectBuilder::new(config).build().await?;
let mut events = handle.subscribe();

handle.send_text("Hello").await?;
while let Some(event) = recv_event(&mut events).await {
    match event {
        SessionEvent::TextDelta(text) => print!("{text}"),
        SessionEvent::TurnComplete => break,
        _ => {}
    }
}
```

### Use L1 (`gemini-adk`) if you need:

- Automatic tool dispatch without manual message matching
- State management with prefix scoping (`session:`, `turn:`, `app:`)
- Phase machine for multi-step conversation flows
- Turn extraction (LLM-based or custom) between turns
- Telemetry and session signals
- Full control over callback registration without the fluent syntax

### Use L2 (`gemini-adk-fluent`) if you want:

- The fastest path from zero to working voice agent
- Chainable builder API with sub-builders for phases and watchers
- Operator algebra for composing tools (`T::simple() | T::google_search()`)
- One-line connection (`connect_vertex(project, location, token)`)
- Sensible defaults (auto-enables transcription when extractors are used)

```rust,ignore
use gemini_adk_fluent::prelude::*;

let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .voice(Voice::Kore)
    .instruction("You are a helpful assistant")
    .on_audio(|data| { /* play audio */ })
    .on_text(|t| print!("{t}"))
    .connect_google_ai("YOUR_KEY")
    .await?;

handle.send_text("Hello!").await?;
handle.done().await?;
```

Most developers should start at L2 and drop to L1/L0 only when they hit a
specific limitation. The layers are designed to compose: you can use L0
types (like `SessionConfig`) with L2 builders via `Live::connect(config)`.
