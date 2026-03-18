# Callback Execution Modes — Blocking vs Concurrent in Full-Duplex Live Sessions

**Date**: 2026-03-02
**Status**: Design RFC
**Scope**: `gemini-adk` (L1) callback execution model, `gemini-adk-fluent` (L2) ergonomic surface
**No changes to**: `gemini-live` (L0) — already maximally flexible via raw broadcast channel

---

## Executive Summary

The current two-lane event processor hard-codes execution semantics per event type:
fast-lane events are always synchronous/non-blocking, control-lane events are always
async/blocking. This is the wrong abstraction boundary. The execution mode should be
a property of the **callback instance**, not the **event type**.

A restaurant order bot's `on_turn_complete` that runs an LLM extraction needs blocking.
The same event used for DataDog telemetry needs concurrent. Today you cannot express this;
both callbacks share the same forced-await path.

This document proposes `CallbackMode::Blocking` / `CallbackMode::Concurrent` as a
per-registration choice, analyzes the real-time audio implications, and defines the
invariants that must hold in a full-duplex speech-to-speech pipeline.

It also covers **non-blocking tool calls** — a Gemini API-level feature where the model
continues generating speech while tools execute in the background. This is a separate
axis from `CallbackMode` and introduces `ToolExecutionMode::Background`, a
`ResultFormatter` trait for customizing acknowledgment/result presentation, and per-tool
execution mode configuration. The two axes (callback mode and tool execution mode)
compose orthogonally to produce the optimal voice UX: zero dead air during tool
execution with natural filler speech from the model.

---

## 1. The Full-Duplex Topology

Understanding where blocking actually hurts requires tracing every data path:

```
 ┌────────────────────┐                    ┌───────────────────┐
 │   User Application │                    │   Gemini Server   │
 │                    │                    │                   │
 │  send_audio() ─────┼──→ command_tx ──→  │  ← user audio     │
 │  send_text()  ─────┼──→ command_tx ──→  │  ← user text      │
 │                    │                    │                   │
 │                    │    gemini-live        │                   │
 │                    │    connection      │                   │
 │                    │    loop            │                   │
 │                    │   (tokio::select!) │                   │
 │                    │                    │                   │
 │                    │  ← event_tx ←──── │  → model audio     │
 │                    │  broadcast         │  → model text      │
 │                    │  (bounded buf)     │  → tool calls      │
 │                    │       │            │  → turn complete   │
 │                    │       ▼            │                   │
 │              ┌─────┼──────────────┐     │                   │
 │              │  gemini-adk processor  │     │                   │
 │              │  ┌──────────────┐  │     │                   │
 │              │  │  Router task │  │     │                   │
 │              │  └──┬───────┬──┘  │     │                   │
 │              │     │       │     │     │                   │
 │              │  ┌──▼──┐ ┌──▼──┐  │     │                   │
 │  callbacks ←─┼──│Fast │ │Ctrl │──┼─→ send_tool_response()  │
 │              │  │Lane │ │Lane │  │     │                   │
 │              │  └─────┘ └─────┘  │     │                   │
 │              └────────────────────┘     │                   │
 └────────────────────┘                    └───────────────────┘
```

### Critical Insight: Input and Output Are Independent Paths

The user's microphone → `send_audio()` → `command_tx` → WebSocket is a **completely
separate channel** from the server's response → `event_tx` → processor → callbacks.
A blocking callback does NOT stop the user's voice from reaching Gemini. The model
continues hearing the user speak.

**What blocking affects**: processing of subsequent **server** events. Model audio,
text deltas, tool calls, and turn completions queue in the broadcast buffer. If the
buffer overflows, events are **permanently dropped** (`RecvError::Lagged`).

### Timing Budget

- Audio frame interval: **~40ms** (PCM16 24kHz, typical chunk size)
- Broadcast buffer: configurable, default 256 events
- Buffer runway at audio rate: **~10 seconds** before overflow at 25 events/sec
- Control events (TurnComplete, ToolCall): **~every 2-30 seconds**
- LLM extraction call: **1-5 seconds**
- LLM text agent call: **2-10 seconds**
- Human-in-the-loop approval: **5-60 seconds**
- Network hiccup (WebSocket relay): **50-500ms**

---

## 2. The Proposal: Per-Callback Execution Mode

### 2.1 The Enum

```rust
/// How the event processor executes a callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallbackMode {
    /// Await the callback to completion before processing the next event.
    ///
    /// Guarantees:
    /// - FIFO ordering: event N+1's handler runs AFTER event N's handler finishes
    /// - State consistency: any state written by the handler is visible to subsequent handlers
    /// - Completion: errors from the handler can be observed and acted on
    ///
    /// Risks:
    /// - Pipeline stall: slow handlers block ALL subsequent events in the same lane
    /// - Buffer overflow: if stall exceeds broadcast buffer runway, events are dropped
    /// - Audio underrun: if used for fast-lane events, audio playback stutters
    #[default]
    Blocking,

    /// Spawn the callback as an independent tokio task. The processor continues
    /// immediately without waiting.
    ///
    /// Guarantees:
    /// - Zero pipeline latency: next event processes in <1μs regardless of handler duration
    /// - Isolation: handler failure does not affect the pipeline
    ///
    /// Risks:
    /// - No ordering: handlers for event N and N+1 may complete in any order
    /// - State races: concurrent handlers writing to State have last-write-wins semantics
    /// - Unbounded concurrency: if handlers are slower than event rate, tasks accumulate
    /// - Silent failure: handler panics are caught by tokio, logged, but not propagated
    Concurrent,
}
```

### 2.2 Uniform Callback Representation

All callbacks become async with an associated mode. The fast/control lane distinction
becomes an **optimization** (sync fast-lane callbacks bypass the async runtime), not a
semantic contract.

```rust
pub struct EventCallbacks {
    // Every callback: (handler, mode)
    pub on_audio:             Option<(AsyncCallback<Bytes>,          CallbackMode)>,
    pub on_text:              Option<(AsyncCallback<String>,         CallbackMode)>,
    pub on_text_complete:     Option<(AsyncCallback<String>,         CallbackMode)>,
    pub on_input_transcript:  Option<(AsyncCallback<(String, bool)>, CallbackMode)>,
    pub on_output_transcript: Option<(AsyncCallback<(String, bool)>, CallbackMode)>,
    pub on_vad_start:         Option<(AsyncCallback<()>,             CallbackMode)>,
    pub on_vad_end:           Option<(AsyncCallback<()>,             CallbackMode)>,
    pub on_phase:             Option<(AsyncCallback<SessionPhase>,   CallbackMode)>,
    pub on_interrupted:       Option<(AsyncCallback<()>,             CallbackMode)>,
    pub on_tool_call:         Option<(ToolCallCallback,              CallbackMode)>,  // always forced Blocking
    pub on_tool_cancelled:    Option<(AsyncCallback<Vec<String>>,    CallbackMode)>,
    pub on_turn_complete:     Option<(AsyncCallback<()>,             CallbackMode)>,
    pub on_go_away:           Option<(AsyncCallback<Duration>,       CallbackMode)>,
    pub on_connected:         Option<(AsyncCallback<()>,             CallbackMode)>,
    pub on_disconnected:      Option<(AsyncCallback<Option<String>>, CallbackMode)>,
    pub on_error:             Option<(AsyncCallback<String>,         CallbackMode)>,
    pub on_extracted:         Option<(AsyncCallback<(String, serde_json::Value)>, CallbackMode)>,
    // ... interceptors remain Blocking-only (they transform data in the pipeline)
    pub before_tool_response: Option<ToolResponseInterceptor>,
    pub on_turn_boundary:     Option<TurnBoundaryCallback>,
    pub instruction_template: Option<InstructionTemplateCallback>,
}
```

