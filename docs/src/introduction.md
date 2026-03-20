# gemini-rs

Full Rust SDK for the Gemini Multimodal Live API — wire protocol, agent runtime, and fluent DX in three layered crates.

```text
┌─────────────────────────────────────────────────────┐
│  gemini-adk-fluent-rs (L2 — Fluent DX)                    │
│  AgentBuilder · Live · S·C·T·P·M·A operators       │
├─────────────────────────────────────────────────────┤
│  gemini-adk-rs (L1 — Agent Runtime)                       │
│  Agent · Tools · State · Phases · TextAgent · LLM  │
├─────────────────────────────────────────────────────┤
│  gemini-genai-rs (L0 — Wire Protocol)                     │
│  Transport · Session · Protocol · VAD · Buffers    │
└─────────────────────────────────────────────────────┘
```

## Quick Start

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

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
- **Examples** — 30 progressive cookbook examples (Crawl/Walk/Run) plus interactive `gemini-adk-web-rs` demos
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
