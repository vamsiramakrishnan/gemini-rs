# gemini-rs

> Full Rust SDK for the Gemini Multimodal Live API -- wire protocol, agent runtime, and fluent DX in three layered crates.

[![CI](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/ci.yml)
[![Docs](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/docs.yml/badge.svg)](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/docs.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
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
  deployments (`rs-genai`).
- **Agent runtime** with typed state, phase machines, tool dispatch, text agent
  combinators, and a three-lane processor architecture (`rs-adk`).
- **Fluent builder API** where a production voice agent is 20 lines of
  declarative Rust, not 200 lines of boilerplate (`adk-rs-fluent`).

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
┌──────────────────────────────────────────────────────────────────────┐
│  adk-rs-fluent  (L2 -- Fluent DX)                                   │
│                                                                      │
│  Live::builder()  .  AgentBuilder  .  S.C.T.P.M.A operators         │
│  PhaseBuilder  .  WatchBuilder  .  Temporal patterns                 │
├──────────────────────────────────────────────────────────────────────┤
│  rs-adk  (L1 -- Agent Runtime)                                       │
│                                                                      │
│  LiveSessionBuilder  .  LiveHandle  .  Three-lane processor          │
│  State (prefix-scoped)  .  PhaseMachine  .  ToolDispatcher           │
│  TextAgent combinators  .  Extractors  .  Watchers  .  Telemetry    │
│  LlmAgent  .  Runner  .  SessionService  .  MCP  .  A2A            │
├──────────────────────────────────────────────────────────────────────┤
│  rs-genai  (L0 -- Wire Protocol)                                     │
│                                                                      │
│  Transport (WebSocket + Mock)  .  Codec (JSON)  .  Auth providers    │
│  SessionHandle  .  Protocol types  .  VAD  .  Jitter buffer         │
│  Telemetry (OTel + Prometheus)  .  REST APIs (feature-gated)         │
└──────────────────────────────────────────────────────────────────────┘
```

### Three-Lane Processor (L1)

All Live session events are routed through a zero-copy dispatcher into three
independent lanes:

| Lane | Handles | Latency target | Sync model |
|------|---------|----------------|------------|
| **Fast** | Audio chunks, text deltas, VAD events, transcription | < 1 ms | Sync callbacks, no locks |
| **Control** | Tool calls, phase transitions, extractors, watchers, turn lifecycle | Async | Owned `TranscriptBuffer`, `join_all` extractors |
| **Telemetry** | `SessionSignals` + `SessionTelemetry` | Debounced 100 ms | `AtomicU64` counters, CAS latency tracking |

---

## Quick Start

### Google AI (API Key)

```rust
use adk_rs_fluent::prelude::*;

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
use rs_genai::prelude::*;

let session = rs_genai::quick_connect(
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
| [`rs-genai`](crates/rs-genai) | L0 -- Wire | Protocol types, WebSocket transport, auth providers, VAD, jitter buffer, REST APIs (feature-gated). Full Rust equivalent of Google's `@google/genai`. |
| [`rs-adk`](crates/rs-adk) | L1 -- Runtime | Agent runtime with state management, phase machines, tool dispatch, text agent combinators, extractors, watchers, telemetry. Full Rust equivalent of Google's `@google/adk`. |
| [`adk-rs-fluent`](crates/adk-rs-fluent) | L2 -- Fluent | `Live::builder()` API, `AgentBuilder`, S.C.T.P.M.A operator algebra, composition patterns, test utilities. |

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
    .affective_dialog(true)             // emotionally expressive responses
    .context_compression(4000, 2000)    // auto-compress context window
    .on_audio(|data| speaker.write(data))
    .on_input_transcript(|text, _final| println!("[User] {text}"))
    .on_output_transcript(|text, _final| println!("[Agent] {text}"))
    .on_interrupted(|| async { speaker.flush().await })
    .on_turn_complete(|| async { println!("--- turn complete ---") })
    .connect_vertex(project, location, token)
    .await?;
```

**Available voices:** `Aoede`, `Charon`, `Fenrir`, `Kore`, `Puck` (default), or `Voice::Custom("name")`.

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
                scheduling: None,  // or Some(FunctionResponseScheduling::WhenIdle)
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
use rs_adk::State;
use rs_adk::state::StateKey;

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
| `session:` | Auto-tracked signals | Session |
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
        .prompt_on_enter(true)   // model speaks immediately on entry
        .transition("main", |s| s.get::<bool>("greeted").unwrap_or(false))
        .on_enter(|state, _writer| async move {
            state.set("entered_greeting", true);
        })
        .done()
    .phase("main")
        .dynamic_instruction(|s| {
            let topic: String = s.get("topic").unwrap_or_default();
            format!("Help the user with: {topic}")
        })
        .tools(vec!["search".into(), "lookup".into()])   // per-phase tool filter
        .guard(|s| s.get::<bool>("verified").unwrap_or(false))  // entry guard
        .transition("farewell", |s| s.get::<bool>("done").unwrap_or(false))
        .done()
    .phase("farewell")
        .instruction("Say goodbye and provide a reference number.")
        .terminal()
        .done()
    .initial_phase("greeting")
    .connect_vertex(project, location, token)
    .await?;
```

**Phase features at a glance:**

- Static or dynamic (state-driven) instructions
- `InstructionModifier` variants: `StateAppend`, `Conditional`, `CustomAppend`
- Per-phase tool filtering via `tools_enabled`
- Phase-level entry guards (block entry when predicate fails)
- `on_enter` / `on_exit` async callbacks with `State` + `SessionWriter`
- `on_enter_context` -- inject conversational context across transitions
- `prompt_on_enter` -- trigger model response immediately on phase entry
- Validated transition graph with history ring buffer (capped at 100 entries)

### Extraction Pipeline

Run out-of-band LLM calls after each turn to extract structured data from
the conversation transcript. Schema-guided via `schemars::JsonSchema`.

```rust
use schemars::JsonSchema;

#[derive(Deserialize, Serialize, JsonSchema)]
struct OrderState {
    phase: String,
    items: Vec<String>,
    total: Option<f64>,
}

let handle = Live::builder()
    .instruction("You are a restaurant order assistant.")
    .extract_turns::<OrderState>(
        flash_llm,  // Arc<dyn BaseLlm> -- any Gemini model
        "Extract: items ordered, quantities, order phase, running total",
    )
    .on_extracted(|name, value| async move {
        println!("Extracted {name}: {value}");
    })
    .connect_vertex(project, location, token)
    .await?;

// Read latest extraction at any time
let order: Option<OrderState> = handle.extracted("OrderState");
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
use adk_rs_fluent::prelude::*;

let prompt = P::role("a customer support agent for Acme Corp")
    + P::task("help customers with billing inquiries")
    + P::constraint("never reveal internal pricing formulas")
    + P::guidelines(vec![
        "Be empathetic and professional",
        "Confirm resolution before closing",
    ]);

let instruction = prompt.render();
```

### REST APIs (Feature-Gated)

The L0 crate also provides feature-gated access to Gemini REST APIs beyond
the Live WebSocket connection:

```toml
[dependencies]
rs-genai = { version = "0.1", features = ["generate", "embed", "files"] }
# Or enable everything:
# rs-genai = { version = "0.1", features = ["all-apis"] }
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

## Cookbook Examples

The `cookbooks/` directory contains runnable examples at increasing complexity:

| Cookbook | Description | Key features |
|---------|-------------|--------------|
| [`voice-chat`](cookbooks/voice-chat) | Minimal voice conversation | Audio I/O, VAD, transcription |
| [`text-chat`](cookbooks/text-chat) | Text-only conversation | Text streaming, turn lifecycle |
| [`tool-calling`](cookbooks/tool-calling) | Function calling demo | Tool declarations, dispatch, state tracking |
| [`transcription`](cookbooks/transcription) | Real-time speech-to-text | Input and output transcript callbacks |
| [`agents`](cookbooks/agents) | Multi-agent pipelines | Text agent combinators, routing |
| [`ui`](cookbooks/ui) | Full web UI with devtools | Axum WebSocket, phases, extractors, multi-agent handoff, telemetry |

The **ui** cookbook includes multiple showcase apps:

| App | Description | Difficulty |
|-----|-------------|------------|
| `support-assistant` | Multi-agent handoff (billing + technical) with state extraction, phase tracking, and evaluation | Advanced |
| `tool-calling` | Interactive tools (weather, time, calculator) | Beginner |
| `guardrails` | Content safety and policy enforcement | Intermediate |
| `playbook` | Phase-driven conversational flows | Intermediate |
| `debt-collection` | Regulated conversation with compliance phases | Advanced |

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
| **ALSA dev** (Linux) | any | Audio I/O for voice cookbooks |

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

### Run the UI cookbook

```bash
cd cookbooks/ui
GEMINI_API_KEY="your-key" cargo run
# Open http://localhost:3000
```

### Generate documentation

```bash
cargo doc --workspace --no-deps --open
```

### Feature flags (rs-genai)

```bash
# Default: live + vad + tracing
cargo build -p rs-genai

# With REST APIs
cargo build -p rs-genai --features generate,embed,files

# Everything
cargo build -p rs-genai --features all-apis,metrics,opus
```

---

## Project Structure

```
gemini-rs/
  crates/
    rs-genai/              L0: Wire protocol, transport, types
    rs-adk/                L1: Agent runtime, state, phases, tools
    adk-rs-fluent/         L2: Fluent builder API, operators
  cookbooks/
    voice-chat/            Minimal voice conversation
    text-chat/             Text-only conversation
    tool-calling/          Function calling example
    transcription/         Speech-to-text example
    agents/                Multi-agent pipelines
    ui/                    Full web UI with devtools
  tools/
    adk-transpiler/        Python ADK to Rust transpiler
  Cargo.toml               Workspace root
```

---

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for
details.