### 2.3 Forced Modes (Non-Negotiable)

Some callbacks have **physical constraints** that override user preference:

| Callback | Forced Mode | Reason |
|----------|-------------|--------|
| `on_tool_call` | **Blocking** | Return value (`Option<Vec<FunctionResponse>>`) drives tool response. Cannot be fire-and-forget. |
| `on_interrupted` | **Blocking** | Must complete before `interrupted` atomic flag is cleared. Otherwise audio leaks through during the handler. |
| `before_tool_response` | **Blocking** | Return value (transformed responses) is sent to Gemini. Data-flow dependency. |
| `on_turn_boundary` | **Blocking** | Injects content via `SessionWriter` before `on_turn_complete`. Ordering is semantic. |
| `instruction_template` | **Blocking** (sync) | Return value drives `update_instruction`. Pure function, already <1ms. |

If the user attempts to register these with `Concurrent`, the builder should either
warn at registration time or silently upgrade to `Blocking`. The preferred approach
is a compile-time restriction: these callbacks only expose `_blocking()` registration
in the fluent API, with no `_concurrent()` variant.

---

## 3. Processor Architecture

### 3.1 Current: Hard-Coded Two Lanes

```
broadcast ──→ Router ──→ fast_tx (unbounded mpsc) ──→ Fast Consumer (sync, inline)
                    └──→ ctrl_tx (bounded mpsc/64) ──→ Control Consumer (async, await)
```

### 3.2 Proposed: Mode-Aware Unified Dispatch

The router remains. Each lane checks the callback's mode before executing:

```
broadcast ──→ Router ──→ fast_tx ──→ Fast Consumer
                    │                  │
                    │                  ├─ mode == Concurrent → call inline (sync) or spawn
                    │                  └─ mode == Blocking   → await (must be on async task)
                    │
                    └──→ ctrl_tx ──→ Control Consumer
                                      │
                                      ├─ mode == Concurrent → tokio::spawn(cb(data))
                                      └─ mode == Blocking   → cb(data).await
```

#### Fast Lane Mode Handling

For fast-lane events registered as `Concurrent` (the default and common case), the
existing sync path is preserved as-is — no async overhead. This is the zero-cost path.

For fast-lane events registered as `Blocking`, the callback must be async. The fast
lane calls `.await` on it, which means the fast lane task must be an async context
(it already is — it's a `tokio::spawn`'d future). While the blocking callback runs,
all other fast-lane events queue in the unbounded mpsc channel. This is the explicit
tradeoff the user opts into.

**Timeout guard for blocking fast-lane callbacks:**

```rust
if mode == CallbackMode::Blocking {
    match tokio::time::timeout(FAST_LANE_BUDGET, cb(data)).await {
        Ok(()) => {}
        Err(_) => {
            tracing::warn!(
                event = %event_name,
                budget_ms = FAST_LANE_BUDGET.as_millis(),
                "Blocking fast-lane callback exceeded budget — pipeline stalled"
            );
            // Do NOT cancel the future — let it finish. Just log the overrun.
            // Cancellation mid-callback is unsafe (partial state writes).
        }
    }
}
```

Default budget: 50ms (slightly over one audio frame). This is a warning, not a hard
kill — cancelling a future mid-execution can leave state inconsistent.

#### Control Lane Mode Handling

For control-lane events registered as `Blocking` (the default), behavior is identical
to today: `cb(data).await`.

For control-lane events registered as `Concurrent`:

```rust
if mode == CallbackMode::Concurrent {
    let cb = cb.clone();
    tokio::spawn(async move {
        if let Err(e) = std::panic::AssertUnwindSafe(cb(data))
            .catch_unwind()
            .await
        {
            tracing::error!("Concurrent callback panicked: {:?}", e);
        }
    });
}
```

The spawned task is fully independent. It inherits no ordering relationship with
subsequent events. Its result is not observed by the pipeline.

### 3.3 Concurrent Task Pressure Relief

Unbounded `tokio::spawn` under sustained event load is a memory leak. For concurrent
callbacks on high-frequency events (audio, text), apply a semaphore:

```rust
/// Maximum concurrent spawned tasks per callback slot.
/// When full, oldest task is dropped (newest wins).
const MAX_CONCURRENT_TASKS: usize = 64;
```

For audio at 25/sec with a 200ms handler, steady state is ~5 concurrent tasks.
64 provides generous headroom. If the semaphore is full, the processor logs a
warning and drops the event (not the task). This is the correct behavior: if you
can't keep up, dropping the newest event is better than accumulating unbounded
memory.

Alternative: use a `tokio::sync::Semaphore` and `.try_acquire()`. On failure,
skip the spawn and log. This provides explicit backpressure visibility.

---

## 4. Real-World Expected Behavior

### 4.1 Restaurant Order Bot — Extraction + Background Agent

**Setup:**
```rust
Live::builder()
    .on_audio(|data| play_speaker(data))                       // Concurrent (default)
    .on_turn_complete(|| async { log_turn_complete().await })   // Blocking? No — just telemetry
    .on_turn_complete_concurrent(|| async {                     // Concurrent — fire-and-forget
        metrics::emit("turn_complete").await;
    })
    .extract_turns::<OrderState>(llm, "Extract order items")   // Blocking (extractor pipeline)
    .on_extracted_concurrent(|name, value| async move {        // Concurrent — background agent
        let ticket = kitchen_agent.format_ticket(&value).await;  // 2s LLM call
        kitchen_display.update(ticket).await;
    })
    .instruction_template(|state| {                            // Blocking (sync, <1ms)
        match state.get::<String>("OrderState.phase")? {
            "greeting" => Some("Welcome the customer warmly.".into()),
            "ordering" => Some("Take orders. Suggest popular items.".into()),
            "confirming" => Some("Read back the order. Ask for confirmation.".into()),
            _ => None,
        }
    })
```

**Timeline — User Orders Pizza and Garlic Bread:**

