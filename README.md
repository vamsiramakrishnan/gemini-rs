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

API reference: <https://docs.rs/gemini-adk-fluent-rs>

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

<!-- quickstart:Cargo.toml -->
```toml
[dependencies]
gemini-adk-fluent-rs = "2.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
# For typed answers and tool arguments:
serde = { version = "1", features = ["derive"] }
schemars = "0.8"
```

Voice I/O is optional:

<!-- quickstart:Cargo.toml:voice -->
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

`GeminiLlm::from_env()` reads these and fails before any request, naming the
variable to set, when one is missing. Live sessions use the same variables.

## Text agent

Ask a question:

<!-- quickstart:src/bin/hello_text.rs -->
```rust
use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("assistant")
        .instruction("Answer in one sentence.")
        .build(GeminiLlm::from_env()?)?;

    println!("{}", agent.ask("What is the Gemini Live API?").await?);
    Ok(())
}
```

Each program below adds one idea. Hold a conversation and stream the reply:

<!-- quickstart:src/bin/chat.rs -->
```rust
use gemini_adk_fluent_rs::prelude::*;
use std::io::Write;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("assistant")
        .instruction("Be brief.")
        .build(GeminiLlm::from_env()?)?;

    let mut chat = agent.chat();
    for message in ["Hi, I'm Ada.", "What's my name?"] {
        println!("> {message}");
        let mut reply = chat.send_stream(message);
        while let Some(event) = reply.next().await {
            if let RunEvent::TextDelta(text) = event? {
                print!("{text}");
                std::io::stdout().flush().ok();
            }
        }
        println!();
    }
    Ok(())
}
```

Get a typed answer — the type's JSON Schema is sent as the response schema,
and a reply that does not parse is sent back once for correction:

<!-- quickstart:src/bin/typed.rs -->
```rust
use gemini_adk_fluent_rs::prelude::*;

/// The model's answer, as a Rust type: its JSON Schema is the response schema.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct City {
    name: String,
    country: String,
    /// Population, in millions.
    population_millions: f64,
}

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("geographer").build(GeminiLlm::from_env()?)?;

    let city: City = agent.ask_as("Describe the largest city in Japan.").await?;
    println!(
        "{}, {}: {:.1} million people",
        city.name, city.country, city.population_millions
    );
    Ok(())
}
```

Give the agent a tool — a documented `async fn`, whose doc comment is what the
model reads:

<!-- quickstart:src/bin/tool.rs -->
```rust
use gemini_adk_fluent_rs::prelude::*;

/// Get the current weather for a city.
///
/// # Arguments
///
/// * `city` - The city name, e.g. "Paris".
#[tool]
async fn get_weather(city: String) -> serde_json::Value {
    // A real tool would call a weather service here.
    serde_json::json!({ "city": city, "condition": "rain", "celsius": 14 })
}

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("weather")
        .instruction("Answer weather questions using your tool.")
        .tool(get_weather())
        .build(GeminiLlm::from_env()?)?;

    println!("{}", agent.ask("Do I need an umbrella in Paris?").await?);
    Ok(())
}
```

Test it without a model. `MockLlm` replies from a script and records every
request, so a test asserts on what the agent sent as well as what it answered:

<!-- quickstart:tests/agent_test.rs -->
```rust
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::{LlmResponse, MockLlm};

/// Look up the status of an order.
///
/// # Arguments
///
/// * `id` - The order id, e.g. "A-17".
#[tool]
async fn order_status(id: String) -> String {
    format!("order {id} has shipped")
}

#[tokio::test]
async fn the_agent_checks_the_order_before_answering() -> Result<(), Box<dyn std::error::Error>> {
    let llm = MockLlm::script([
        LlmResponse::tool_call("order_status", serde_json::json!({ "id": "A-17" })),
        LlmResponse::from_text("Your order has shipped."),
    ]);
    let agent = AgentBuilder::new("support")
        .tool(order_status())
        .build(llm.clone())?;

    assert_eq!(
        agent.ask("Where is order A-17?").await?,
        "Your order has shipped."
    );

    // The tool ran, and its result went back to the model.
    let followup = serde_json::to_string(&llm.last_request().unwrap().contents)?;
    assert!(followup.contains("order A-17 has shipped"));
    Ok(())
}
```

From a clone:

```bash
cargo run -p example-quickstart --bin hello-text   # also: chat, typed, tool
cargo test -p example-quickstart
```

## Voice session

<!-- quickstart:src/bin/hello_voice.rs -->
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

## Tools

A tool is a documented `async fn` marked `#[tool]`: the doc comment is its
description, a `# Arguments` section describes its parameters, the parameter
types are its schema, and it may return any `Serialize` value or a `Result`
with any error. The same tool works on a text agent (`AgentBuilder::tool`) and
a Live session (`Live::builder().tool`), and a governed flow can gate it.

See the [Tool System](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/tools.html) chapter for closures with typed arguments (`T::typed`), confirmation, MCP, HTTP tools, and result handling.

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
| Text agent: ask, chat, typed answers, tools, tests | `examples/quickstart` |
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
cd my-agent && cargo run
```

The generated `src/main.rs` is compiled in this repository's CI, so a new
project builds on the release it names.

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
