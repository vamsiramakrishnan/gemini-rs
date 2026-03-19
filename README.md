# gemini-rs

> Full Rust SDK for the Gemini Multimodal Live API -- wire protocol, agent runtime, and fluent DX in three layered crates.

[![CI](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/ci.yml)
[![Docs](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/docs.yml/badge.svg)](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/docs.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/gemini-genai-rs.svg)](https://crates.io/crates/gemini-genai-rs)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

---

## Why gemini-rs?

Google's Gemini Multimodal Live API enables full-duplex, real-time voice and
text conversations with tool calling, streaming audio, and mid-session
instruction updates. Building on it raw means wrestling with WebSocket frame
parsing, binary/text codec differences between Google AI and Vertex AI,
authentication token management, voice activity detection, barge-in handling,
and turn lifecycle -- before you write a single line of agent logic.

**gemini-rs** eliminates that friction. It gives you a layered Rust SDK where
each crate adds exactly the abstraction you need:

- **Wire-level access** for custom transports, proxies, or non-standard
  deployments (`gemini-genai-rs`).
- **Agent runtime** with typed state, phase machines, tool dispatch, text agent
  combinators, and a three-lane processor architecture (`gemini-adk-rs`).
- **Fluent builder API** where a production voice agent is 20 lines of
  declarative Rust, not 200 lines of boilerplate (`gemini-adk-fluent-rs`).

Every layer is independently usable. Pick the altitude that fits your problem.

### Raw WebSocket vs. Fluent API

<table>
<tr><th>Raw WebSocket (L0 only)</th><th>Fluent API (L2)</th></tr>
<tr>
<td>

```rust
// Connect, subscribe, send, match events,
// handle tool calls, manage turns, track
// state, parse audio frames ...
let session = quick_connect(
    "KEY", "gemini-2.0-flash-live-001"
).await?;
session.send_text("Hello").await?;
let mut events = session.subscribe();
while let Ok(event) = events.recv().await {
    match event {
        SessionEvent::Audio(data) => {
            /* decode, buffer, play */
        }
        SessionEvent::TextDelta(t) => {
            print!("{t}");
        }
        SessionEvent::ToolCall(calls) => {
            // dispatch, build responses,
            // send back ...
        }
        SessionEvent::TurnComplete => break,
        _ => {}
    }
}
```

</td>
<td>

```rust
let handle = Live::builder()
    .instruction("You are a helpful assistant.")
    .greeting("Say hello to the user.")
    .on_audio(|data| speaker.send(data))
    .on_text(|t| print!("{t}"))
    .on_tool_call(|calls, state| async move {
        // auto-dispatched with .tools()
        None
    })
    .connect_google_ai("KEY")
    .await?;

handle.send_text("Hello").await?;
```

</td>
</tr>
</table>

---

## Architecture

```
+----------------------------------------------------------------------+
|  gemini-adk-fluent-rs  (L2 -- Fluent DX)                                    |
|                                                                      |
|  Live::builder()  .  AgentBuilder  .  S.C.T.P.M.A operators         |
|  PhaseBuilder  .  WatchBuilder  .  Temporal patterns                 |
+----------------------------------------------------------------------+
|  gemini-adk-rs  (L1 -- Agent Runtime)                                       |
|                                                                      |
|  LiveSessionBuilder  .  LiveHandle  .  Three-lane processor          |
|  State (prefix-scoped)  .  PhaseMachine  .  ToolDispatcher           |
|  TextAgent combinators  .  Extractors  .  Watchers  .  Telemetry    |
|  LlmAgent  .  Runner  .  SessionService  .  MCP  .  A2A            |
+----------------------------------------------------------------------+
|  gemini-genai-rs  (L0 -- Wire Protocol)                                     |
|                                                                      |
|  Transport (WebSocket + Mock)  .  Codec (JSON)  .  Auth providers    |
|  SessionHandle  .  Protocol types  .  VAD  .  Jitter buffer         |
|  Telemetry (OTel + Prometheus)  .  REST APIs (feature-gated)         |
+----------------------------------------------------------------------+
```

Each layer depends only on the one below it. Application code imports from the
highest layer it needs (`gemini_adk_fluent_rs::prelude::*` re-exports all three).

---

## Core Concepts & How They Interplay

A gemini-rs voice session is built from six core concepts that work together.
This section shows what each one does and how they connect.

```
                         +------------------+
                         |   Live::builder  |  (L2 Fluent API)
                         +--------+---------+
                                  |  configures
          +-----------+-----------+-----------+-----------+
          |           |           |           |           |
     +----v---+  +----v----+  +--v---+  +----v----+  +--v--------+
     | Phases |  |Extractors| | Tools |  |Watchers |  | Telemetry |
     +----+---+  +----+----+  +--+---+  +----+----+  +-----+-----+
          |           |          |           |              |
          +-----+-----+----+----+-----+-----+              |
                |          |          |                     |
          +-----v----------v----------v-----+        +-----v-----+
          |            State                |        | Signals & |
          |  (prefix-scoped, concurrent)    |<-------+ Counters  |
          +---------------------------------+        +-----------+
```

### 1. State -- The Shared Spine

Everything reads from and writes to `State`. It is the single source of truth
for a session -- a concurrent, typed key-value store with prefix-scoped
namespaces.

```
State
  |
  +-- app:caller_name = "Alice"          (application state)
  +-- session:turn_count = 5             (auto-tracked by SessionSignals)
  +-- session:total_token_count = 1284   (auto-tracked from UsageMetadata)
  +-- derived:risk_level = "high"        (computed variable, read-only)
  +-- turn:transcript = "I need help"    (cleared each turn)
  +-- bg:verification_status = "pending" (background agent result)
```

**Why it matters:** Phase transitions check state. Extractors write to state.
Watchers fire when state changes. Computed variables derive from state.
Telemetry auto-populates state. Everything converges here.

### 2. Phases -- Conversation Structure

Phases define the *shape* of a conversation: what the model should do, what
tools are available, and when to move on.

```
  [greeting] ---> [identify_caller] ---> [handle_request] ---> [farewell]
       |               |                       |                    |
   instruction:    instruction:            instruction:         instruction:
   "Welcome..."   "Get name..."          "Help with..."       "Say goodbye"
       |               |                       |
   tools: []       tools: [lookup]         tools: [search, calc]
       |               |                       |
   transition:     transition:             transition:
   caller_name     request_type            resolved == true
   is_some()       is_some()
```

Each phase declares:
- **Instruction**: what the model should do (static or state-driven dynamic)
- **Tools**: which tools are available in this phase
- **Transitions**: state predicates that trigger moves to the next phase
- **Guards**: predicates that must be true before entering a phase
- **Needs**: state keys still required (drives navigation context)
- **Lifecycle hooks**: `on_enter` / `on_exit` for side effects

Phases don't micromanage the model. They set guardrails -- the LLM naturally
asks follow-up questions until the transition predicate becomes true.

### 3. Extractors -- Structured Data from Conversation

Extractors run out-of-band LLM calls to pull structured data from the
conversation transcript and write it into State.

```
 Conversation transcript        OOB LLM call           State
 +-----------------------+     +---------------+     +------------------+
 | "Hi, I'm Alice from   | --> | Extract with  | --> | caller_name:     |
 |  Acme Corp, I need    |     | JSON Schema   |     |   "Alice"        |
 |  help with billing."  |     +---------------+     | caller_org:      |
 +-----------------------+                           |   "Acme Corp"    |
                                                     | request_type:    |
                                                     |   "billing"      |
                                                     +------------------+
                                                           |
                                                    triggers phase
                                                    transition!
```

**Extraction triggers** control *when* extractors fire:

| Trigger | When it fires | Use case |
|---------|--------------|----------|
| `EveryTurn` | After every TurnComplete | Default, high-frequency extraction |
| `Interval(n)` | Every N turns | Reduce LLM costs for slow-changing data |
| `AfterToolCall` | After tool dispatch completes | Extract from tool results |
| `OnPhaseChange` | When phase transitions fire | Re-extract on context shift |

### 4. Watchers & Temporal Patterns -- Reactive State

Watchers observe state changes and fire callbacks. Temporal patterns detect
conditions that persist over time or turns.

```
  State change: app:score = 0.85 --> 0.95
                    |
            +-------v--------+
            | Watcher:       |
            | crossed_above  |
            | threshold=0.9  |
            +-------+--------+
                    |
            fires callback:
            state.set("alert", true)


  Condition held for 30s:          3 consecutive turns:
  +-------------------------+     +-------------------------+
  | when_sustained:         |     | when_turns:             |
  | confused == true        |     | repeating == true       |
  | for 30 seconds          |     | for 3 turns             |
  | --> offer help          |     | --> break loop           |
  +-------------------------+     +-------------------------+
```

### 5. Tools -- Model Actions

Tools give the model the ability to take actions. gemini-rs supports typed
tools (auto-schema from Rust structs), simple tools (raw JSON), built-in
tools (Google Search, code execution), and agent-as-tool (text agent pipelines
callable by the live model).

```
  Model decides to call tool
           |
  +--------v---------+
  |  ToolDispatcher   |  Routes by function name
  +--+-----+-----+---+
     |     |     |
  +--v-+ +-v--+ +v---------+
  |get_| |calc| |verify_   |
  |wx  | |pay | |identity  |
  +----+ +----+ +----------+
  Simple  Typed   AgentTool
  Tool    Tool    (text agent
                   pipeline)

  Background tools: model continues talking
  while the tool executes asynchronously.
```

**Background tool execution** eliminates dead air in voice sessions. Mark
tools as background and the model receives a "processing" acknowledgment
immediately, continuing the conversation while the tool runs:

```rust
Live::builder()
    .tools(dispatcher)
    .tool_background("search_kb")  // runs async, no dead air
```

### 6. Telemetry -- Observability Pipeline

Telemetry flows through two complementary systems, both running on the
telemetry lane (off the hot path):

```
  SessionEvent stream
        |
  +-----v--------------+     +------------------+
  | SessionSignals      |     | SessionTelemetry |
  | (State keys)        |     | (Atomic counters)|
  +-----+---------------+     +--------+---------+
        |                              |
        v                              v
  session:turn_count          audio_chunks_out: 1482
  session:total_token_count   avg_latency_ms: 340
  session:is_speaking         interruptions: 3
  session:silence_ms          total_token_count: 5280
        |                              |
        v                              v
  Available to phases,         snapshot() --> JSON
  watchers, extractors,        for devtools UI
  transition guards
```

**SessionSignals** writes to State -- so phases, watchers, and extractors can
react to session-level metrics (e.g., transition after N turns, alert when
tokens exceed budget).

**SessionTelemetry** tracks lock-free atomic counters (~1ns per operation) for
performance metrics: audio throughput, response latency (min/avg/max via CAS),
turn duration, token usage, and interruption counts.

**UsageMetadata** from the Gemini API is automatically tracked at all layers:
- L0 emits `SessionEvent::Usage(UsageMetadata)` with full token breakdowns
- L1 records in both SessionSignals (state keys) and SessionTelemetry (atomics)
- L2 exposes `.on_usage(|metadata| ...)` callback for real-time observation

### How They Work Together

Here's the flow for a single model turn in a phased conversation:

```
  User speaks: "I'm Alice from Acme Corp"
       |
  [1]  v  Fast lane: on_audio, on_input_transcript (sync, <1ms)
       |
  [2]  v  Model responds, turn completes
       |
  [3]  v  Control lane: TranscriptBuffer records the turn
       |
  [4]  v  Extractors run (OOB LLM call)
       |    --> writes caller_name="Alice", caller_org="Acme Corp" to State
       |
  [5]  v  Watchers fire on state changes
       |    --> crossed_above, became_true, changed_to callbacks
       |
  [6]  v  Computed variables recompute
       |    --> derived:risk_level updates based on new state
       |
  [7]  v  Phase machine evaluates transitions
       |    --> caller_name.is_some() == true
       |    --> transition: identify_caller --> handle_request
       |
  [8]  v  Phase on_exit / on_enter hooks fire
       |    --> instruction updated, navigation context regenerated
       |
  [9]  v  Telemetry lane: SessionSignals + SessionTelemetry update
            --> session:turn_count++, latency recorded, tokens tracked
```

---

## Quick Start

### Google AI (API Key)

```rust
use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .instruction("You are a friendly assistant.")
        .on_text(|t| print!("{t}"))
        .on_turn_complete(|| async { println!("\n---") })
        .connect_google_ai(std::env::var("GEMINI_API_KEY")?)
        .await?;

    handle.send_text("What is the speed of light?").await?;
    tokio::signal::ctrl_c().await?;
    handle.disconnect().await?;
    Ok(())
}
```

### Vertex AI

```rust
let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .voice(Voice::Kore)
    .instruction("You are a customer support agent.")
    .on_audio(|data| playback_tx.send(data.clone()).ok())
    .on_text(|t| print!("{t}"))
    .connect_vertex("my-project", "us-central1", access_token)
    .await?;
```

### Wire Level Only (L0)

```rust
use gemini_genai_rs::prelude::*;

let session = gemini_genai_rs::quick_connect(
    "API_KEY", "gemini-2.0-flash-live-001"
).await?;
session.send_text("What is the speed of light?").await?;

let mut events = session.subscribe();
while let Ok(event) = events.recv().await {
    if let SessionEvent::TextDelta(ref text) = event {
        print!("{text}");
    }
    if let SessionEvent::TurnComplete = event { break; }
}
```

---

## Crate Overview

| Crate | Layer | Description |
|-------|-------|-------------|
| [`gemini-genai-rs`](crates/gemini-genai-rs) | L0 -- Wire | Protocol types, WebSocket transport, auth providers, VAD, jitter buffer, REST APIs (feature-gated). Full Rust equivalent of Google's `@google/genai`. |
| [`gemini-adk-rs`](crates/gemini-adk-rs) | L1 -- Runtime | Agent runtime with state management, phase machines, tool dispatch, text agent combinators, extractors, watchers, telemetry. Full Rust equivalent of Google's `@google/adk`. |
| [`gemini-adk-fluent-rs`](crates/gemini-adk-fluent-rs) | L2 -- Fluent | `Live::builder()` API, `AgentBuilder`, S.C.T.P.M.A operator algebra, composition patterns, test utilities. |

---

## Features

### Voice / Live Sessions

Build full-duplex voice sessions with callbacks for every event type. Audio,
text, transcription, interruptions, and turn lifecycle are all handled.

```rust
let handle = Live::builder()
    .model(GeminiModel::GeminiLive2_5FlashNativeAudio)
    .voice(Voice::Puck)
    .instruction("You are a weather assistant.")
    .greeting("Greet the user and ask how you can help.")
    .transcription(true, true)          // input + output transcription
    .thinking(1024)                     // enable thinking with token budget
    .include_thoughts()                 // receive thought summaries
    .affective_dialog(true)             // emotionally expressive responses
    .context_compression(4000, 2000)    // auto-compress context window
    .on_audio(|data| speaker.write(data))
    .on_thought(|text| println!("[Thought] {text}"))
    .on_input_transcript(|text, _final| println!("[User] {text}"))
    .on_output_transcript(|text, _final| println!("[Agent] {text}"))
    .on_interrupted(|| async { speaker.flush().await })
    .on_turn_complete(|| async { println!("--- turn complete ---") })
    .on_usage(|usage| {
        if let Some(total) = usage.total_token_count {
            println!("Tokens used: {total}");
        }
    })
    .connect_vertex(project, location, token)
    .await?;
```

**Available voices:** `Aoede`, `Charon`, `Fenrir`, `Kore`, `Puck` (default), or `Voice::Custom("name")`.

### Thinking (Gemini 2.5+)

The `gemini-2.5-flash-native-audio-preview-12-2025` model supports thinking
capabilities with dynamic thinking enabled by default. Control the thinking
budget and receive thought summaries in your session:

```rust
let handle = Live::builder()
    .model(GeminiModel::Custom(
        "models/gemini-2.5-flash-native-audio-preview-12-2025".into(),
    ))
    .thinking(1024)           // set thinking token budget (0 = disable)
    .include_thoughts()       // receive thought summaries via on_thought
    .on_thought(|text| println!("[Thought] {text}"))
    .on_text(|t| print!("{t}"))
    .connect_google_ai(api_key)
    .await?;
```

**How it works in the three-lane architecture:**

- `thinkingConfig` (`thinkingBudget`, `includeThoughts`) is sent in the setup
  message's `generationConfig`
- When `includeThoughts` is true, thought parts arrive as `Part::Thought` in
  `model_turn` content — emitted as `SessionEvent::Thought(String)`
- Thought events are routed to the **fast lane** and delivered via the
  `on_thought` sync callback (< 1ms, no allocations)

**Platform support:** Google AI only. On Vertex AI, `thinkingConfig` is
automatically stripped from the setup message — no code changes needed.

### Tool Calling

Declare function tools with JSON Schema parameters. The SDK auto-dispatches
tool calls when you provide a `ToolDispatcher`, or you can handle them manually
in `on_tool_call`.

```rust
let handle = Live::builder()
    .instruction("You can check the weather and do math.")
    .on_tool_call(|calls, state| async move {
        let responses: Vec<FunctionResponse> = calls.iter().map(|call| {
            let result = match call.name.as_str() {
                "get_weather" => json!({"temp": 22, "condition": "sunny"}),
                _ => json!({"error": "unknown tool"}),
            };
            FunctionResponse {
                name: call.name.clone(),
                response: result,
                id: call.id.clone(),
                scheduling: None,
            }
        }).collect();
        Some(responses)
    })
    .connect_google_ai(api_key)
    .await?;
```

Or use built-in tools directly:

```rust
Live::builder()
    .google_search()        // Google Search grounding
    .code_execution()       // Sandbox code execution
    .url_context()          // URL content retrieval
```

### State Management

A concurrent, type-safe `State` container with prefix-scoped namespaces,
atomic read-modify-write, delta tracking, and transparent derived fallbacks.

```rust
use gemini_adk_rs::State;
use gemini_adk_rs::state::StateKey;

// Typed keys eliminate typo bugs
const TURN_COUNT: StateKey<u32> = StateKey::new("session:turn_count");
const SENTIMENT: StateKey<String> = StateKey::new("derived:sentiment");

let state = State::new();

// Prefix-scoped accessors
state.app().set("flag", true);              // writes to "app:flag"
state.user().set("name", "Alice");          // writes to "user:name"
state.session().set("turn_count", 0u32);    // writes to "session:turn_count"
state.turn().set("transcript", "hello");    // writes to "turn:transcript"

// Atomic read-modify-write
state.modify("session:turn_count", 0u32, |n| n + 1);

// Transparent derived fallback: get("risk") auto-checks "derived:risk"
state.set("derived:risk", 0.85);
let risk: Option<f64> = state.get("risk");  // returns Some(0.85)

// Delta tracking for transactional state
let tracked = state.with_delta_tracking();
tracked.set("temp:scratch", 42);
tracked.commit();   // merge into main store
// or: tracked.rollback();
```

**Prefix namespaces:**

| Prefix | Purpose | Lifetime |
|--------|---------|----------|
| `session:` | Auto-tracked signals (turn count, tokens, timing) | Session |
| `derived:` | Read-only computed variables | Session |
| `turn:` | Cleared each turn | Turn |
| `app:` | Application state | Session |
| `bg:` | Background task state | Session |
| `user:` | User-scoped state | Session |
| `temp:` | Scratch space | Explicit |

### Phase System

Declarative conversation phase management with guard-based transitions,
per-phase tool filtering, instruction composition, and async lifecycle callbacks.

```rust
let handle = Live::builder()
    .phase("greeting")
        .instruction("Welcome the user warmly.")
        .prompt_on_enter(true)
        .transition_with("identify", |s| {
            s.get::<String>("caller_name").is_some()
        }, "when caller provides their name")
        .done()
    .phase("identify")
        .instruction("Confirm the caller's identity.")
        .needs(&["caller_name", "caller_org"])
        .tools(vec!["lookup_contact".into()])
        .transition_with("handle", |s| {
            s.get::<bool>("verified").unwrap_or(false)
        }, "when identity is verified")
        .done()
    .phase("handle")
        .dynamic_instruction(|s| {
            let topic: String = s.get("topic").unwrap_or_default();
            format!("Help the caller with: {topic}")
        })
        .tools(vec!["search".into(), "calc".into()])
        .transition_with("farewell", |s| {
            s.get::<bool>("resolved").unwrap_or(false)
        }, "when the request is resolved")
        .done()
    .phase("farewell")
        .instruction("Say goodbye and provide a reference number.")
        .terminal()
        .done()
    .initial_phase("greeting")
    // Phase defaults inherited by all phases
    .phase_defaults(|p| {
        p.with_state(&["caller_name", "caller_org"])
         .navigation()  // inject phase navigation context
    })
    // Recommended: set persona once, steer via context injection
    .steering_mode(SteeringMode::ContextInjection)
    .connect_vertex(project, location, token)
    .await?;
```

#### Steering Modes

Control how the SDK delivers phase instructions to the model. This is the most
impactful configuration choice for multi-phase apps:

| Mode | System Instruction | Phase Instructions | Best For |
|------|--------------------|--------------------|----------|
| `ContextInjection` | Set once at connect | Delivered as model-role context turns | Multi-phase apps with stable persona (**recommended**) |
| `InstructionUpdate` | Replaced on every transition | Baked into system instruction | Agents with radically different personas per phase |
| `Hybrid` | Replaced on transition | Modifiers as context turns | Persona shifts + per-turn steering |

```rust
// Recommended: base persona at connect, phase context injected per turn
Live::builder()
    .instruction("You are a helpful assistant.")
    .steering_mode(SteeringMode::ContextInjection)
```

#### Context Delivery Timing

Control when model-role context turns hit the wire:

| Mode | Behavior | Best For |
|------|----------|----------|
| `Immediate` (default) | Send as single batched frame during TurnComplete | Low-latency, text-only apps |
| `Deferred` | Queue until next user send (audio/text/video) | Voice apps — eliminates mid-silence frames |

```rust
// Voice app: flush context alongside user audio, not during silence
Live::builder()
    .steering_mode(SteeringMode::ContextInjection)
    .context_delivery(ContextDelivery::Deferred)
```

With `Deferred`, the `DeferredWriter` wraps the session writer and drains pending context before each `send_audio`/`send_text`/`send_video`. Context that requires a prompt (e.g. `prompt_on_enter`) is always sent immediately.

See the [Steering Modes guide](docs/user-guide/steering-modes.md) for the full
decision matrix, anti-patterns, and implementation details.

#### Phase Navigation Context

The `.navigation()` modifier injects a structured description of the current
phase graph into the model's instruction, giving it awareness of where it is,
what it still needs, and where it can go:

```
[Navigation]
Current phase: identify -- Confirm the caller's identity.
Previous: greeting (turn 2)
Still needed: caller_org
Possible next:
  -> handle: when identity is verified
```

This is auto-generated from `.needs()`, `.transition_with()` descriptions, and
phase history. The model can use this to guide the conversation naturally.

### Extraction Pipeline

Run out-of-band LLM calls to extract structured data from the conversation
transcript. Schema-guided via `schemars::JsonSchema`.

```rust
use schemars::JsonSchema;

#[derive(Deserialize, Serialize, JsonSchema)]
struct CallerInfo {
    caller_name: Option<String>,
    caller_org: Option<String>,
    request_type: Option<String>,
}

let handle = Live::builder()
    .instruction("You are a receptionist.")
    // Extract every 2 turns instead of every turn (reduces LLM costs)
    .extract_turns_triggered::<CallerInfo>(
        flash_llm,
        "Extract caller name, organization, and request type",
        5,  // transcript window size
        ExtractionTrigger::Interval(2),
    )
    .on_extracted(|name, value| async move {
        println!("Extracted {name}: {value}");
    })
    .connect_vertex(project, location, token)
    .await?;

// Read latest extraction at any time
let info: Option<CallerInfo> = handle.extracted("CallerInfo");
```

Extractors automatically enable transcription and warm up the OOB LLM
connection at session start for fast first-extraction latency.

### State Watchers & Temporal Patterns

React to state changes and time-based conditions declaratively:

```rust
Live::builder()
    // Fire when app:score crosses above 0.9
    .watch("app:score")
        .crossed_above(0.9)
        .then(|_old, _new, state| async move {
            state.set("high_score_alert", true);
        })
    // Fire when a boolean becomes true
    .watch("app:escalated")
        .became_true()
        .blocking()   // block turn processing until complete
        .then(|_old, _new, _state| async move {
            notify_supervisor().await;
        })
    // Fire when condition holds for 30 seconds continuously
    .when_sustained("user_confused",
        |s| s.get::<bool>("confused").unwrap_or(false),
        Duration::from_secs(30),
        |_state, writer| async move { /* offer help */ },
    )
    // Fire after 3 consecutive turns matching condition
    .when_turns("stuck_in_loop",
        |s| s.get::<bool>("repeating").unwrap_or(false),
        3,
        |_state, writer| async move { /* break loop */ },
    )
```

### Computed (Derived) State

Register reactive computed variables that update when their dependencies change:

```rust
Live::builder()
    .computed("risk_level", &["app:sentiment_score"], |state| {
        let score: f64 = state.get("app:sentiment_score")?;
        if score < 0.3 { Some(json!("high")) }
        else { Some(json!("low")) }
    })
    // Read transparently: state.get("risk_level") auto-checks "derived:risk_level"
```

### Text Agent Combinators

Build complex request/response LLM pipelines that can be dispatched from
Live session hooks. These use standard `generate()` calls (not WebSocket
sessions), enabling background processing during a voice conversation.

| Combinator | Purpose |
|-----------|---------|
| `LlmTextAgent` | Core agent -- generate, tool dispatch, loop |
| `FnTextAgent` | Zero-cost state transform (no LLM call) |
| `SequentialTextAgent` | Run children in order, state flows forward |
| `ParallelTextAgent` | Run children concurrently via `tokio::spawn` |
| `LoopTextAgent` | Repeat until max iterations or predicate |
| `FallbackTextAgent` | Try each child, first success wins |
| `RouteTextAgent` | State-driven deterministic branching |
| `RaceTextAgent` | Run concurrently, first to finish wins |
| `TimeoutTextAgent` | Wrap an agent with a time limit |
| `MapOverTextAgent` | Iterate an agent over a list in state |
| `TapTextAgent` | Read-only observation (no mutation) |
| `DispatchTextAgent` | Fire-and-forget background tasks |
| `JoinTextAgent` | Wait for dispatched tasks |

Register text agents as tools the live model can call. The agent shares
the session's `State`, so mutations are visible to watchers and phase
transitions:

```rust
Live::builder()
    .agent_tool("verify_identity", "Verify caller identity", verifier_agent)
    .agent_tool("calc_payment", "Calculate payment plans", calc_pipeline)
```

### S.C.T.P.M.A Composition

Six operator namespaces for composing different aspects of agent configuration:

| Namespace | Operator | Purpose | Example |
|-----------|----------|---------|---------|
| `S::` | `>>` | State transforms | `S::set("key", val) >> S::rename("a", "b")` |
| `C::` | `+` | Context engineering | `C::last_n(5) + C::system_only()` |
| `T::` | `\|` | Tool composition | `T::function(search) \| T::google_search()` |
| `P::` | `+` | Prompt composition | `P::role("assistant") + P::task("summarize")` |
| `M::` | `\|` | Middleware composition | `M::log() \| M::rate_limit(10)` |
| `A::` | `+` | Artifact schemas | `A::produces(schema) + A::consumes(schema)` |

**Prompt composition example:**

```rust
use gemini_adk_fluent_rs::prelude::*;

let prompt = P::role("a customer support agent for Acme Corp")
    + P::task("help customers with billing inquiries")
    + P::constraint("never reveal internal pricing formulas")
    + P::guidelines(vec![
        "Be empathetic and professional",
        "Confirm resolution before closing",
    ]);

let instruction = prompt.render();
```

### Callback Modes

Control-lane callbacks support two execution modes:

| Mode | Method suffix | Behavior |
|------|--------------|----------|
| **Blocking** | `.on_turn_complete()` | Awaited inline -- event loop waits |
| **Concurrent** | `.on_turn_complete_concurrent()` | Spawned as detached task -- fire and forget |

Use concurrent mode for logging, analytics, webhook dispatch, or background
agent triggering where you don't need ordering guarantees.

### REST APIs (Feature-Gated)

The L0 crate also provides feature-gated access to Gemini REST APIs beyond
the Live WebSocket connection:

```toml
[dependencies]
gemini-genai-rs = { version = "0.1", features = ["generate", "embed", "files"] }
# Or enable everything:
# gemini-genai-rs = { version = "0.1", features = ["all-apis"] }
```

| Feature | API |
|---------|-----|
| `generate` | Content generation (`generateContent`) |
| `embed` | Text embeddings |
| `files` | File upload and management |
| `models` | Model listing and info |
| `tokens` | Token counting |
| `caches` | Context caching |
| `tunings` | Fine-tuning jobs |
| `batches` | Batch prediction |
| `chats` | Multi-turn chat sessions |

---

## Three-Lane Processor Architecture

All Live session events are routed through a zero-copy dispatcher into three
independent lanes, each optimized for its latency profile:

```
  SessionEvent (broadcast from L0)
         |
    +----+----+
    |  Router  |   Zero-work dispatcher -- NO state access on hot path
    +--+--+--+-+
       |  |  |
       |  |  +------------------------------+
       |  +----------------+                 |
       |                   |                 |
  +----v---------+   +-----v----------+  +---v--------------+
  | Fast Lane    |   | Control Lane   |  | Telemetry Lane   |
  | (sync <1ms)  |   | (async)        |  | (own broadcast)  |
  +--------------+   +--------------  +  +------------------+
  | on_audio     |   | on_tool_call   |  | SessionSignals   |
  | on_text      |   | on_interrupted |  |  (State keys)    |
  | on_vad_*     |   | Phase trans.   |  | SessionTelemetry |
  | on_input_    |   | Extractors     |  |  (AtomicU64)     |
  |   transcript |   |  (concurrent)  |  | on_usage cb      |
  | on_output_   |   | Watchers       |  | Debounced 100ms  |
  |   transcript |   | Computed state |  |   flush          |
  +--------------+   | Temporal ptns  |  +------------------+
                     | TranscriptBuf  |
                     |  (owned, no    |
                     |   mutex)       |
                     +----------------+
```

**Design constraints:**
- Fast lane callbacks must be sync and complete in < 1ms (no allocations, no locks, no async)
- Control lane owns the `TranscriptBuffer` exclusively (no `Arc<Mutex<>>`)
- Telemetry lane runs on its own broadcast receiver (never blocks the router)
- Extractors run concurrently via `futures::future::join_all`

---

## Examples

The `examples/` directory contains runnable examples organized by complexity.
Each demonstrates specific SDK features at the layer you need.

### Getting Started

```bash
# 1. Configure credentials
cp .env.example .env
# Edit .env: set GEMINI_API_KEY (Google AI) or GOOGLE_CLOUD_PROJECT + GOOGLE_CLOUD_LOCATION (Vertex AI)

# 2. Run a standalone example
cargo run -p text-chat       # http://127.0.0.1:3001
cargo run -p voice-chat      # http://127.0.0.1:3002
cargo run -p tool-calling    # http://127.0.0.1:3003
cargo run -p transcription   # http://127.0.0.1:3004

# 3. Run the multi-app Web UI (all apps + devtools panel)
cargo run -p gemini-adk-web-rs         # http://127.0.0.1:3000
```

### Standalone Examples

These run independently with their own Axum server and minimal UI.

| Example | Port | Layer | What You Learn |
|---------|------|-------|----------------|
| [`text-chat`](examples/text-chat) | 3001 | L0 | Wire protocol basics — connect, send text, receive streaming deltas |
| [`voice-chat`](examples/voice-chat) | 3002 | L0 | Bidirectional audio, voice selection, VAD events, transcription |
| [`tool-calling`](examples/tool-calling) | 3003 | L1 | `TypedTool` with auto-generated JSON Schema, `ToolDispatcher` routing |
| [`transcription`](examples/transcription) | 3004 | L0 | Every Gemini Live config option: VAD, activity handling, affective dialog, context compression, session resumption |
| [`agents`](examples/agents) | CLI | L1/L2 | Text agent combinators (`>>`, `\|`, `/`), `TypedTool`, copy-on-write builders |

### ADK Web UI (`gemini-adk-web-rs`)

The Web UI bundles all apps below into a single Axum server with a shared
devtools panel showing real-time state, timeline, transcript, and telemetry.

#### Crawl (Beginner)

| App | What It Demonstrates | Key SDK Features |
|-----|---------------------|-----------------|
| **text-chat** | Minimal text-only session — no microphone needed | `Live::builder().text_only()`, text streaming |
| **voice-chat** | Native audio chat with real-time transcription | `Modality::Audio`, voice selection, input/output transcription |
| **tool-calling** | Three demo tools: weather, time, calculator | `FunctionDeclaration`, `on_tool_call`, `NonBlocking` behavior, `WhenIdle` scheduling |

#### Walk (Intermediate)

| App | What It Demonstrates | Key SDK Features |
|-----|---------------------|-----------------|
| **all-config** | Configuration playground — every Gemini Live option in one app | Dynamic tool creation, modality switching, Google Search, code execution, context compression |
| **guardrails** | Real-time policy monitoring with corrective injection | `RegexExtractor`, `.watch()` state reactions, `.instruction_amendment()`, PII/off-topic/sentiment detection |
| **playbook** | 6-phase customer support flow with state extraction | `.phase()` chains, `.transition_with()` guards, `.greeting()`, `.with_context()`, `RegexExtractor` |

#### Run (Advanced)

| App | What It Demonstrates | Key SDK Features |
|-----|---------------------|-----------------|
| **support-assistant** | Multi-agent handoff between billing and technical support | Dual state machines (10 phases), `.computed()` derived state, cross-agent transitions, telemetry |
| **call-screening** | Incoming call screening with sentiment analysis and smart routing | Phase machine, tool calling (`check_contact_list`, `check_calendar`, `take_message`, `transfer_call`, `block_caller`), `NonBlocking` tools |
| **clinic** | HIPAA-aware telehealth scheduling with clinical triage | 8 tools (`verify_patient`, `check_availability`, `book_appointment`, etc.), patient intake flow, department routing |
| **restaurant** | Restaurant reservation and ordering system | 6 tools (`check_availability`, `make_reservation`, `get_menu`, etc.), dietary handling, occasion tracking |
| **debt-collection** | FDCPA-compliant debt collection with compliance gates | `StateKey<T>`, identity verification, payment negotiation, cease-and-desist handling, compliance watchers |

### Platform Support

All examples work with both **Google AI** (API key) and **Vertex AI** (project/location).
The SDK auto-strips unsupported features on Vertex AI — no code changes needed:

| Feature | Google AI | Vertex AI |
|---------|-----------|-----------|
| Async tool calling (`NonBlocking`, `WhenIdle`/`Silent`) | Supported | Stripped automatically |
| Thinking (`thinkingConfig`) | Supported | Stripped automatically |

---

## Common Errors & Solutions

### Vertex AI sends binary WebSocket frames

**Symptom:** `serde_json::from_str` fails on messages from Vertex AI.

**Cause:** Vertex AI sends Binary WebSocket frames, not Text frames (unlike
Google AI).

**Solution:** Already handled by `TungsteniteTransport::recv()`. If you build a
custom transport, handle both `Message::Text` and `Message::Binary`.

### Native audio model only supports AUDIO output modality

**Symptom:** Error when requesting `Modality::Text` with
`GeminiLive2_5FlashNativeAudio`.

**Solution:** Use `Modality::Audio` only, or switch to `Gemini2_0FlashLive`
which supports text output:

```rust
// Correct for native audio model:
config.response_modalities(vec![Modality::Audio])

// For text output, use the non-native model:
.model(GeminiModel::Gemini2_0FlashLive)
```

### Vertex AI endpoint URL

**Symptom:** Connection fails to `global-aiplatform.googleapis.com`.

**Solution:** Use `aiplatform.googleapis.com` (no `global-` prefix). The SDK
handles this automatically via the `Platform` enum.

### Tool declarations cannot be updated mid-session

**Symptom:** Attempting to add or remove tools after `connect()`.

**Cause:** The Gemini Live API does not support updating tool definitions after
session setup.

**Solution:** Declare all tools upfront. Use per-phase `tools_enabled` to
control which tools the model can call at any given point in the conversation.

### Extraction returns stale data

**Symptom:** `handle.extracted::<T>(name)` returns the previous turn's data.

**Cause:** Extractors run asynchronously on the control lane after each turn
completes.

**Solution:** Use the `on_extracted` callback for real-time notifications, or
poll `handle.extracted()` after the turn-complete event.

### State key not found despite being set

**Symptom:** `state.get("risk")` returns `None` even though you called
`state.set("derived:risk", 0.85)`.

**Solution:** The derived fallback works correctly: `get("risk")` checks
`derived:risk` automatically. However, `get("app:risk")` does NOT trigger the
fallback -- prefixed keys are looked up exactly as specified.

### Session disconnects after inactivity

**Symptom:** Server sends `GoAway` and closes the connection.

**Solution:** Handle gracefully with `.on_go_away(|ttl| async move { ... })`.
Enable session resumption with `.session_resume(true)` for transparent reconnect
support.

### Context window fills up in long conversations

**Symptom:** Model responses degrade in quality after many turns.

**Solution:** Enable context window compression:

```rust
Live::builder()
    .context_compression(4000, 2000)  // trigger at 4k tokens, compress to 2k
```

---

## Development

### Prerequisites

| Requirement | Version | Purpose |
|------------|---------|---------|
| **Rust** | 1.75+ | Language toolchain ([install](https://rustup.rs/)) |
| **cargo** | (bundled) | Build system and package manager |
| **pkg-config** | any | Locates system libraries |
| **OpenSSL** | 1.1+ | TLS for WebSocket connections |
| **ALSA dev** (Linux) | any | Audio I/O for voice examples |

**Quick setup (Ubuntu/Debian):**

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Install system dependencies
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev libasound2-dev build-essential
```

**Quick setup (macOS):**

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# System deps (OpenSSL via Homebrew)
brew install openssl pkg-config
```

**Environment variables:**

```bash
# Google AI (API key auth)
export GEMINI_API_KEY="your-api-key"

# Vertex AI (service account auth)
export GOOGLE_CLOUD_PROJECT="your-project-id"
export GOOGLE_CLOUD_LOCATION="us-central1"
```

### Build

```bash
cargo build --workspace
```

### Test

```bash
cargo test --workspace
```

### Lint

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

### Run the Web UI

```bash
cd apps/gemini-adk-web-rs
GEMINI_API_KEY="your-key" cargo run
# Open http://localhost:3000
```

### Generate documentation

```bash
cargo doc --workspace --no-deps --open
```

### Feature flags (gemini-genai-rs)

```bash
# Default: live + vad + tracing
cargo build -p gemini-genai-rs

# With REST APIs
cargo build -p gemini-genai-rs --features generate,embed,files

# Everything
cargo build -p gemini-genai-rs --features all-apis,metrics,opus
```

---

## Project Structure

```
gemini-rs/
  crates/
    gemini-genai-rs/              L0: Wire protocol, transport, types
    gemini-adk-rs/                L1: Agent runtime, state, phases, tools
    gemini-adk-fluent-rs/         L2: Fluent builder API, operators
  examples/
    text-chat/             Minimal text-only session (L0)
    voice-chat/            Bidirectional audio chat (L0)
    tool-calling/          TypedTool + ToolDispatcher (L1)
    transcription/         Every Gemini Live config option (L0)
    agents/                Text agent combinators (L1/L2)
    INDEX.md               Full example reference with per-app docs
  apps/
    gemini-adk-web-rs/               Multi-app Web UI with devtools (L2)
      src/apps/            13 showcase apps (see examples/INDEX.md)
  tools/
    gemini-adk-transpiler-rs/        Python ADK to Rust transpiler
  Cargo.toml               Workspace root
```

---

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for
details.
