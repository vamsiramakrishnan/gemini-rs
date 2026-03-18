# ADK Devtools Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship `adk web` and `adk run` CLI commands backed by a binary-multiplexed WebSocket protocol and embedded dev UI, matching adk-js devtools feature parity while leveraging Rust's performance for voice-first agents.

**Architecture:** Two new crates — `adk-devtools` (library with DevServer, DevRepl, protocol types, embedded UI) and `adk-cli` (thin binary wrapping `cargo run` with env vars). The protocol uses a single WebSocket connection with binary frames for audio (zero base64 overhead) and JSON text frames for control/devtools channels.

**Tech Stack:** Axum (server), clap (CLI), rust-embed (static assets), rustyline (REPL), cpal (voice I/O, feature-gated), serde_json (protocol), vanilla JS/CSS (dev UI).

---

## Context

### Why Not REST?

adk-js uses REST + SSE because it's a text-first SDK — every interaction is request/response. gemini-rs is voice-first: bidirectional audio streaming, real-time VAD events, phase transitions, live state mutations, and telemetry all happen concurrently. REST + SSE cannot model this. The existing Web UI already uses pure WebSocket for this reason.

### What adk-js Has That We Don't

| Capability | adk-js | gemini-rs |
|-----------|--------|-----------|
| CLI tool (`adk` command) | 6 commands | None |
| Dev server with UI | Express + Angular | Web UI (demo, not product) |
| Interactive REPL | `adk run` with readline | None |
| Agent scaffolding | `adk create` | None |
| Standardized API protocol | REST + SSE | Ad-hoc WebSocket JSON |
| Agent discovery | Directory scan + dynamic import | Manual registration |

### What We Already Have

- Full agent runtime (L0/L1/L2 crates)
- Three-lane processor (fast/control/telemetry)
- Working Web UI with devtools panel (state, events, phases, telemetry, tools)
- WebSocket audio streaming (base64 over JSON)

---

## Crate Structure

```
┌─────────────────────────────────────────────────────┐
│  tools/adk-cli/                (Binary crate)       │
│  `adk create` scaffolding + `adk web`/`adk run`     │
│  thin wrapper: runs `cargo run` on user's project   │
├─────────────────────────────────────────────────────┤
│  crates/adk-devtools/          (Library crate)      │
│  DevServer · DevRepl · Protocol · Embedded UI       │
│  Depends on: adk-rs-fluent (L2)                     │
└─────────────────────────────────────────────────────┘
```

### `crates/adk-devtools/`

Library crate providing:

- **Protocol types** — Rust enums for all WebSocket messages (server→client, client→server), serde-derived
- **DevServer** — Axum server with WebSocket handler, agent registry, embedded static UI
- **DevRepl** — Terminal REPL with rustyline, optional voice mode via cpal
- **Embedded UI** — Vanilla JS/CSS dev UI compiled into the binary via `rust-embed`
- **`run()` entry point** — Reads `ADK_MODE` env var, dispatches to DevServer or DevRepl

### `tools/adk-cli/`

Thin binary crate providing:

- **`adk web [dir]`** — Sets `ADK_MODE=web`, runs `cargo run` in the user's project directory
- **`adk run <agent> [--voice]`** — Sets `ADK_MODE=run ADK_AGENT=<agent>`, runs `cargo run`
- **`adk create <name>`** — Scaffolds a new agent project (Cargo.toml, main.rs, .env)

---

## Agent Discovery

Rust cannot dynamically import files at runtime like JS. The architecture inverts: the **user's binary registers agents** with the devtools server.

### AgentDescriptor

```rust
pub struct AgentDescriptor {
    /// Human-readable name (used in URL paths and CLI)
    pub name: String,
    /// Short description shown in dev UI sidebar
    pub description: String,
    /// Category for grouping in UI
    pub category: AgentCategory,
    /// Feature flags controlling which devtools panels appear
    pub features: Vec<Feature>,
    /// Factory called per WebSocket connection to create a live session builder
    pub factory: Box<dyn Fn() -> LiveBuilder + Send + Sync>,
}

pub enum AgentCategory {
    Basic,
    Advanced,
    Showcase,
}

pub enum Feature {
    Voice,
    Text,
    Tools,
    Phases,
    Extraction,
    Evaluation,
}
```

### User Registration Pattern

