# gemini-rs

Full Rust SDK for the Gemini Multimodal Live API — wire protocol, agent runtime, and fluent DX in three layered crates.

```text
┌─────────────────────────────────────────────────────┐
│  gemini-adk-fluent-rs (L2 — Fluent DX)                    │
│  AgentBuilder · Live · S·C·T·P·M·A · .govern/.on_enter │
├─────────────────────────────────────────────────────┤
│  gemini-adk-rs (L1 — Agent Runtime)                       │
│  Agent · Tools · State · Phases · TextAgent · LLM  │
│  Governed Agents: Flow · Extract · Resolver        │
├─────────────────────────────────────────────────────┤
│  gemini-genai-rs (L0 — Wire Protocol)                     │
│  Transport · Session · Protocol · VAD · Buffers    │
└─────────────────────────────────────────────────────┘
```

> **Governed Agents** — `Flow` (control DAG), `Extract` (deterministic + async
> fact resolution), and `Resolver` (orchestration) are **L1 runtime primitives**,
> *not* a layer above the fluent API. L2 surfaces them ergonomically
> (`Live::govern`, `.extract_record`, `.on_enter`). They compose over one shared
> `State` spine — see the [Governed Flows](./user-guide/flow.md),
> [Extraction](./user-guide/extraction.md), and
> [Agent Orchestration](./user-guide/orchestration.md) chapters.

## Quick Start

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

// `connect_from_env()` resolves Google AI vs Vertex AI from the environment —
// no auth ceremony. See "Authentication & Connecting" for the variables it reads.
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

New here? Start with [Setup and Running](./setup-and-running.md) and
[Authentication & Connecting](./user-guide/auth-and-connecting.md), then browse
the [cookbook](./cookbooks.md) (Crawl → Walk → Run).

## Guide Structure

This book is organized into six sections:

- **Getting Started** — Local setup, architecture overview, migration guide, and best practices
- **Voice & Live Sessions** — Building real-time voice agents with phases, state, and watchers
- **Tools & Extraction** — Tool system, deterministic + LLM extraction, MCP
- **Composition & Patterns** — Governed Flows, Agent Orchestration, text-agent combinators, S·C·T·P·M·A operators, middleware
- **Examples** — 40 progressive cookbook examples (Crawl/Walk/Run/Governed) plus interactive `gemini-adk-web-rs` demos
- **ADK Web UI** — Design system, dark/light mode, DevTools panels, and the cookbook browser

## API Reference

For detailed type and method documentation, see the [rustdoc API reference](./api/gemini_genai_rs/index.html).

| Crate | Layer | API Docs |
|-------|-------|----------|
| `gemini-genai-rs` | L0 — Wire Protocol | [gemini_genai_rs](./api/gemini_genai_rs/index.html) |
| `gemini-adk-rs` | L1 — Agent Runtime | [gemini_adk_rs](./api/gemini_adk_rs/index.html) |
| `gemini-adk-fluent-rs` | L2 — Fluent DX | [gemini_adk_fluent_rs](./api/gemini_adk_fluent_rs/index.html) |

## Links

- [GitHub Repository](https://github.com/vamsiramakrishnan/gemini-rs)
- [Contributing Guide](https://github.com/vamsiramakrishnan/gemini-rs/blob/main/CONTRIBUTING.md)
- [Changelog](https://github.com/vamsiramakrishnan/gemini-rs/blob/main/CHANGELOG.md)
