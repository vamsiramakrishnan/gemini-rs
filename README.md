# gemini-rs

### The model improvises. The conversation must not.

[![CI](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/ci.yml)
[![Docs](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/docs.yml/badge.svg)](https://github.com/vamsiramakrishnan/gemini-rs/actions/workflows/docs.yml)
[![crates.io](https://img.shields.io/crates/v/gemini-adk-fluent-rs.svg)](https://crates.io/crates/gemini-adk-fluent-rs)
[![Rust](https://img.shields.io/badge/rust-1.93%2B-orange.svg)](rust-toolchain.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**v1.0 · MIT** · [book](https://vamsiramakrishnan.github.io/gemini-rs/) · [API reference](https://vamsiramakrishnan.github.io/gemini-rs/api/gemini_genai_rs/index.html)

gemini-rs is a full Rust SDK for the Gemini Multimodal Live API. A live voice model will happily book the table before checking availability — not because the prompt was wrong, but because a prompt is advice, and advice is not enforcement. Here you declare the conversation as a contract — steps, completion guards, tool gates, ordering constraints — and the runtime enforces it while the model speaks: a tool the flow has not admitted does not execute, whatever the model intends. The same contract is a JSON document you can validate, simulate, test, and code-generate offline, and a canvas you can edit by hand.

## Quickstart

Rust 1.93+. On Linux, voice needs the audio and TLS headers:
`sudo apt-get install pkg-config libssl-dev libasound2-dev` (macOS needs nothing extra).

Every snippet below is a complete file, compiled in CI exactly as printed
([`examples/quickstart`](examples/quickstart) — a drift test fails if the README
and the compiled programs ever disagree).

**1. Add the dependencies** — one crate, one feature flag, and tokio:

<!-- quickstart:Cargo.toml -->
```toml
[dependencies]
gemini-adk-fluent-rs = { version = "1.0", features = ["voice-io"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Text agents work out of the box (`gemini-llm` is a default feature). `voice-io`
powers `talk()` and is opt-in because it pulls in system audio. Writing typed tools later
adds `serde`, `serde_json`, and `schemars = "0.8"` (the 0.8 pin matters — schemars 1.x
is a different trait).

**2. Set one environment variable:**

| Platform | Environment |
|---|---|
| **Google AI** (fastest) | `export GEMINI_API_KEY=…` — [get a key](https://aistudio.google.com/apikey) |
| **Vertex AI** | `export GOOGLE_GENAI_USE_VERTEXAI=true GOOGLE_CLOUD_PROJECT=my-project GOOGLE_CLOUD_LOCATION=us-central1`, then `gcloud auth application-default login` |

One variable serves the whole stack — Live sessions and text agents accept the
same `GEMINI_API_KEY` / `GOOGLE_GENAI_API_KEY` / `GOOGLE_API_KEY` chain.

**3a. First sound** — the whole voice app is `src/main.rs`:

<!-- quickstart:src/bin/hello_voice.rs -->
```rust
use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    Live::builder()
        .instruction("You are a helpful concierge.")
        .greeting("Greet the caller and ask how you can help.")
        .connect_from_env() // GEMINI_API_KEY, or the Vertex AI env vars
        .await?
        .talk() // microphone in, speakers out, barge-in handled
        .await?;
    Ok(())
}
```

`cargo run` and speak. You don't pick a model: connect resolves a default the
target platform actually serves (Google AI's catalog and Vertex AI's disagree),
and `GEMINI_LIVE_MODEL=…` (or `.model(…)`) overrides it. An interruption flushes the
speaker buffer instead of playing stale speech.

**3b. First token** — the text agent, no microphone or audio deps needed:

<!-- quickstart:src/bin/hello_text.rs -->
```rust
use gemini_adk_fluent_rs::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Reads GEMINI_API_KEY (Google AI) or the Vertex AI env vars.
    let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));

    let agent = AgentBuilder::new("assistant")
        .instruction("You are a concise assistant.")
        .build(llm)?;

    let state = State::new();
    state.set("input", "In one sentence: what is the Gemini Live API?")?;
    println!("{}", agent.run(&state).await?);
    Ok(())
}
```

**From a clone instead:** `git clone` this repo, then
`cargo run -p example-quickstart --bin hello-text` (or `--bin hello-voice`) —
same programs, workspace paths.

## Pick your path

| I want to… | Do this | Read this |
|---|---|---|
| Talk to a voice agent right now | Quickstart 3a above | [Voice & Live Sessions](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/live-sessions.html) |
| Build a text agent / pipeline | Quickstart 3b, then combinators (`>>` `\|` `/` `*`) | [Text Agents](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/text-agents.html) |
| Give the model tools | `TypedTool` / `T::simple` + `ToolDispatcher` | [Tool System](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/tools.html) |
| Make the conversation follow rules | `Flow` + `Live::govern` — the governed-flow demo runs offline, no key: `cargo run -p example-cookbook --bin 37-governed-flow` | [Governed Flows](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/flow.html) |
| Edit flows on a canvas | `cargo run -p gemini-adk-web-rs` → `http://localhost:25125/flows` | [Flow Studio](https://vamsiramakrishnan.github.io/gemini-rs/flow-studio.html) |
| Put an agent on a phone line | Twilio / SIP / AudioHook examples | [Telephony](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/telephony.html) |
| Learn by example | 30 progressive cookbook binaries | [`examples/INDEX.md`](examples/INDEX.md) |
| Scaffold a project | `cargo install gemini-adk-cli-rs` → `adk create my-agent` | [`tools/gemini-adk-cli-rs`](tools/gemini-adk-cli-rs) |

## One call, walked through

`examples/voice-spec-demo` places a real phone-style call with no human in the room: Gemini TTS plays the caller, Gemini Live answers as the agent, and one JSON document governs the agent. The document is the restaurant cookbook from the Flow Studio gallery, edited by hand — because the document the Studio edits is just JSON.

```text
$ cargo run -p example-voice-spec-demo
spec `trattoria-voice` valid: true
  embedded test `party books a table`: passed (6 events)
  embedded test `no booking before availability`: passed (3 events)
connected: models/gemini-2.5-flash-native-audio-preview-12-2025
  [flow] done: [] · active: [gather] · admitted tools: [capture_party]
[caller] Hi! I'd like a table for two tomorrow at seven in the evening.
  [flow] done: [check, gather] · active: [book] · admitted tools: [book_table]
[caller] Seven thirty works perfectly. Please book it.
  [flow] done: [book, check, farewell, gather] · active: []
wrote voice-spec-demo.wav — 27.3s of call audio
```

Read the trace, not the adjectives. Before the call, the document's embedded tests replay through the real flow monitor — offline, no API key. During the call, `book_table` is not admitted until an availability check has actually run; the constraint `never book_table until availability_checked` is enforced by the runtime, not requested of the model. The full log is checked in at [`examples/voice-spec-demo/run-receipt.txt`](examples/voice-spec-demo/run-receipt.txt); the document is [`spec.json`](examples/voice-spec-demo/spec.json) beside it.

The contract, in the fluent phrasing:

```rust
let flow = Flow::new()
    .step("gather").allow(["capture_party"])
        .done(Guard::captured(["party_size", "requested_time"]))
    .step("check").after("gather").allow(["check_availability"])
        .done(Guard::is_true("availability_checked"))
        .ground("Party of {party_size}.")
    .step("book").after("check").allow(["book_table"])
        .done(Guard::called_ok("book_table"))
    .step("farewell").after("book").terminal()
    .once("book_table")
    .never("book_table").until(Guard::is_true("availability_checked"))
    .build()?;

Live::builder().govern(flow).connect_from_env().await?.talk().await?;
```

Guards are a closed, serializable vocabulary — `is_true`, `captured`, `called_ok`, `resolved`, composed with any/all — which is why the same flow round-trips between Rust, JSON, and the canvas without loss, and why a stuck step can explain itself: the runtime prints the truth value of every atom it is waiting on.

## The document is the program

Everything a session is — flow, tools (mock, HTTP, MCP), extraction schemas, phases, watchers, computed state, memory slots, runtime tuning, and its own test suite — serializes into one `SessionSpec` document. `validate()` compiles it with did-you-mean diagnostics on state keys. `simulate` replays the embedded tests. `to_rust()` prints the equivalent fluent program; generated cookbook apps compile under `-D warnings`. Nothing here is Studio magic: the Studio is one client of the document.

<p align="center"><img src="docs/assets/studio/flow-studio.gif" alt="Flow Studio click-through: load a cookbook from the gallery, drag a node, validate, run the embedded tests, scrub a simulated session on the canvas, read the generated Rust" width="900"></p>

The Flow Studio (`cargo run -p gemini-adk-web-rs` → `/flows`) is the same document on a canvas. Conditional edges render dashed with their guard as the label; the badge is the compiler's verdict; the Preview scrubber is the offline simulator driving the canvas; the Code tab is `to_rust()`.

<p align="center"><img src="docs/assets/studio/canvas.png" alt="The clinic-intake DAG on the Studio canvas: a dashed conditional emergency edge labeled is_true(is_emergency) into a terminal close step, a green valid badge, and compile diagnostics naming the four steps and four tools" width="900"></p>

<p align="center"><img src="docs/assets/studio/preview.png" alt="The offline simulator scrubbing event 3 of 6 of an embedded test: identify and triage latched done, schedule active, the three conformance tests reported passed in the diagnostics console" width="900"></p>

Six industry cookbooks ship in the gallery — healthcare intake, debt collection, telecom support, call screening, returns desk, table booking — each with embedded conformance tests. A CI test walks the gallery and requires every cookbook to compile and pass its own tests. The tour lives in [the Flow Studio chapter](https://vamsiramakrishnan.github.io/gemini-rs/flow-studio.html).

## The expensive callback is the one that blocks

Live audio does not wait. Every session event is routed through three lanes with different latency contracts: a **fast lane** (sync, sub-millisecond — audio chunks, text deltas, transcripts, VAD), a **control lane** (async — tool calls, turn boundaries, extraction, phase transitions), and a **telemetry lane** (lock-free counters, off the hot path). The fast-lane contract is enforced by convention and documented per callback; a blocked audio callback costs the conversation, so hooks that need to block get a `_concurrent` variant that detaches instead.

The same discipline shapes the audio path. `voice::pump()` is the device-independent duplex core: feed microphone frames at any sample rate on one channel, receive playback at any rate on another, and an interruption arrives as an explicit `Playback::Flush` — stale audio is dropped, never played. `talk()` is `pump()` on cpal devices. Telephony is `pump()` on a phone line.

## A phone line is just another pump

Three runnable ends, one audio core:

**Twilio Media Streams** — the SDK speaks the protocol and bridges G.711 μ-law to the session at 8 kHz; barge-in becomes Twilio's `clear`, DTMF lands in session state where flow guards read it ([`examples/telephony`](examples/telephony)).

**No carrier at all** (feature `sip`) — an in-process SIP agent over [rsipstack](https://github.com/restsend/rsipstack): SDP negotiation, symmetric RTP, G.711 inside the SDK. Any softphone or PBX extension dials it directly:

```rust
let mut agent = SipAgent::bind("0.0.0.0:5060".parse()?).await?;
while let Some(incoming) = agent.next_call().await {
    let session = Live::builder()
        .instruction("Answer the phone politely.")
        .connect_from_env().await?;
    let call = incoming.answer(&session).await?;   // SDP answer + RTP starts
    tokio::spawn(async move { call.ended().await; });
}
```

**A contact-center platform's virtual-agent slot** — [`examples/audiohook`](examples/audiohook) is the proof that a new transport needs no SDK changes: a bot server speaking the open [AudioHook protocol](https://developer.genesys.cloud/devapps/audiohook/) (the WebSocket dialect a Genesys-style platform dials out to), with the wire protocol as a pure, offline-tested state machine and the session glue as one `select!` loop over the same `pump`.

All three paths land DTMF keypresses in session state where flow guards read them — Twilio as protocol events, SIP as RFC 4733 telephone events negotiated in the SDP answer, AudioHook as `dtmf` messages. Deliberately deferred: SIP registration and SRTP. The [telephony chapter](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/telephony.html) has the decision table.

## Choose your altitude

Three crates, each depending only on the one below, each usable alone:

| Crate | Layer | The contract |
|---|---|---|
| [`gemini-adk-fluent-rs`](crates/gemini-adk-fluent-rs) | L2 · authoring | Two equivalent phrasings — fluent chain and JSON document. Never invents runtime semantics. |
| [`gemini-adk-rs`](crates/gemini-adk-rs) | L1 · runtime | State, flows, tools, extraction, phases, watchers. Gives meaning and enforcement; never hides a decision. |
| [`gemini-genai-rs`](crates/gemini-genai-rs) | L0 · wire | A duplex authenticated frame stream, both platforms. Never interprets the conversation. |

Each layer's promise is named in code — `primitives` modules with compile-time drift tests, so a renamed or removed primitive breaks the contract at build time, not in a reader's mental model. [`gemini-memory-rs`](crates/gemini-memory-rs) sits beside the stack: durable memory as human-readable Markdown, prepared asynchronously, consumed synchronously, projected into governed state where guards read it.

Beyond flows, the L1/L2 surface covers the rest of a production session: typed tools with schemas derived from Rust types, per-tool policies (`confirm`/`timeout`/`cached`), background tool execution, MCP servers, out-of-band LLM extraction plus deterministic CPU recognizers (`#[derive(Extract)]` — no model in the control loop), phases with three steering modes, state watchers and temporal patterns, session persistence and repair, and text-agent combinators (`>>` `|` `/` `*`) with an eight-namespace composition algebra. Every one has a [book chapter](https://vamsiramakrishnan.github.io/gemini-rs/).

## What the tests hold

- 2,500+ workspace tests, none requiring an API key — including G.711 codec conformance against known wire values, RTP wire layouts, SDP offer/answer round-trips, a raw-UDP SIP signalling integration test, guard truth-trace suites, and codegen goldens.
- The Quickstart programs above are compiled in CI verbatim — a drift test pins the README's fences to [`examples/quickstart`](examples/quickstart).
- Every gallery cookbook compiles through the real flow compiler and passes its own embedded tests, in CI.
- Generated apps compile as standalone crates under `RUSTFLAGS="-D warnings"`.
- Layer contracts are drift-tested; docs build with `RUSTDOCFLAGS="-D warnings"`.

```bash
cargo test --workspace          # ~2,500 tests, no credentials
cargo run -p example-cookbook --bin 37-governed-flow    # governed flow, no credentials
cargo run -p gemini-adk-web-rs  # Web UI + Flow Studio → :25125
```

[`examples/`](examples/INDEX.md) holds 30 progressive cookbook binaries (builders → combinators → multi-agent → governed capstones), the telephony and SIP agents, the TTS-driven call above, and focused per-layer demos. [`apps/gemini-adk-web-rs`](apps/gemini-adk-web-rs) bundles 13 showcase apps with a shared DevTools panel.

## When it doesn't work

| Symptom | Cause and fix |
|---|---|
| Connect fails: *"model not found for API version v1beta"* or setup closes without `setupComplete` | The model isn't in your platform's Live catalog — Google AI and Vertex AI serve **different model names**. Leave `.model()` unset to get a platform-appropriate default, or list what your key can reach: `curl "https://generativelanguage.googleapis.com/v1beta/models?key=$GEMINI_API_KEY"` and look for `bidiGenerateContent` under `supportedGenerationMethods`. |
| Text agent errors: *"GeminiLlm requires the 'gemini-llm' feature flag"* | You built with `--no-default-features`; add `gemini-llm` back to the feature list (it is on by default). |
| No `talk()` method on the handle | Add `features = ["voice-io"]`; on Linux install `libasound2-dev` first. |
| `JsonSchema` trait bound errors on your tool structs, or "multiple versions of crate schemars" | Pin `schemars = "0.8"` — plain `cargo add schemars` installs 1.x, a different trait. |
| Live connects but the text agent authenticates with an empty key | You exported only `GOOGLE_API_KEY` with an older SDK — use `GEMINI_API_KEY`; since 1.0.1 all three names work everywhere. |
| `.on_thought()` never fires | Thinking is Google AI only; `thinkingConfig` is auto-stripped on Vertex AI. |

More in the book's [Troubleshooting & FAQ](https://vamsiramakrishnan.github.io/gemini-rs/user-guide/troubleshooting.html).

## Documentation

The [book](https://vamsiramakrishnan.github.io/gemini-rs/) is the reference — 30+ chapters from setup to the layer contract, deployed from [`docs/`](docs) on every push to `main`, with the merged [rustdoc API reference](https://vamsiramakrishnan.github.io/gemini-rs/api/gemini_genai_rs/index.html) beside it. [CLAUDE.md](CLAUDE.md) is the condensed map of the codebase. [ROADMAP.md](ROADMAP.md) says what is deliberately not built yet.

Building locally: Rust 1.93+, `pkg-config libssl-dev` (plus `libasound2-dev` for `voice-io`); `cargo build --workspace`; `mdbook build docs` for the book. Releases go through `just release <version>` — see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT — see [LICENSE](LICENSE).