```rust
// user's main.rs
use adk_devtools::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let billing = Agent::live()
        .name("billing-agent")
        .description("Handles billing inquiries")
        .category(AgentCategory::Showcase)
        .features(vec![Feature::Voice, Feature::Tools, Feature::Phases])
        .instruction("You handle billing inquiries...")
        .tools(billing_tools())
        .build();

    let support = Agent::live()
        .name("support-agent")
        .description("Tech support assistant")
        .instruction("You handle tech support...")
        .tools(support_tools())
        .build();

    adk_devtools::run(vec![billing, support])
}
```

### Entry Point Behavior

`adk_devtools::run()` reads the `ADK_MODE` environment variable:

| `ADK_MODE` | Behavior |
|-----------|----------|
| `web` (default) | Start Axum server with dev UI on `127.0.0.1:8000` |
| `run` | Start interactive REPL for the agent named in `ADK_AGENT` |
| unset | Default to `web` |

Additional env vars: `ADK_AGENT` (agent name for run mode), `ADK_HOST`, `ADK_PORT`.

This means `cargo run` in the user's project always works — no `adk` CLI required. The CLI is a convenience wrapper.

---

## Binary Multiplexed WebSocket Protocol

Single WebSocket connection per agent session. Two frame types:

### Binary Frames (Audio Hot Path)

```
┌──────────┬──────────────────────────────────────┐
│ 1 byte   │ Payload                              │
│ channel  │ Raw bytes (PCM16, JPEG, etc.)        │
├──────────┼──────────────────────────────────────┤
│ 0x01     │ Mic audio (client → server, PCM16)   │
│ 0x02     │ Model audio (server → client, PCM16) │
│ 0x03     │ Video frame (client → server, JPEG)  │
└──────────┴──────────────────────────────────────┘
```

- 1-byte channel tag + raw payload
- Zero serialization overhead — PCM16 bytes from Gemini go straight to browser
- 33% bandwidth savings over base64 encoding
- Router is a single `match bytes[0]` — effectively free

### Text Frames (Control + Devtools)

JSON objects with a `ch` (channel) field for routing:

```json
{"ch": "control", "type": "start", "agent": "billing-agent", "model": "gemini-2.5-flash-live"}
{"ch": "state", "key": "session:name", "value": "John", "prefix": "session"}
{"ch": "phase", "from": "greeting", "to": "identification", "reason": "guard"}
{"ch": "telemetry", "stats": {"rtt_ms": 142, "turns": 5, "audio_out_kb": 384}}
{"ch": "tool", "name": "lookup_account", "args": {"id": "12345"}, "result": {"name": "Jane"}}
{"ch": "transcript", "role": "user", "text": "Hello", "final": true}
{"ch": "text", "delta": "I can help"}
```

### Control Channel Messages

**Client → Server:**

| Type | Payload | Purpose |
|------|---------|---------|
| `start` | `{ agent, model?, voice? }` | Start session with named agent |
| `text` | `{ text }` | Send text message |
| `stop` | `{}` | End session |

**Server → Client:**

| Type | Payload | Purpose |
|------|---------|---------|
| `connected` | `{ session_id, agent }` | Session established |
| `turn_complete` | `{}` | Model finished responding |
| `interrupted` | `{}` | User interrupted model |
| `vad_start` | `{}` | Voice activity detected |
| `vad_end` | `{}` | Voice activity ended |
| `error` | `{ message }` | Error occurred |

### Devtools Channels (Server → Client Only)

| Channel | Purpose | Frequency |
|---------|---------|-----------|
| `state` | State key mutations | Per mutation |
| `phase` | Phase transitions | Per transition |
| `telemetry` | Aggregated metrics | 100ms debounce |
| `transcript` | Streaming transcription (user + model) | Per chunk |
| `text` | Model text output (delta + complete) | Per token |
| `tool` | Tool call lifecycle (invoked, result, error) | Per tool call |

All devtools data piggybacks on existing L1 processor callbacks — no new instrumentation needed in core crates.

---

## DevServer Architecture

### Routes

| Route | Method | Purpose |
|-------|--------|---------|
| `/` | GET | Serve embedded dev UI (index.html) |
| `/static/*` | GET | Serve embedded CSS/JS/assets |
| `/api/agents` | GET | List registered agents (JSON) |
| `/ws/:agent` | GET | WebSocket upgrade for named agent |

Four routes. The REST surface is intentionally minimal — the WebSocket carries everything.

### WebSocket Handler

Per-connection flow:

1. Client connects to `/ws/:agent`
2. Server upgrades to WebSocket
3. Client sends `{"ch": "control", "type": "start", "agent": "billing-agent"}`
4. Server looks up `AgentDescriptor` by name
5. Server calls `descriptor.factory()` to get a `LiveBuilder`
6. Server configures callbacks on the builder to emit on the WebSocket:
   - `on_audio` → binary frame `0x02`
   - `on_text` → `text` channel JSON
   - `on_state_change` → `state` channel JSON
   - `on_phase_change` → `phase` channel JSON
   - `on_telemetry` → `telemetry` channel JSON
   - `on_tool_call` → `tool` channel JSON
   - `on_transcript` → `transcript` channel JSON
7. Server calls `.connect()` to establish Gemini Live session
8. Incoming binary frames (0x01) → `handle.send_audio()`
9. Incoming text `{"ch": "control", "type": "text"}` → `handle.send_text()`
10. On disconnect → session drops automatically

### Static Asset Embedding

Dev UI assets embedded via `rust-embed`:

```rust
#[derive(RustEmbed)]
#[folder = "ui/dist/"]
struct UiAssets;
```

The `ui/dist/` directory contains the built dev UI (HTML, CSS, JS). Built as part of the crate's build process, or checked in as pre-built assets.

---

## Dev UI

### Tech Stack

Vanilla JS + CSS. No framework, no build step, no node_modules. The entire UI is ~4 files embedded in the binary:

- `index.html` — Layout shell
- `app.js` — WebSocket lifecycle, message routing, conversation rendering
- `devtools.js` — State inspector, events log, phase timeline, tool calls, telemetry
- `audio.js` — Mic capture (AudioWorklet, 16kHz PCM16) + playback (AudioWorklet, 24kHz)
- `styles.css` — Layout and theming

### Layout

```
┌──────────────────────────────────────────────────────────────┐
│  ┌─sidebar──┐  ┌─main─────────────────┐  ┌─devtools───────┐ │
│  │          │  │                       │  │                │ │
│  │ Agents   │  │   Conversation        │  │ [State]        │ │
│  │ ──────── │  │                       │  │ [Events]       │ │
│  │ ● billing│  │   [user]: Hello       │  │ [Phases]       │ │
│  │   support│  │   [model]: Hi! How    │  │ [Tools]        │ │
│  │   triage │  │   can I help?         │  │ [Telemetry]    │ │
│  │          │  │                       │  │                │ │
│  │          │  │   ◉ Listening...      │  │ session:name   │ │
│  │          │  │                       │  │  → "John"      │ │
│  │          │  │   ┌──────────┐ 🎤     │  │ derived:risk   │ │
│  │          │  │   │ Type...  │ Send   │  │  → "high"      │ │
│  │          │  │   └──────────┘        │  │                │ │
│  └──────────┘  └───────────────────────┘  └────────────────┘ │
└──────────────────────────────────────────────────────────────┘
```

### Panels

| Panel | Content | Data Source |
|-------|---------|-------------|
| **Agent Sidebar** | List of registered agents, click to connect/disconnect | `GET /api/agents` |
| **Conversation** | Chat bubbles with streaming text, voice activity indicators | `text` + `transcript` channels |
| **State Inspector** | Live key-value table grouped by prefix, highlights on change | `state` channel |
| **Events Log** | Chronological stream of all protocol messages with timestamps | All channels |
| **Phase Timeline** | Visual phase transitions: from → to, reason, duration | `phase` channel |
| **Tool Calls** | Tool name, args JSON, result/error, latency | `tool` channel |
| **Telemetry** | RTT, audio throughput, turn count, interruptions, uptime | `telemetry` channel |

### Audio Handling

**Mic capture (browser → server):**
- `AudioWorklet` at 16kHz mono (replaces deprecated ScriptProcessorNode)
- Float32 → PCM16 conversion in worklet thread
- Send as binary WebSocket frame: `[0x01][pcm16 bytes]`
- No base64, no JSON wrapping

**Playback (server → browser):**
- Receive binary WebSocket frame: `[0x02][pcm16 bytes]`
- `Int16Array` view directly on `ArrayBuffer` (no parsing)
- Queue on `AudioWorklet` at 24kHz with sequential scheduling
- `clearQueue()` on `interrupted` control message

### What We Reuse From Web UI

| File | Reuse | Changes |
|------|-------|---------|
| `audio.js` | Recording/playback logic | Upgrade to AudioWorklet, switch from base64 to binary frames |
| `devtools.js` | State grouping, event logging, telemetry rendering | Refactor into cleaner modules, add phase timeline |
| `app.js` | WebSocket lifecycle | Rewrite for binary protocol + agent sidebar |

---

## `adk run` — Interactive REPL

### Text Mode (default)