```
t=0.0s  User: "I'll have a large pepperoni pizza"
        → Audio flows to Gemini via send_audio (unaffected by callbacks)
        → on_audio fires (Concurrent) — speaker plays model silence/filler

t=2.0s  Model: "Great choice! Anything else?"
        → on_audio fires 50x (Concurrent) — speaker plays response audio
        → on_text fires 8x (Concurrent) — UI updates text bubble
        → TurnComplete fires

t=2.0s  CONTROL LANE: TurnComplete processing begins
        │ Step 1: TranscriptBuffer.end_turn() — <1ms
        │ Step 2: Extractor LLM call — 1.5s (BLOCKING, correct)
        │         During this 1.5s:
        │         - User says "and garlic bread" (audio flows to Gemini via command_tx)
        │         - Gemini processes it, starts responding
        │         - Server events (AudioData, TextDelta) enter broadcast buffer
        │         - Fast lane STILL PROCESSES audio — speaker plays model's response
        │           (fast lane is a separate task, not blocked by control lane)
        │         - Control lane events (next TurnComplete) queue in ctrl_tx channel
        │
        │ Step 3: Extractor returns {items: ["lg pepperoni pizza"], phase: "ordering"}
        │ Step 4: on_extracted fires (CONCURRENT)
        │         → tokio::spawn(kitchen_agent.format_ticket()) — runs in background
        │         → control lane continues IMMEDIATELY
        │ Step 5: instruction_template called — returns "Take orders..." — <1ms
        │ Step 6: on_turn_complete (Concurrent) — metrics fire-and-forget
        │
t=3.5s  CONTROL LANE: ready for next event
        → Dequeues next TurnComplete (from user's "garlic bread" exchange)
        → Extractor runs again, now has 2-turn window
        → Returns {items: ["lg pepperoni pizza", "garlic bread"], phase: "ordering"}

t=4.0s  BACKGROUND: kitchen_agent from t=2.0s finishes
        → Kitchen display updates with first ticket
        → Immediately overwritten when second extraction's kitchen_agent finishes at t=5.5s
```

**Key Observation**: The 1.5s extractor blocking is invisible to the user because:
1. User's audio continues flowing to Gemini (separate path)
2. Model's audio continues playing via the fast lane (separate task)
3. Only control-lane events queue — and they're infrequent (~1 per turn)

The background kitchen agent (2s LLM call) runs concurrently. Without `Concurrent`
mode, it would add 2 seconds of dead time where no control events process. With
`Concurrent`, the pipeline moves on instantly.

**Eventual Consistency**: The kitchen display might briefly show the first order
before updating to include garlic bread. For a kitchen display, this is perfectly
acceptable — it's a "live feed" that refines as the conversation progresses.

---

### 4.2 Live Audio Relay — Translation Booth

**Setup**: Model translates speech in real-time. Audio is transcoded to Opus and
pushed to a WebSocket relay serving 50 remote listeners.

```rust
Live::builder()
    .on_audio(|pcm_data| {                                     // Concurrent (default)
        let opus = transcode_pcm_to_opus(pcm_data);           // ~5ms CPU
        relay_tx.try_send(opus).ok();                          // non-blocking channel send
    })
```

**Why Concurrent is correct here:**

