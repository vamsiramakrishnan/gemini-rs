# gemini-rs

**Rust SDK and runtime for text agents and live voice conversations.**

Rust SDKs and runtime components for Gemini text agents, Gemini Live sessions, governed conversation flows, typed tools, telephony, and offline flow simulation.

[![CI](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/ci.yml)
[![Docs](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/docs.yml/badge.svg)](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/docs.yml)
[![crates.io](https://img.shields.io/crates/v/gemini-adk-fluent-rs.svg)](https://crates.io/crates/gemini-adk-fluent-rs)
[![docs.rs](https://img.shields.io/docsrs/gemini-adk-fluent-rs)](https://docs.rs/gemini-adk-fluent-rs)
[![Rust](https://img.shields.io/badge/rust-1.93%2B-orange.svg)](rust-toolchain.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Book: <https://vamsiramakrishnan.github.io/gemini-rs/>

API reference: <https://vamsiramakrishnan.github.io/gemini-rs/api/gemini_genai_rs/index.html>

## Choose your first implementation

| Build | Start here | What to verify |
|---|---|---|
| A text agent | [Text agent](#text-agent) | One authenticated request and its returned text |
| A microphone conversation | [Voice session](#voice-session) | Audio dependencies, connection, and playback |
| A conversation with permitted actions | [Governed flows](#governed-flows) | State transitions and tool admission |
| A transport integration without the fluent layer | [Crate responsibilities](#what-it-contains) | The smallest crate and feature set you need |

Start with one path. Text agents do not require the system audio stack. Flow
simulation is useful before a live call, but a passing simulation does not
establish microphone, telephony, or provider behavior.


## What it contains

The workspace is split by responsibility:

| Crate | Responsibility |
| --- | --- |
| `gemini-genai-rs` | Gemini API transport and model-facing types |
| `gemini-adk-rs` | state, flows, tools, extraction, phases, watchers, and runtime semantics |
| `gemini-adk-fluent-rs` | fluent authoring API over the runtime |

The runtime can enforce flow state while a Live session is active. A tool call can be rejected when the current flow has not admitted that tool yet.

The same flow can be serialized, validated, simulated offline, edited in Flow Studio, and generated back into Rust.

## Install

Text agents need Rust 1.93+.

```toml
[dependencies]
gemini-adk-fluent-rs = "2.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Voice I/O is optional:

```toml
gemini-adk-fluent-rs = { version = "2.0", features = ["voice-io"] }
```

On Linux, voice builds need `pkg-config`, `libssl-dev`, and `libasound2-dev`. The text path does not pull in the system audio stack.

## Authentication

Google AI:

```bash
export GEMINI_API_KEY=...
```

Vertex AI:

```bash
export GOOGLE_GENAI_USE_VERTEXAI=true
export GOOGLE_CLOUD_PROJECT=my-project
export GOOGLE_CLOUD_LOCATION=us-central1
gcloud auth application-default login
```

Live sessions and text agents use the same environment-based credential resolution.

## Text agent

```rust
use gemini_adk_fluent_rs::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));

    let agent = AgentBuilder::new("assistant")
        .instruction("Answer in one sentence.")
        .build(llm)?;

    let state = State::new();
    state.set("input", "What is the Gemini Live API?")?;

    println!("{}", agent.run(&state).await?);
    Ok(())
}
```

From a clone:

```bash
cargo run -p example-quickstart --bin hello-text
```

## Voice session

```rust
use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    Live::builder()
        .instruction("You are a concise concierge.")
        .greeting("Ask how you can help.")
        .connect_from_env()
        .await?
        .talk()
        .await?;

    Ok(())
}
```

From the workspace:

```bash
cargo run -p example-quickstart --features voice --bin hello-voice
```

`talk()` uses the local audio devices. The lower-level `voice::pump()` API accepts and emits audio frames without depending on a specific device transport.

## Governed flows

A `Flow` describes step ordering, tool admission, completion guards, and terminal conditions.

```rust
let flow = Flow::new()
    .step("gather")
        .allow(["capture_party"])
        .done(Guard::captured(["party_size", "requested_time"]))
    .step("check")
        .after("gather")
        .allow(["check_availability"])
        .done(Guard::is_true("availability_checked"))
    .step("book")
        .after("check")
        .allow(["book_table"])
        .done(Guard::called_ok("book_table"))
    .never("book_table")
        .until(Guard::is_true("availability_checked"))
    .build()?;
```

When the flow is attached to a Live session, the runtime evaluates these guards before admitting gated tools.

The guard vocabulary is serializable. A `SessionSpec` can therefore carry the flow, tool declarations, extraction schemas, phases, watchers, runtime settings, and embedded tests as data rather than only as Rust source.

## SessionSpec and Flow Studio

Run the editor locally:

```bash
cargo run -p gemini-adk-web-rs
```

Open:

```text
http://localhost:25125/flows
```

Flow Studio reads and writes the same session document consumed by the runtime. The preview path runs the offline simulator against embedded tests. The code view uses the session document's Rust generator.

The studio does not define separate runtime semantics; it is a client of the same document and validation code.

## Typed tools

Use the tool APIs when the model needs callable application functions. Typed tools expose their schema to the runtime and feed results through the same session state and flow machinery.

See the [Tool System](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/tools.html) chapter for `TypedTool`, dispatch, MCP, HTTP tools, and tool result handling.

## Callback lanes

Live audio and control work have different latency requirements. The runtime separates callbacks into three paths:

- fast lane: synchronous audio chunks, text deltas, transcripts, and VAD events;
- control lane: async tool calls, extraction, phase changes, and turn boundaries;
- telemetry lane: counters outside the audio hot path.

Callbacks that may block should use the concurrent variants rather than run on the audio path.

## Telephony

The audio core is shared across local devices and phone transports.

The repository includes examples for:

- Twilio Media Streams;
- SIP/RTP with G.711 through the optional `sip` feature;
- AudioHook-compatible WebSocket integrations.

These transports feed audio and DTMF into the same session state and flow runtime.

SIP registration and SRTP are not implemented in the current example path. See the [telephony chapter](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/telephony.html) for the current support matrix.

## Examples

| Task | Start here |
| --- | --- |
| Text agent | `examples/quickstart` |
| Voice session | `examples/quickstart` with `voice` feature |
| Governed flow | `example-cookbook` binary `37-governed-flow` |
| Flow Studio | `gemini-adk-web-rs` |
| End-to-end voice spec | `examples/voice-spec-demo` |
| Telephony | `examples/telephony` and `examples/audiohook` |
| Cookbook index | [`examples/INDEX.md`](examples/INDEX.md) |

## CLI

Scaffold an application with:

```bash
cargo install gemini-adk-cli-rs
adk create my-agent
```

## Boundaries

- A flow can gate tool admission inside the gemini-rs runtime. It cannot make an external tool idempotent by itself.
- Offline simulation checks the declared session model and embedded test events. It is not a substitute for a live model/transport integration test.
- Generated Rust and serialized session documents share one runtime model, but compatibility still depends on the schema and crate versions being used together.
- The fast callback path assumes callback implementations obey its latency contract. Use concurrent callbacks for work that can block.

## Development

```bash
git clone https://github.com/vamsiramakrishnan/gemini-rs.git
cd gemini-rs
cargo test --workspace
```

The README quickstart programs are compiled in CI from the checked examples so documentation changes that break those examples fail the build.

## License

MIT. See [LICENSE](LICENSE).
