<div class="hero">
  <img class="hero-mark" src="./assets/brand/logo-mark.svg" alt="gemini-rs">
  <div class="hero-eyebrow">v2.0 · Rust SDK</div>

# gemini-rs

  <p class="hero-sub">
    <strong>The model improvises. The conversation must not.</strong><br>
    Full-duplex Gemini Live agents in Rust — one wire protocol, one governed
    runtime, one fluent API, in three layered crates.
  </p>
  <div class="hero-cta">

[Get started](./setup-and-running.md) [Browse the cookbook](./cookbooks.md) [GitHub](https://github.com/vamsiramakrishnan/gemini-rs)

  </div>
</div>

<div class="stat-row">
  <div class="stat"><b>3</b><span>layered crates</span></div>
  <div class="stat"><b>30</b><span>cookbook recipes</span></div>
  <div class="stat"><b>2,500+</b><span>tests, no API key</span></div>
  <div class="stat"><b>6</b><span>Studio cookbooks</span></div>
</div>

gemini-rs treats a conversation as a contract: you declare the flow — steps,
completion guards, tool gates, ordering constraints — and the runtime
enforces it while the model speaks. The same contract is a JSON document that
validates, simulates, tests, and code-generates offline, and a canvas you can
edit by hand. Drop to `gemini-genai-rs` for byte-level control of the
Multimodal Live protocol, or reach for `gemini-adk-fluent-rs` and have a
governed voice agent talking in five lines — the same `State` spine underpins
every layer, so you never hit a ceiling.

<p align="center"><img src="./assets/diagrams/architecture-stack.svg" alt="Three-crate layered architecture: L2 fluent DX over L1 runtime over L0 wire protocol" width="720"></p>

| Crate | Layer | Use it when… |
|-------|-------|--------------|
| `gemini-adk-fluent-rs` | **L2 — Fluent DX** | You're building an application: `Live::builder()`, `AgentBuilder`, the S·C·T·P·M·A algebra, `SessionSpec`, voice I/O, telephony. **Start here.** |
| `gemini-adk-rs` | **L1 — Agent runtime** | You need the runtime directly: `State`, phases, tool dispatch, `Flow`, extraction, watchers, combinators, telemetry. |
| `gemini-genai-rs` | **L0 — Wire protocol** | You need raw WebSocket access, custom transports, or the feature-gated REST APIs. |

Plus `gemini-memory-rs`, a [contextual memory engine](./memory.md) for Live
sessions, independent of the stack.

> **Governed Agents** — `Flow` (control DAG), `Extract` (deterministic + async
> fact resolution), and `Resolver` (orchestration) are **L1 runtime primitives**,
> surfaced ergonomically at L2 (`Live::govern`, `.extract_record`, `.on_enter`).
> They compose over one shared `State` spine — see
> [Governed Flows](./user-guide/flow.md), [Extraction](./user-guide/extraction.md),
> and [Agent Orchestration](./user-guide/orchestration.md).

## Quick start

A voice agent in five lines *(feature `voice-io`)*:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

Live::builder()
    .instruction("You are a helpful concierge.")
    .greeting("Greet the caller.")
    .connect_from_env().await?     // Google AI or Vertex — resolved from env
    .talk().await?;                // microphone in, speakers out, barge-in handled
```

Or with explicit callbacks:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let handle = Live::builder()
    .voice(Voice::Kore)
    .instruction("You are a helpful voice assistant.")
    .on_audio(|pcm| { /* play audio */ })
    .on_text(|text| print!("{text}"))
    .connect_from_env()
    .await?;

handle.send_text("What's the weather like?").await?;
handle.disconnect().await?;
```

New here? Start with [Setup and Running](./setup-and-running.md) and
[Authentication & Connecting](./user-guide/auth-and-connecting.md), then browse
the [cookbook](./cookbooks.md) (Crawl → Walk → Run → Governed).

## Sessions as data — and the Flow Studio

A whole governed session — flow DAG, tools, extraction, phases, watchers,
computed state, memory, runtime tuning, and an embedded test suite — can be
one JSON document (`SessionSpec`). It validates, simulates, and code-generates
offline, and the [Flow Studio](./flow-studio.md) is a drag-and-drop editor
over exactly that document:

<p align="center"><img src="./assets/studio/flow-studio.gif" alt="Flow Studio click-through: load a cookbook, drag nodes, validate, run embedded tests, scrub a simulated session, read the generated Rust" width="860"></p>

See [Flows as JSON](./user-guide/flow-json.md) for the format and
[the Studio tour](./flow-studio.md) for the editor.

## Find your way around

<div class="card-grid">
  <div class="card">
    <h4>Getting started</h4>
    <p>Setup, authentication, the three-layer architecture and its contract,
    migrating from raw WebSockets, best practices.</p>
  </div>
  <div class="card">
    <h4>Voice &amp; Live sessions</h4>
    <p>Live sessions and callbacks, voice I/O, telephony (Twilio + SIP),
    phases, steering, state, watchers, persistence, record &amp; replay.</p>
  </div>
  <div class="card">
    <h4>Tools &amp; extraction</h4>
    <p>The tool system, per-tool policies, MCP interop, and deterministic +
    LLM-backed extraction pipelines.</p>
  </div>
  <div class="card">
    <h4>Composition &amp; patterns</h4>
    <p>Governed flows, flows as JSON, the Flow Studio, orchestration,
    text-agent combinators, the S·C·T·P·M·A algebra, middleware.</p>
  </div>
  <div class="card">
    <h4>Memory</h4>
    <p>The durable memory engine — async-prepare, sync-consume — and its
    declarative binding into governed state.</p>
  </div>
  <div class="card">
    <h4>Examples &amp; Web UI</h4>
    <p>30 progressive cookbook recipes plus the interactive
    <code>gemini-adk-web-rs</code> demo suite and DevTools.</p>
  </div>
</div>

## API reference

For detailed type and method documentation, see the
[rustdoc API reference](./api/gemini_genai_rs/index.html).

| Crate | Layer | API Docs |
|-------|-------|----------|
| `gemini-genai-rs` | L0 — Wire Protocol | [gemini_genai_rs](./api/gemini_genai_rs/index.html) |
| `gemini-adk-rs` | L1 — Agent Runtime | [gemini_adk_rs](./api/gemini_adk_rs/index.html) |
| `gemini-adk-fluent-rs` | L2 — Fluent DX | [gemini_adk_fluent_rs](./api/gemini_adk_fluent_rs/index.html) |

## Links

- [GitHub Repository](https://github.com/vamsiramakrishnan/gemini-rs)
- [Contributing Guide](https://github.com/vamsiramakrishnan/gemini-rs/blob/main/CONTRIBUTING.md)
- [Changelog](https://github.com/vamsiramakrishnan/gemini-rs/blob/main/CHANGELOG.md)