Audio arrives at 25 chunks/sec. The transcode takes ~5ms. If the relay WebSocket
backs up, `try_send` drops the frame rather than blocking. Listeners experience
a brief audio gap (masked by Opus's PLC — packet loss concealment) rather than
the entire pipeline stalling.

**The Blocking alternative and why it fails:**

```rust
// ANTI-PATTERN for audio relay
.on_audio_blocking(|pcm_data| async move {
    let opus = transcode_pcm_to_opus(pcm_data);
    relay_ws.send(opus).await;  // blocks on WebSocket write
})
```

If the relay WebSocket backs up for 200ms (common under network congestion):
- Audio chunk at t=0ms blocks for 200ms
- Chunks at t=40ms, 80ms, 120ms, 160ms queue behind it
- At t=200ms, handler returns — but 5 chunks are backlogged
- Each chunk handler takes 5ms + network time
- The backlog grows monotonically — it **never recovers**
- After ~10 seconds, the broadcast buffer overflows
- Events are permanently dropped — not just audio, but text, VAD, everything

**When Blocking audio IS correct:**

A recording pipeline where ordering and completeness matter more than latency:

```rust
// CORRECT: lossless recording to file, blocking ensures ordering
.on_audio_blocking(|pcm_data| async move {
    file_writer.write_all(&pcm_data).await;  // local I/O, <1ms
    // Blocking is fine because local file writes are fast and ordered
})
```

Local file I/O completes in <1ms — well within the 40ms frame budget. The blocking
guarantee ensures chunks are written in exact order with no interleaving. This is
the right tradeoff for archival recording.

---

### 4.3 Human-in-the-Loop Tool Approval

**Setup**: Financial agent that requires user confirmation for transactions.

```rust
Live::builder()
    .tools(dispatcher)
    .on_tool_call(|calls| async move {                         // Blocking (forced)
        for call in &calls {
            if call.name == "transfer_money" {
                let approved = show_confirmation_dialog(&call).await;  // waits for tap
                if !approved {
                    return Some(vec![FunctionResponse {
                        name: call.name.clone(),
                        response: json!({"error": "User denied the transfer"}),
                        id: call.id.clone(),
                    }]);
                }
            }
        }
        None  // fall through to auto-dispatch
    })
```

**Timeline — User says "Send Bob $5000" then changes mind:**

```
t=0s   User: "Send five thousand dollars to Bob"
       → Audio flows to Gemini

t=2s   Model: "I'll transfer $5000 to Bob"
       → Model emits ToolCall: transfer_money({amount: 5000, to: "Bob"})

t=2s   CONTROL LANE: on_tool_call fires (BLOCKING — forced)
       │ App shows: "Transfer $5000 to Bob? [Approve] [Deny]"
       │
       │ t=4s  User says: "Wait, actually cancel that"
       │       → Audio flows to Gemini via send_audio (UNBLOCKED)
       │       → Gemini processes the cancellation
       │       → Gemini may send ToolCallCancelled or Interrupted
       │       → These events queue in ctrl_tx (behind the pending approval)
       │
       │ t=8s  User taps [Deny]
       │ → on_tool_call returns error response
       │ → Tool response sent to Gemini: {"error": "User denied"}

t=8s   CONTROL LANE: processes queued events
       → ToolCallCancelled (if sent) — callback fires, dispatcher cancels
       → Model receives denial, says "No problem, transfer cancelled"
```

**Why concurrent is physically impossible**: `on_tool_call`'s return value IS the
tool response. If spawned concurrently, the processor would read `None` (no override)
and immediately auto-dispatch the transfer — executing it before the user sees the
dialog. The money moves. There is no undo.

**Full-duplex benefit**: While the dialog is showing, the user's voice still reaches
Gemini. If Gemini decides to cancel the tool call (because it heard "cancel that"),
that cancellation event queues behind the dialog. The user can also tap Deny to
guarantee rejection regardless of what Gemini decides. Both paths converge correctly.

---

### 4.4 Fluent Text Agent as Turn-Complete Handler

**Setup**: A summarizer agent condenses the conversation every turn. A separate
context-stuffer injects relevant knowledge at turn boundaries.

```rust
let summarizer = AgentBuilder::new("summarizer")
    .instruction("Condense the conversation into 3 key points. Be concise.")
    .build(llm.clone());

Live::builder()
    .on_extracted_concurrent(move |name, value| {              // Concurrent
        let agent = summarizer.clone();
        async move {
            let summary = agent.run_json(&value).await;
            state.set("conversation_summary", summary);
        }
    })
    .on_turn_boundary(|state, writer| async move {             // Blocking (forced)
        // Inject latest summary as context for next turn
        if let Some(summary) = state.get::<String>("conversation_summary") {
            writer.send_client_content(
                vec![Content::user().text(format!("[Context: {summary}]"))],
                false,
            ).await.ok();
        }
    })
```

**The interplay between Concurrent and Blocking:**

```
Turn 1 completes:
  → Extractor runs (Blocking) → produces structured data
  → on_extracted fires (Concurrent) → spawns summarizer agent (2s LLM call)
  → on_turn_boundary fires (Blocking) → reads state.conversation_summary
    → summary is None (agent hasn't finished yet) → no context injected
  → on_turn_complete fires

Turn 2 completes:
  → Meanwhile, Turn 1's summarizer finished → state.conversation_summary = "..."
  → Extractor runs → produces new data
  → on_extracted fires (Concurrent) → spawns new summarizer agent
  → on_turn_boundary fires (Blocking) → reads state.conversation_summary
    → summary is Turn 1's summary (available now!) → context injected
  → Model sees the summary for Turn 1 during Turn 3

Turn N completes:
  → on_turn_boundary always injects summary from Turn N-1
  → Summary is one turn behind — eventually consistent
```

**Advantage**: The summarizer agent takes 2-3 seconds. If it were blocking, the live
conversation would freeze for 2-3 seconds every turn — the user hears silence, the
model can't respond. With concurrent mode, the conversation flows naturally. The
summary is one turn behind, which is acceptable for context enrichment (the current
turn's transcript is already in the model's context window via Gemini's server-side
management).

**This is the key pattern**: `on_extracted` concurrent for slow background work,
`on_turn_boundary` blocking for fast state-to-context injection. The boundary
callback reads whatever the latest state is — it doesn't wait for the concurrent
agent to finish. Eventual consistency at the application layer, strong ordering at
the pipeline layer.

---

### 4.5 Session Initialization — Profile Loading

**Setup**: App loads user profile from database when session connects.

```rust
Live::builder()
    .on_connected(|| async {                                   // Blocking (recommended)
        let profile = db::load_user_profile(user_id).await;    // 100-200ms
        state.set("user_profile", profile);
        state.set("language", profile.preferred_language);
    })
    .instruction_template(|state| {
        let lang = state.get::<String>("language")?;
        Some(format!("Respond in {lang}. Be friendly."))
    })
```

**Why blocking is correct**: If `on_connected` is concurrent, the first TurnComplete
arrives before the profile is loaded. `instruction_template` reads `language` from
state — it's `None`. The model responds in English instead of the user's preferred
Spanish. The first impression is wrong.

With blocking: the 100-200ms initialization delay is invisible to the user. The
WebSocket is connected but the model hasn't started speaking yet. No audio is lost.
The profile is ready before the first exchange.

**When concurrent is acceptable**: If the profile is optional / nice-to-have (e.g.,
personalized greetings but default works fine):

```rust
.on_connected_concurrent(|| async {
    // Best-effort profile enrichment
    if let Ok(profile) = db::load_user_profile(user_id).await {
        state.set("user_profile", profile);
    }
})
```

The first turn uses a generic greeting. By the second turn, the profile is loaded.

---

### 4.6 Error Reporting and Telemetry

Every `on_error` registration in a production system is fire-and-forget:

```rust
.on_error_concurrent(|err| async move {
    sentry::capture_message(&err, sentry::Level::Warning);     // HTTP POST, 50-200ms
    metrics::counter!("session_errors").increment(1);
})
.on_disconnected_concurrent(|reason| async move {
    analytics::track("session_ended", json!({"reason": reason})).await;
})
```

**Blocking these is never correct in production.** A network timeout to Sentry
(3-5 second default) would stall the entire pipeline — killing the session — because
the error reporter itself became the error. Concurrent mode isolates failure: if
Sentry is down, the callback fails silently and the session continues.

---

## 5. Fluent API Surface

### 5.1 Naming Convention

Each callback gets two registration methods. The undecorated name uses the
**recommended default** for that event type:

```rust
impl Live {
    // Audio: default Concurrent (high frequency, must not block)
    fn on_audio(self, f: impl AsyncFn(&Bytes)) -> Self            // Concurrent
    fn on_audio_blocking(self, f: impl AsyncFn(&Bytes)) -> Self   // Blocking (opt-in)

    // Text: default Concurrent
    fn on_text(self, f: impl AsyncFn(&str)) -> Self               // Concurrent
    fn on_text_blocking(self, f: impl AsyncFn(&str)) -> Self      // Blocking (opt-in)

    // Turn complete: default Blocking (extractors depend on it)
    fn on_turn_complete(self, f: impl AsyncFn()) -> Self           // Blocking
    fn on_turn_complete_concurrent(self, f: impl AsyncFn()) -> Self // Concurrent (opt-in)

    // Connected: default Blocking (init before events)
    fn on_connected(self, f: impl AsyncFn()) -> Self               // Blocking
    fn on_connected_concurrent(self, f: impl AsyncFn()) -> Self    // Concurrent (opt-in)

    // Error: default Concurrent (never block on error reporting)
    fn on_error(self, f: impl AsyncFn(String)) -> Self             // Concurrent
    fn on_error_blocking(self, f: impl AsyncFn(String)) -> Self    // Blocking (opt-in)

    // Extracted: default Concurrent (background agents)
    fn on_extracted(self, f: impl AsyncFn(String, Value)) -> Self             // Concurrent
    fn on_extracted_blocking(self, f: impl AsyncFn(String, Value)) -> Self    // Blocking (opt-in)

    // Tool call: ONLY blocking (forced — return value is the response)
    fn on_tool_call(self, f: impl AsyncFn(Vec<FunctionCall>) -> ...) -> Self  // Blocking only

    // Interrupted: ONLY blocking (forced — must clear flag after)
    fn on_interrupted(self, f: impl AsyncFn()) -> Self             // Blocking only
}
```

### 5.2 Defaults Table

| Callback | Default Mode | Rationale |
|----------|-------------|-----------|
| `on_audio` | Concurrent | 40ms frame budget, blocking causes cascade failure |
| `on_text` | Concurrent | High frequency, UI update only |
| `on_text_complete` | Concurrent | Response done, no ordering need |
| `on_input_transcript` | Concurrent | High frequency, informational |
| `on_output_transcript` | Concurrent | High frequency, informational |
| `on_vad_start` | Concurrent | Latency-sensitive signal |
| `on_vad_end` | Concurrent | Latency-sensitive signal |
| `on_phase` | Concurrent | Informational |
| `on_tool_call` | **Blocking (forced)** | Return value drives response |
| `on_tool_cancelled` | Concurrent | Informational cleanup |
| `on_turn_complete` | Blocking | Extractors, state mutation, instruction update |
| `on_go_away` | Blocking | Must save state before disconnect |
| `on_connected` | Blocking | Init must complete before events flow |
| `on_disconnected` | Concurrent | Cleanup can happen in background |
| `on_resumed` | Blocking | Re-init after reconnection |
| `on_error` | Concurrent | Never block on error reporting |
| `on_transfer` | Blocking | Agent switch needs ordering |
| `on_extracted` | Concurrent | Background agents, eventual consistency |
| `on_interrupted` | **Blocking (forced)** | Must clear atomic flag after handler |
| `before_tool_response` | **Blocking (forced)** | Return value is the transformed response |
| `on_turn_boundary` | **Blocking (forced)** | Content injection must complete before turn_complete |
| `instruction_template` | **Blocking (sync)** | Return value drives instruction update |

### 5.3 Backward Compatibility

The current API where fast-lane callbacks are `Fn` (sync, not async) remains valid.
Sync callbacks are treated as `Concurrent` mode with zero async overhead — they
execute inline on the fast lane task, exactly as today. The async variants are
additive.

```rust
// EXISTING: sync closure — still works, Concurrent, zero overhead
.on_audio(|data: &Bytes| { speaker.write(data); })

// NEW: async closure — Concurrent (default), spawned
.on_audio(|data: &Bytes| async move { relay.send(data).await; })

// NEW: async closure — Blocking (explicit opt-in)
.on_audio_blocking(|data: &Bytes| async move { file.write_all(data).await; })
```

---

## 6. Tradeoff Summary

### 6.1 The Core Tension

| Property | Blocking | Concurrent |
|----------|----------|------------|
| Ordering guarantee | FIFO — event N+1 handler runs after N completes | None — handlers may interleave or reorder |
| State consistency | Strong — handler N's writes visible to handler N+1 | Eventual — concurrent writes are last-write-wins |
| Pipeline latency | handler_duration added to every event | Zero — pipeline proceeds in <1μs |
| Failure isolation | Handler failure can stall pipeline | Handler failure is isolated (logged, dropped) |
| Memory pressure | Bounded — one handler at a time | Unbounded without semaphore — tasks accumulate |
| Backpressure signal | Natural — slow handler = slow pipeline = visible problem | Hidden — tasks pile up silently until OOM |
| Audio safety | Dangerous — any >40ms handler causes stutter | Safe — pipeline never stalls |
| Real-time UX | Pauses visible to user (dead air, stuttering) | Smooth — conversation flows naturally |
| Correctness for data-flow deps | Required — tool responses, interceptors | Incorrect — spawned task can't return values |

### 6.2 Decision Framework

Choose **Blocking** when:
- The callback's return value drives pipeline behavior (tool responses, interceptors)
- Subsequent callbacks read state written by this callback
- Ordering is semantically required (init before events, cleanup before disconnect)
- The callback is fast (<50ms) and the event is infrequent (<1/sec)

Choose **Concurrent** when:
- The callback is pure observation (telemetry, logging, display updates)
- The callback does slow I/O (network, LLM calls, database) unrelated to pipeline flow
- The event is high-frequency (audio, text deltas)
- Eventual consistency is acceptable (background summarization, analytics)
- You'd rather drop an event than stall the conversation

### 6.3 The Speech-to-Speech Rule of Thumb

In a live duplex audio session, **dead air is the worst user experience**. A 200ms
pause in model audio output is perceptible. A 1-second pause feels like the system
crashed. A 3-second pause (LLM extraction call) is an eternity.

If your callback CAN be concurrent, it SHOULD be concurrent. Only use blocking when
correctness demands it — and keep blocking handlers fast (<100ms for control lane,
<10ms for fast lane).

The defaults in Section 5.2 encode this principle: fast-lane events default to
Concurrent (never risk audio), control-lane events default to Blocking only when
they participate in data flow (tool call, turn boundary, instruction template).
Events that are purely informational (error, disconnected, extracted) default to
Concurrent.

---

## 7. Implementation Scope

### 7.1 Callback Mode Changes

| Layer | Change | Description |
|-------|--------|-------------|
| `gemini-live` | None | Raw broadcast channel. Maximum flexibility already. |
| `gemini-adk/callbacks.rs` | Medium | Add `CallbackMode` enum. Change all callback fields to `(handler, mode)` tuples. Add forced-mode validation. |
| `gemini-adk/processor.rs` | Medium | Replace hard-coded lane behavior with mode-aware dispatch. Add timeout guard for blocking fast-lane. Add semaphore for concurrent task pressure. |
| `gemini-adk-fluent/live.rs` | Small | Add `_blocking()` / `_concurrent()` variants per callback. Undecorated name uses recommended default. |
| Tests | Small | Existing tests pass (sync callbacks = Concurrent, same as before). Add mode-switching tests. |

### 7.2 Non-Blocking Tool Call Changes

| Layer | Change | Description |
|-------|--------|-------------|
| `gemini-live` | None | Wire types (`FunctionCallingBehavior`, `ToolConfig`) already complete. |
| `gemini-adk/tool.rs` | Medium | Add `ToolExecutionMode` enum, `ResultFormatter` trait, `register_function_with_mode()`. Track background tools in `ActiveStreamingTool` map for cancellation. |
| `gemini-adk/processor.rs` | Medium | In `ControlEvent::ToolCall` handler, check tool mode. For `Background`: send ack via formatter, spawn tool, wire cancellation. For `Standard`: unchanged. |
| `gemini-adk/callbacks.rs` | Small | Add optional `before_tool_ack` interceptor for customizing acknowledgments. |
| `gemini-adk-fluent/live.rs` | Small | Add `.tool_behavior()`, `.tool_background()`, `.tool_background_with_formatter()`. |
| Tests | Medium | Test background dispatch, cancellation, formatter, mixed Standard+Background in same session. |

### Migration

Existing code continues to work without changes:
- Sync fast-lane callbacks → treated as `Concurrent` (same behavior as today)
- Async control-lane callbacks → treated as `Blocking` (same behavior as today)
- New code can opt into the opposite mode per callback

---

## 8. Non-Blocking Tool Calls — Gemini API-Level Execution

This is a **fundamentally different axis** from `CallbackMode`. `CallbackMode` controls
how **our processor** executes callbacks. Non-blocking tool execution controls how
**the Gemini server** treats the tool call — whether the model waits for the result
or continues talking.

These two axes are orthogonal and compose independently:

```
                          Gemini API Level
                    ┌──────────────┬──────────────┐
                    │   BLOCKING   │ NON_BLOCKING  │
   ┌────────────────┼──────────────┼──────────────┤
   │ CallbackMode:: │ Model waits  │ Model talks   │
   │ Blocking       │ Pipeline     │ Pipeline      │
   │ (our processor │ waits        │ waits         │
   │  awaits cb)    │              │               │
   ├────────────────┼──────────────┼──────────────┤
   │ CallbackMode:: │ Model waits  │ Model talks   │
   │ Concurrent     │ Pipeline     │ Pipeline      │
   │ (our processor │ continues    │ continues     │
   │  spawns cb)    │              │               │
   └────────────────┴──────────────┴──────────────┘
```

### 8.1 What the Gemini API Provides

The wire format already supports this via `FunctionCallingBehavior` in gemini-live:

```rust
// gemini-live/src/protocol/types.rs (already implemented)
pub enum FunctionCallingBehavior {
    /// Model waits for tool response before continuing output.
    #[default]
    Blocking,
    /// Model continues generating audio/text while tool executes in background.
    NonBlocking,
}

pub struct FunctionCallingConfig {
    pub mode: FunctionCallingMode,    // Auto | Any | None
    pub behavior: Option<FunctionCallingBehavior>,  // Blocking | NonBlocking
}
```

This is set at **session setup time** in `ToolConfig` — it applies to ALL tool calls
in that session.

### 8.2 How Non-Blocking Tool Calls Work

#### Standard (Blocking) Tool Call Flow

```
User: "What's the weather in Tokyo?"
                                                        Model
t=0s   Model decides to call get_weather({city:"Tokyo"})  │ STOPS OUTPUT
t=0s   ToolCall event arrives at client                    │ (silence)
t=0s   Client dispatches tool, calls weather API           │ (waiting...)
t=2s   Client sends FunctionResponse back                  │
t=2s   Model receives result, RESUMES                      │ RESUMES
t=2s   Model: "It's 22°C and sunny in Tokyo!"              ▼
```

The model produces **dead air** while waiting for the tool result. In a voice
conversation, this is a 2-second silence — unnatural and awkward.

#### Non-Blocking Tool Call Flow

```
User: "What's the weather in Tokyo?"
                                                        Model
t=0s   Model decides to call get_weather({city:"Tokyo"})  │ KEEPS TALKING
t=0s   ToolCall event arrives at client                    │ "Let me check
t=0s   Client IMMEDIATELY sends: {id, status:"Running"}   │  the weather
t=0s   Client dispatches tool in background                │  for you..."
t=2s   Tool finishes, client sends FunctionResponse        │
t=2s   Model receives result, WEAVES IT IN                 │
t=2s   Model: "Ah, looks like it's 22°C and sunny!"        ▼
```

The model produces **filler speech** ("Let me check...", "One moment...") while the
tool runs. No dead air. The result is injected into the conversation when ready, and
the model naturally incorporates it. This is how a human assistant would behave — they
keep talking while looking something up.

### 8.3 The Client-Side Protocol for Non-Blocking

When the Gemini server sends a `ToolCall` event in non-blocking mode, the client must:

1. **Immediately** send an acknowledgment response:
   ```json
   {
     "tool_response": {
       "function_responses": [{
         "name": "get_weather",
         "id": "call-abc-123",
         "response": { "status": "Running..." }
       }]
     }
   }
   ```
   This unblocks the model — it sees `"Running..."` and continues generating output.

2. **Execute the tool** in the background (may take seconds).

3. **When the tool finishes**, send the real result as a `FunctionResponse` via
   `send_tool_response()`. The model receives this mid-conversation and weaves the
   result into its ongoing response.

4. **If the model moves on** before the result arrives (e.g., user changed topic),
   the server may send a `ToolCallCancelled` event. The client should cancel the
   background tool and discard its result.

### 8.4 A ResultFormatter Trait

The acknowledgment response (`"Running..."`) and the final result injection need
formatting. Different tools need different acknowledgment messages and result
presentations. A `ResultFormatter` trait makes this pluggable:

```rust
/// Formats tool results for injection into the conversation.
///
/// For non-blocking tools, this controls both the immediate acknowledgment
/// (what the model sees while the tool runs) and the final result format
/// (how the result is presented when it arrives).
pub trait ResultFormatter: Send + Sync + 'static {
    /// Format the immediate acknowledgment sent when a non-blocking tool starts.
    ///
    /// Default: `{"status": "Running..."}`
    fn format_running(&self, call: &FunctionCall) -> serde_json::Value {
        serde_json::json!({
            "id": call.id,
            "status": "Running..."
        })
    }

    /// Format the final tool result for injection into the conversation.
    ///
    /// Called when the background tool completes. The formatted value is sent
    /// as a `FunctionResponse` to Gemini.
    ///
    /// Default: pass through the raw result.
    fn format_result(
        &self,
        call: &FunctionCall,
        result: Result<serde_json::Value, ToolError>,
    ) -> serde_json::Value {
        match result {
            Ok(value) => value,
            Err(e) => serde_json::json!({"error": e.to_string()}),
        }
    }

    /// Format a cancellation acknowledgment.
    ///
    /// Called when the server cancels a running non-blocking tool.
    /// Default: `{"status": "Cancelled"}`
    fn format_cancelled(&self, call_id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": call_id,
            "status": "Cancelled"
        })
    }
}

/// Default formatter — minimal JSON status messages.
pub struct DefaultResultFormatter;
impl ResultFormatter for DefaultResultFormatter {}
```

**Why this matters in practice**: A weather tool's acknowledgment might be
`{"status": "Checking weather..."}`, while a database query's might be
`{"status": "Querying records..."}`. The model uses these status messages to
generate appropriate filler speech. A descriptive status produces better filler
than a generic "Running...".

### 8.5 Per-Tool Behavior Configuration

Today, `FunctionCallingBehavior` is set at the **session level** — all tools in a
session are either blocking or non-blocking. But real applications mix both:

- `get_weather` → Non-blocking (2-3s API call, filler speech is natural)
- `transfer_money` → Blocking (must confirm before proceeding, no filler)
- `search_knowledge_base` → Non-blocking (1-5s, model can say "let me look that up")
- `set_reminder` → Blocking (model should confirm "reminder set" immediately after)

This requires **per-tool behavior** at the gemini-adk layer, even if Gemini only supports
session-level configuration today:

```rust
/// Execution behavior for a tool in the dispatch pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolExecutionMode {
    /// Standard: execute the tool, block the control lane until done, send response.
    /// The model waits for the result (if Gemini behavior is BLOCKING) or the
    /// client holds the response until ready.
    #[default]
    Standard,

    /// Non-blocking: immediately send a "Running..." acknowledgment, execute
    /// the tool in a background task, send the real result when done.
    /// The model continues generating output during execution.
    ///
    /// Requires `FunctionCallingBehavior::NonBlocking` at the session level.
    /// If the session is in Blocking mode, this degrades gracefully to Standard.
    Background {
        /// Optional custom formatter for this tool's responses.
        formatter: Option<Arc<dyn ResultFormatter>>,
    },
}
```

### 8.6 How It Interacts with the Control Lane

#### Standard Tool (current behavior, unchanged)

```
ControlEvent::ToolCall([get_weather({city:"Tokyo"})])
  │
  ├→ on_tool_call callback (Blocking, forced)
  │   → returns None (no override)
  │
  ├→ ToolDispatcher::call_function("get_weather", args).await  ← BLOCKS 2s
  │
  ├→ before_tool_response interceptor
  │
  └→ writer.send_tool_response(results)

  Control lane blocked for 2 seconds. No other control events process.
```

#### Non-Blocking Tool (new behavior)

```
ControlEvent::ToolCall([get_weather({city:"Tokyo"})])
  │
  ├→ on_tool_call callback (Blocking, forced)
  │   → returns None (no override)
  │
  ├→ Dispatcher sees: get_weather is ToolExecutionMode::Background
  │
  ├→ IMMEDIATELY sends acknowledgment:
  │   writer.send_tool_response([{name: "get_weather", id: "call-123",
  │     response: formatter.format_running(&call)}])
  │
  ├→ tokio::spawn(async {
  │     let result = dispatcher.call_function("get_weather", args).await;
  │     let formatted = formatter.format_result(&call, result);
  │     let response = before_tool_response_interceptor(formatted, state);
  │     writer.send_tool_response([response]);
  │   })
  │
  └→ Control lane IMMEDIATELY proceeds to next event

  Control lane blocked for <1ms. Tool runs in background.
  Model continues talking. Result injected when ready.
```

#### Non-Blocking Tool with Cancellation

```
t=0s   ToolCall: get_weather({city:"Tokyo"})
       → Ack sent, tool spawned in background
       → Control lane moves on

t=1s   User: "Actually forget the weather, tell me a joke"
       → Audio flows to Gemini
       → Gemini decides to cancel the tool

t=1.2s ToolCallCancelled(["call-123"])
       → Control lane processes cancellation
       → dispatcher.cancel_by_ids(["call-123"])  ← cancels background task
       → on_tool_cancelled callback fires
       → Background task receives cancellation, stops weather API call

t=1.5s Model: "Sure! Why did the programmer quit his job? ..."
       → No stale weather result injected — it was cancelled
```

### 8.7 Real-World Scenario: Travel Agent with Mixed Tool Modes

**Setup**: A voice travel agent that searches flights (slow, non-blocking), books
tickets (fast but needs confirmation, blocking), and checks loyalty points (instant,
standard).

```rust
let mut dispatcher = ToolDispatcher::new();

// Search flights — 3-8 seconds, model should keep talking
dispatcher.register_function_with_mode(
    Arc::new(search_flights_tool),
    ToolExecutionMode::Background {
        formatter: Some(Arc::new(FlightSearchFormatter)),
    },
);

// Book ticket — fast but needs human confirmation via on_tool_call
dispatcher.register_function(Arc::new(book_ticket_tool));  // Standard (default)

// Check loyalty points — instant (<100ms), standard is fine
dispatcher.register_function(Arc::new(check_points_tool));  // Standard (default)

Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .voice(Voice::Kore)
    .instruction("You are a travel agent. Search for flights, book tickets, check loyalty.")
    .tools(dispatcher)
    .tool_behavior(FunctionCallingBehavior::NonBlocking)  // session-level

    .on_tool_call(|calls| async move {
        // Only intercept book_ticket for human approval
        for call in &calls {
            if call.name == "book_ticket" {
                let approved = confirm_booking(&call).await;
                if !approved {
                    return Some(vec![FunctionResponse {
                        name: call.name.clone(),
                        response: json!({"error": "Booking cancelled by user"}),
                        id: call.id.clone(),
                    }]);
                }
            }
        }
        None  // auto-dispatch everything else
    })

    .on_audio(|data| play_speaker(data))
    .on_text(|t| print!("{t}"))
    .connect_vertex("vital-octagon-19612", "us-central1", token)
    .await?;
```

**Timeline — User asks to search flights:**

```
t=0s   User: "Find me flights from SF to Tokyo next Friday"

t=2s   Model decides: ToolCall search_flights({from:"SFO", to:"NRT", date:"2026-03-06"})
       │
       │ PROCESSOR:
       │ 1. on_tool_call fires → returns None (no override for search_flights)
       │ 2. Dispatcher sees search_flights is Background mode
       │ 3. Immediately sends: {"id":"call-1", "status":"Searching flights from SFO to NRT..."}
       │ 4. Spawns background task: search_flights_tool.call(args)
       │ 5. Control lane moves on in <1ms
       │
       │ MODEL (unblocked, sees "Searching flights..."):
       │ "I'm searching for flights from San Francisco to Tokyo for next Friday.
       │  Let me find the best options for you. While I'm looking, do you have
       │  a preference for airlines or time of day?"

t=4s   User: "I prefer evening departures"
       → Audio flows to Gemini, model acknowledges

t=6s   BACKGROUND: search_flights_tool completes
       → FlightSearchFormatter.format_result() produces structured results
       → before_tool_response interceptor augments with loyalty tier
       → writer.send_tool_response([{name:"search_flights", id:"call-1",
           response: {flights: [{...}, {...}, {...}]}}])

t=6.5s MODEL (receives results mid-conversation):
       "Great news! I found 3 flights for next Friday evening:
        1. JAL 2 departing 6:15 PM — $890
        2. United 837 departing 7:30 PM — $750
        3. ANA 8 departing 8:45 PM — $820
        Would you like to book any of these?"

t=8s   User: "Book the United flight"

t=9s   Model: ToolCall book_ticket({flight:"UA837", date:"2026-03-06"})
       │
       │ PROCESSOR:
       │ 1. on_tool_call fires → intercepts book_ticket
       │ 2. Shows confirmation: "Book UA837 SFO→NRT Mar 6, $750? [Confirm] [Cancel]"
       │ 3. Control lane BLOCKS waiting for user tap (this is Standard mode via override)
       │ 4. Model is also waiting (BLOCKING at Gemini level for book_ticket)
       │    → appropriate here: no filler speech while money is being spent
       │
       │ t=12s User taps [Confirm]
       │ → on_tool_call returns None → auto-dispatch → book_ticket executes
       │ → Response sent: {"confirmation":"UA837-XYZW", "status":"booked"}

t=12s  Model: "Your United flight 837 is confirmed! Confirmation code XYZW.
        Is there anything else you need for your trip?"
```

**Key observations:**
- `search_flights` runs for **4 seconds** in the background — zero dead air, model
  fills with natural conversation, even asks a useful follow-up question
- `book_ticket` correctly blocks — no filler speech while charging a credit card
- `check_points` (if called) runs in <100ms as standard — so fast it doesn't matter
- The `on_tool_call` callback is always **Blocking** (forced) at the processor level,
  but the **tool execution itself** is Background/Standard per tool
- Cancellation works: if the user says "never mind" during the flight search, the
  server sends `ToolCallCancelled`, the background task is aborted, no stale results

### 8.8 Custom ResultFormatter Example

```rust
struct FlightSearchFormatter;

impl ResultFormatter for FlightSearchFormatter {
    fn format_running(&self, call: &FunctionCall) -> serde_json::Value {
        let from = call.args["from"].as_str().unwrap_or("origin");
        let to = call.args["to"].as_str().unwrap_or("destination");
        // Descriptive status → model generates better filler speech
        json!({
            "status": format!("Searching flights from {} to {}...", from, to),
            "estimated_time": "3-5 seconds"
        })
    }

    fn format_result(
        &self,
        _call: &FunctionCall,
        result: Result<serde_json::Value, ToolError>,
    ) -> serde_json::Value {
        match result {
            Ok(flights) => {
                // Summarize for the model — don't dump raw API response
                let count = flights["results"].as_array().map(|a| a.len()).unwrap_or(0);
                json!({
                    "status": "complete",
                    "found": count,
                    "flights": flights["results"],
                    "note": "Present the top 3 options with price and departure time."
                })
            }
            Err(e) => json!({
                "status": "failed",
                "error": e.to_string(),
                "fallback": "Apologize and suggest trying again or checking manually."
            }),
        }
    }
}
```

The `"note"` and `"fallback"` fields are **prompt engineering for the model** — they
guide how the model presents the results or handles errors in speech. This is the
power of a pluggable formatter: it's not just data transformation, it's
**conversation design**.

### 8.9 Three-Way Interaction: Gemini Mode x Tool Mode x Callback Mode

The full matrix of how these three axes compose:

```
Gemini Session Behavior:  BLOCKING or NON_BLOCKING (wire-level, setup time)
Tool Execution Mode:      Standard or Background   (per-tool, gemini-adk level)
Callback Mode:            Blocking or Concurrent    (per-callback, gemini-adk level)
```

| Gemini | Tool | on_tool_call CB | Behavior |
|--------|------|-----------------|----------|
| BLOCKING | Standard | Blocking (forced) | Current behavior. Model waits. Pipeline waits. Tool runs. Response sent. |
| NON_BLOCKING | Standard | Blocking (forced) | Unusual but valid. Pipeline dispatches synchronously, sends result. Model was already talking (filler). Result injected. |
| NON_BLOCKING | Background | Blocking (forced) | **Optimal for slow tools.** Pipeline sends ack instantly, spawns tool. Model talks. Result injected later. |
| BLOCKING | Background | Blocking (forced) | **Graceful degradation.** Session is Blocking so model waits regardless. Tool runs in background but model won't talk until response. Effectively same as Standard. |

**The interesting case is row 3** — `NON_BLOCKING` session + `Background` tool. This
is where all three axes work together:

1. Gemini sends `ToolCall` → model keeps producing audio (NON_BLOCKING)
2. `on_tool_call` fires (Blocking at callback level) → user can override/deny
3. If no override, dispatcher sees `Background` mode → sends ack, spawns tool
4. Control lane unblocks → processes next events (model's audio, text, etc.)
5. Fast lane keeps playing model's filler speech to the user
6. Background tool finishes → formatted result sent → model weaves it in
7. `on_extracted` (if extraction pipeline configured) fires Concurrent → background
   agent processes the tool result for state updates

The user hears continuous speech throughout. The tool runs silently in the background.
The model naturally transitions from filler to result presentation. **This is the
gold standard for voice UX with tool use.**

### 8.10 Implementation Status

| Component | Layer | Status |
|-----------|-------|--------|
| `FunctionCallingBehavior` enum | gemini-live (wire) | Done |
| `FunctionCallingConfig.behavior` field | gemini-live (wire) | Done |
| `ToolConfig` serialization in setup | gemini-live (wire) | Done |
| `ToolCallCancellation` message parsing | gemini-live (wire) | Done |
| `ToolExecutionMode` per-tool enum | gemini-adk (runtime) | Not implemented |
| `ResultFormatter` trait | gemini-adk (runtime) | Not implemented |
| Background tool dispatch in processor | gemini-adk (runtime) | Not implemented |
| `register_function_with_mode()` on dispatcher | gemini-adk (runtime) | Not implemented |
| `.tool_behavior()` on Live builder | gemini-adk-fluent (DX) | Not implemented |
| Per-tool `.with_mode(Background)` in fluent | gemini-adk-fluent (DX) | Not implemented |

The wire protocol is complete. The runtime and fluent layers need the dispatch
logic, formatter trait, per-tool mode configuration, and the background spawn +
cancellation wiring in the processor.

### 8.11 Fluent API Surface for Non-Blocking Tools

```rust
Live::builder()
    // Session-level: tell Gemini to allow non-blocking tool calls
    .tool_behavior(FunctionCallingBehavior::NonBlocking)

    // Per-tool: mark specific tools as background-executed
    .tool_background(search_flights_tool)
    .tool_background_with_formatter(search_flights_tool, FlightSearchFormatter)

    // Standard tools (default — no annotation needed)
    .tool(book_ticket_tool)
    .tool(check_points_tool)

    // Or via the dispatcher with explicit modes
    .tools(dispatcher)  // dispatcher has per-tool modes pre-configured

    // Callbacks work as before — on_tool_call is always Blocking
    .on_tool_call(|calls| async move { /* override/approve logic */ })
    .on_tool_cancelled(|ids| async move { /* cleanup */ })
```

---

## 9. Open Questions

1. **Should sync callbacks remain as an optimization?** Today, sync `Fn` callbacks on
   the fast lane avoid async runtime overhead entirely. If we unify everything to async,
   even a simple `|data| speaker.write(data)` pays the cost of `Box::pin` + poll. On
   hot paths (audio at 25/sec), this matters. Recommendation: keep the sync fast path
   for `Concurrent` mode, only require async for `Blocking` mode on fast-lane events.

2. **Concurrent semaphore size**: Fixed at 64? Configurable per callback? The right
   answer depends on whether the callback is audio (25/sec, need headroom) vs turn
   complete (0.1/sec, 4 is plenty). Per-callback configuration adds API surface.
   Recommendation: reasonable fixed default (64), with an advanced
   `.on_audio_concurrent_with_limit(f, 16)` escape hatch.

3. **Concurrent callback error visibility**: Spawned tasks that panic are caught by
   tokio's panic handler. Should we collect errors and surface them via `on_error`?
   This creates a feedback loop (concurrent `on_error` spawns a task that panics →
   fires `on_error` again). Recommendation: log panics with `tracing::error!`, do
   not re-enter the callback system.

4. **Per-tool vs session-level non-blocking**: Gemini's `FunctionCallingBehavior` is
   set at session level — all tools share the same mode. If we want per-tool modes
   (search = Background, book = Standard), the client must handle the split: set the
   session to `NonBlocking`, but for Standard tools, wait for the result before sending
   the response (effectively making it blocking from the model's perspective even
   though the server allows non-blocking). Alternatively, we could send a "complete"
   acknowledgment for Standard tools that includes the actual result inline, so the
   model never sees a "Running..." status for them. Need to verify Gemini's behavior
   when receiving an immediate full result in NonBlocking mode.

5. **Background tool concurrency limit**: Multiple non-blocking tool calls can run
   simultaneously (model calls `search_flights` and `check_weather` in the same turn).
   Should we limit concurrent background tools? The dispatcher already tracks
   `ActiveStreamingTool` handles — extend this to background tools for cancellation
   and concurrency management. Recommendation: default limit of 8 concurrent background
   tools, configurable via `ToolDispatcher::with_max_background(n)`.

6. **ResultFormatter and `before_tool_response` interaction**: For background tools,
   should `before_tool_response` run on the acknowledgment, the final result, or both?
   Recommendation: only on the final result. The acknowledgment is a status message,
   not a tool result. The interceptor should see the real data for augmentation.
   Expose a separate `before_tool_ack` interceptor if users need to customize the
   acknowledgment beyond what `ResultFormatter::format_running()` provides.
