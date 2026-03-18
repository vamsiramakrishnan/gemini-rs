# gemini-rs

Full Rust SDK for the Gemini Multimodal Live API — wire protocol, agent runtime, and fluent DX in three layered crates.

```text
┌─────────────────────────────────────────────────────┐
│  adk-rs-fluent (L2 — Fluent DX)                    │
│  AgentBuilder · Live · S·C·T·P·M·A operators       │
├─────────────────────────────────────────────────────┤
│  rs-adk (L1 — Agent Runtime)                       │
│  Agent · Tools · State · Phases · TextAgent · LLM  │
├─────────────────────────────────────────────────────┤
│  rs-genai (L0 — Wire Protocol)                     │
│  Transport · Session · Protocol · VAD · Buffers    │
└─────────────────────────────────────────────────────┘
```

## Quick Start

```rust,ignore
use adk_rs_fluent::prelude::*;

let handle = Live::builder()
    .model(GeminiModel::Gemini2_0Flash)
    .voice(Voice::Kore)
    .instruction("You are a helpful assistant.")
    .on_audio(|audio| { /* play audio */ })
    .on_text(|text| { /* display text */ })
    .connect()
    .await?;
```

## Guide Structure

This book is organized into six sections:

- **Getting Started** — Architecture overview, migration guide, and best practices
- **Voice & Live Sessions** — Building real-time voice agents with phases, state, and watchers
- **Tools & Extraction** — Tool system and structured data extraction from conversations
- **Composition & Patterns** — Text agent combinators, S·C·T·P·M·A operators, middleware
- **Examples** — 30 progressive cookbook examples (Crawl/Walk/Run) plus interactive `adk-web` demos
- **ADK Web UI** — Design system, dark/light mode, DevTools panels, and the cookbook browser

## API Reference

For detailed type and method documentation, see the [rustdoc API reference](./api/rs_genai/index.html).

| Crate | Layer | API Docs |
|-------|-------|----------|
| `rs-genai` | L0 — Wire Protocol | [rs_genai](./api/rs_genai/index.html) |
| `rs-adk` | L1 — Agent Runtime | [rs_adk](./api/rs_adk/index.html) |
| `adk-rs-fluent` | L2 — Fluent DX | [adk_rs_fluent](./api/adk_rs_fluent/index.html) |

## Links

- [GitHub Repository](https://github.com/vamsiramakrishnan/gemini-rs)
- [Contributing Guide](https://github.com/vamsiramakrishnan/gemini-rs/blob/main/CONTRIBUTING.md)
- [Changelog](https://github.com/vamsiramakrishnan/gemini-rs/blob/main/CHANGELOG.md)
