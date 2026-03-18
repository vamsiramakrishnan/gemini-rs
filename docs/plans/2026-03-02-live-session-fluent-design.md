# Live Session Fluent API — Design Document

**Date**: 2026-03-02
**Branch**: `feat/workspace-adk`

## Problem

Every demo repeats ~100 lines of boilerplate: SessionConfig setup, event loop matching, tool dispatch + response, error handling. The Live API's full-duplex WebSocket session has no fluent surface — developers work with raw L0 primitives.

## Constraints (Gemini Live API)

- **Tools fixed at setup** — cannot add/remove tools mid-session
- **System instruction updatable** — via `client_content` with `role: system`
- **Context server-managed** — can trigger compaction via `context_window_compression`
- **Session ~10 minutes** — GoAway 60s before termination, 24h resume window
- **Audio format**: Input PCM16 16kHz, Output PCM16 24kHz
- **Video format**: JPEG 768x768, 1 FPS via `realtime_input`
- **Full-duplex**: Audio/video streaming concurrent with model responses

## Architecture: Two-Lane Event Processing

Audio arrives at ~40-100 events/sec. Tool dispatch can take 1-30 seconds. Sharing one processing loop causes audio stutter during tool execution.

Solution: split the event stream into two priority lanes.

### Fast Lane (sync callbacks, never blocks)
- AudioData, TextDelta, TextComplete
- InputTranscription, OutputTranscription
- VoiceActivityStart, VoiceActivityEnd
- PhaseChanged, SessionResumeHandle

Callbacks are `Fn` (sync), must complete in < 1ms.

### Control Lane (async callbacks, can block)
- ToolCall (dispatch + await result + send response)
- ToolCallCancelled
- Interrupted (flush playback)
- TurnComplete
- GoAway (save state, prepare reconnect)
- Connected, Disconnected, Resumed
- Error
- AgentTransfer

Callbacks are async, awaited on a dedicated task.

### Interruption Ordering

When `Interrupted` arrives:
1. Fast lane sets `interrupted` flag → stops forwarding AudioData
2. Control lane awaits `on_interrupted()` callback
3. Fast lane resumes after control lane processes

### Shared State Between Lanes

```rust
struct SharedLiveState {
    interrupted: AtomicBool,
    active_agent: ArcSwap<String>,
    resume_handle: Mutex<Option<String>>,
}
```

## Crate Distribution

### L0 (gemini-live) — Wire primitives

Add to `SessionHandle` / `SessionWriter`:
- `send_video(jpeg_bytes)` — `realtime_input` with `image/jpeg`
- `update_instruction(text)` — `client_content` with `role: system`
- `compact()` — context compression trigger

### L1 (gemini-adk) — Runtime

New types:
- `EventCallbacks` — typed callback registry (sync + async)
- `LiveSessionBuilder` — combines SessionConfig + Agent + Callbacks + ToolDispatcher
- `LiveHandle` — runtime interaction (send_audio/text/video, subscribe, done)
- `FastLaneConsumer` — task for sync event callbacks
- `ControlLaneProcessor` — task for async event callbacks + tool dispatch

### L2 (gemini-adk-fluent) — Fluent sugar

New type:
- `Live::builder()` — fluent wrapper over `LiveSessionBuilder`
- Methods: `.on_audio()`, `.on_text()`, `.on_interrupted()`, etc.
- Composes with existing M/T/P modules

## Callback Type Signatures

### Fast Lane (Sync)

```rust
type AudioCb      = Box<dyn Fn(&Bytes) + Send + Sync>;
type TextCb       = Box<dyn Fn(&str) + Send + Sync>;
type TranscriptCb = Box<dyn Fn(&str, bool) + Send + Sync>;
type VadCb        = Box<dyn Fn() + Send + Sync>;
type PhaseCb      = Box<dyn Fn(SessionPhase) + Send + Sync>;
```

### Control Lane (Async)

```rust
type InterruptedCb  = Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;
type ToolCallCb     = Box<dyn Fn(Vec<FunctionCall>) -> BoxFuture<'static, Option<Vec<FunctionResponse>>> + Send + Sync>;
type TurnCompleteCb = Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;
type GoAwayCb       = Box<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;
type ConnectedCb    = Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;
type DisconnectedCb = Box<dyn Fn(Option<String>) -> BoxFuture<'static, ()> + Send + Sync>;
type ErrorCb        = Box<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>;
type TransferCb     = Box<dyn Fn(String, String) -> BoxFuture<'static, ()> + Send + Sync>;
```

## Tool Dispatch Integration

- If `ToolDispatcher` registered + `on_tool_call` returns `None` → auto-dispatch, auto-respond
- If `on_tool_call` returns `Some(responses)` → use custom responses
- If neither → log warning, skip

## Multi-Agent In-Session Transfer

Pre-merge all tools at setup. On transfer:
1. `session.update_instruction(target.instruction)`
2. Switch active dispatcher to target's dispatcher
3. Emit `AgentTransfer` event
4. Continue event loop (same WebSocket, same session)

## Middleware Integration

Middleware hooks fire at the control lane level:
1. `Middleware.on_event()` — observe
2. `Plugin.on_event()` — can deny/short-circuit
3. User callback
4. `Middleware.after_tool()` / `Plugin.after_tool()` — for tool calls

Fast lane is middleware-free for performance.

## Target API

```rust
let session = Live::builder()
    .model(Model::GeminiLive2_5FlashNativeAudio)
    .voice(Voice::Kore)
    .instruction("You are a weather assistant")
    .tools(dispatcher)
    .transcription(true, true)
    .vad(Vad::default().silence_duration_ms(2000))
    .session_resume(true)
    .context_compression(4000, 2000)
    .middleware(M::log() | M::latency())
    .on_audio(|data| playback_tx.send(data.clone()).ok())
    .on_text(|t| print!("{t}"))
    .on_interrupted(|| async { playback.flush().await; })
    .on_turn_complete(|| async { save_state().await; })
    .connect(Auth::vertex("project", "us-central1"))
    .await?;

session.send_text("What's the weather?").await?;
session.done().await?;
```