```
$ adk run billing-agent

  gemini-rs devtools v0.1.0
  Agent: billing-agent
  Model: gemini-2.5-flash-live
  Type 'exit' to quit, '/state' to dump state

[you]: I need to update my payment method
[billing-agent]: I can help with that. What's your account number?
[you]: 12345
[billing-agent]: I found your account, Jane Doe.

  ┄┄ state ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
  session:account_id = "12345"
  session:customer_name = "Jane Doe"
  phase: identification → billing
  ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄

[you]: exit
```

- Uses `rustyline` for readline with history
- Sends text via Live session
- Streams model text responses
- Shows state changes and phase transitions inline after each turn
- Slash commands: `/state` (full dump), `/phase` (current phase), `/quit`

### Voice Mode (`--voice`)

```
$ adk run billing-agent --voice

  gemini-rs devtools v0.1.0
  Agent: billing-agent (voice mode)
  Using: default microphone → default speaker
  Press Ctrl+C to quit

  ◉ Listening...
  ◆ Speaking...
  ◉ Listening...
```

- Uses `cpal` crate for system audio I/O (feature-gated: `voice-cli`)
- Mic → PCM16 → Live session → model audio → speaker
- Minimal output: listening/speaking indicators + state changes

### Replay Mode (`--replay`)

```bash
$ adk run billing-agent --replay test_scenario.json
```

```json
{
  "queries": [
    "I need to update my payment method",
    "12345",
    "Yes, replace it"
  ],
  "assert_state": {
    "session:account_id": "12345"
  }
}
```

- Non-interactive batch execution
- Sends queries sequentially, waits for `turn_complete` between each
- Optional state assertions (exit code 0 = pass, 1 = fail)
- Useful for CI smoke tests and demos

---

## `adk create` — Project Scaffolding

Generates a new agent project:

```bash
$ adk create my-agent
```

### Generated Structure

```
my-agent/
├── Cargo.toml
├── src/
│   └── main.rs
└── .env
```

### Generated `Cargo.toml`

```toml
[package]
name = "my-agent"
version = "0.1.0"
edition = "2024"

[dependencies]
adk-rs-fluent = "0.1"
adk-devtools = "0.1"
tokio = { version = "1", features = ["full"] }
```

### Generated `src/main.rs`

```rust
use adk_devtools::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = Agent::live()
        .name("my-agent")
        .description("A voice assistant")
        .instruction("You are a helpful voice assistant.")
        .build();

    adk_devtools::run(vec![agent])
}
```

### Generated `.env`

```bash
GOOGLE_CLOUD_PROJECT=your-project-id
GOOGLE_CLOUD_LOCATION=us-central1
GOOGLE_GENAI_USE_VERTEXAI=TRUE
GEMINI_MODEL=gemini-2.5-flash-live
```

---

## Scope & Priorities

### v1 (This Implementation)

| Component | Priority |
|-----------|----------|
| `adk-devtools` crate with protocol types | P0 |
| DevServer (Axum + WebSocket handler) | P0 |
| Embedded dev UI (vanilla JS) | P0 |
| `adk run` text REPL | P0 |
| `adk-cli` binary (`adk web`, `adk run`) | P0 |
| Binary audio frames | P0 |
| `adk run --voice` (cpal) | P1 |
| `adk run --replay` | P1 |
| `adk create` scaffolding | P1 |

### v2 (Future)

| Component | Priority |
|-----------|----------|
| `adk deploy cloud_run` | P2 |
| `adk api_server` (headless) | P2 |
| Conformance test framework (YAML) | P2 |
| TypeScript client SDK | P3 |
| Agent graph visualization (GraphViz DOT) | P3 |

---

## Dependencies

### `adk-devtools` (library)

```toml
[dependencies]
adk-rs-fluent = { path = "../adk-rs-fluent" }
axum = "0.8"
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.26"
rust-embed = "8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rustyline = "15"
tracing = "0.1"
mime_guess = "2"
dotenv = "0.15"

[dependencies.cpal]
version = "0.15"
optional = true

[features]
default = []
voice-cli = ["cpal"]
```

### `adk-cli` (binary)

```toml
[dependencies]
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
```

---

## Relationship to Web UI

The Web UI (`apps/adk-web/`) continues to exist as example apps. It is NOT replaced. Instead:

- Demo apps can be **trivially ported** to `adk-devtools` by wrapping their `Live::builder()` calls in `AgentDescriptor` factories
- The Web UI serves as a reference for how apps work
- `adk-devtools` is the production-grade version of what the Web UI prototypes

Over time, demo examples may migrate to use `adk_devtools::run()` instead of their own Axum server, eliminating the duplicated server code.
