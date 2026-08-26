<div class="hero">
  <img class="hero-mark" src="./assets/brand/logo-mark.svg" alt="gemini-rs">
  <div class="hero-eyebrow">v0.8 · Rust SDK</div>

# gemini-rs

  <p class="hero-sub">
    Full-duplex Gemini Live agents, in Rust. One wire protocol, one governed
    runtime, one fluent API — from raw WebSocket frames to voice agents that
    steer themselves, in three layered crates.
  </p>
  <div class="hero-cta">
    <a class="primary" href="./setup-and-running.md">Get Started →</a>
    <a class="secondary" href="./cookbooks.md">Browse 40 Cookbooks</a>
    <a class="secondary" href="https://github.com/vamsiramakrishnan/gemini-rs">GitHub</a>
  </div>
</div>

<div class="stat-row">
  <div class="stat"><b>3</b><span>Layered Crates</span></div>
  <div class="stat"><b>40</b><span>Cookbook Recipes</span></div>
  <div class="stat"><b>0</b><span>Auth Ceremony</span></div>
  <div class="stat"><b>&lt;1ms</b><span>Framework Overhead</span></div>
</div>

Most SDKs make you choose: raw control over the wire, or an ergonomic API that
hides too much. **gemini-rs gives you both, without compromise.** Drop to
`gemini-genai-rs` for byte-level control of the Multimodal Live protocol, or
reach for `gemini-adk-fluent-rs` and have a governed voice agent talking in
five lines — the same `State` spine and Live session underpin every layer, so
you never hit a ceiling.

```text
┌───────────────────────────────────────────────────────────┐
│  gemini-adk-fluent-rs  ·  L2 — Fluent DX                   │
│  AgentBuilder · Live · S·C·T·P·M·A · .govern / .on_enter   │
├───────────────────────────────────────────────────────────┤
│  gemini-adk-rs  ·  L1 — Agent Runtime                      │
│  Agent · Tools · State · Phases · TextAgent · LLM          │
│  Governed Agents: Flow · Extract · Resolver                │
├───────────────────────────────────────────────────────────┤
│  gemini-genai-rs  ·  L0 — Wire Protocol                    │
│  Transport · Session · Protocol · VAD · Buffers            │
└───────────────────────────────────────────────────────────┘
```

> **Governed Agents** — `Flow` (control DAG), `Extract` (deterministic +
> async fact resolution), and `Resolver` (orchestration) are **L1 runtime
> primitives**, not a layer above the fluent API. L2 surfaces them
> ergonomically (`Live::govern`, `.extract_record`, `.on_enter`). They
> compose over one shared `State` spine — see [Governed Flows](./user-guide/flow.md),
> [Extraction](./user-guide/extraction.md), and
> [Agent Orchestration](./user-guide/orchestration.md).

## Quick Start

Zero auth ceremony. `connect_from_env()` figures out Google AI vs. Vertex AI
from your environment, and you're talking to a live voice agent in one
`await`.

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)   // native-audio Live model
    .voice(Voice::Kore)
    .instruction("You are a helpful voice assistant.")
    .on_audio(|pcm| { /* play audio */ })
    .on_text(|text| print!("{text}"))
    .connect_from_env()
    .await?;

handle.send_text("What's the weather like?").await?;
handle.disconnect().await?;
```

New here? [Set up your environment](./setup-and-running.md), wire up
[Authentication](./user-guide/auth-and-connecting.md), then work the
[cookbook](./cookbooks.md) Crawl → Walk → Run.

## Find your way around

<div class="card-grid">
  <div class="card">
    <h4>🚀 Getting Started</h4>
    <p>Local setup, the three-layer architecture, migrating from raw
    WebSockets, and the patterns worth knowing before you ship.</p>
  </div>
  <div class="card">
    <h4>🎙️ Voice &amp; Live Sessions</h4>
    <p>Real-time voice agents — phases, shared state, watchers, and the
    session lifecycle that keeps them honest.</p>
  </div>
  <div class="card">
    <h4>🛠️ Tools &amp; Extraction</h4>
    <p>The tool-calling system, deterministic + LLM-backed extraction
    pipelines, and MCP interop.</p>
  </div>
  <div class="card">
    <h4>🧩 Composition &amp; Patterns</h4>
    <p>Governed Flows, agent orchestration, text-agent combinators, the
    S·C·T·P·M·A operator algebra, and middleware.</p>
  </div>
  <div class="card">
    <h4>📚 Examples</h4>
    <p>40 progressive cookbook recipes — Crawl, Walk, Run, Governed — plus
    the interactive <code>gemini-adk-web-rs</code> demo suite.</p>
  </div>
  <div class="card">
    <h4>🖥️ ADK Web UI</h4>
    <p>The design system, dark/light theming, DevTools panels, and the
    built-in cookbook browser.</p>
  </div>
</div>

## API Reference

For exhaustive type and method documentation, dive into the
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
