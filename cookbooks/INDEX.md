# Cookbook Examples

Runnable examples demonstrating gemini-rs features, organized by difficulty.

## Getting Started

1. Copy `.env.example` to `.env` and configure your credentials:
   - **Google AI**: Set `GOOGLE_GENAI_API_KEY`
   - **Vertex AI**: Set `GOOGLE_GENAI_USE_VERTEXAI=TRUE`, `GOOGLE_CLOUD_PROJECT`, and `GOOGLE_CLOUD_LOCATION`
2. Optionally set `GEMINI_MODEL` to override the default model.

### Standalone cookbooks

```bash
cargo run -p cookbook-text-chat       # http://127.0.0.1:3001
cargo run -p cookbook-voice-chat      # http://127.0.0.1:3002
cargo run -p cookbook-tool-calling    # http://127.0.0.1:3003
cargo run -p cookbook-transcription   # http://127.0.0.1:3004
```

### Multi-app UI

```bash
cargo run -p cookbook-ui              # http://127.0.0.1:3000
```

All apps listed below are available in the multi-app UI with a shared devtools panel showing state, transcript, and telemetry.

---

## Index

### Crawl (Beginner)

| App | Description | Features | Crate Level |
|-----|-------------|----------|-------------|
| **text-chat** | Minimal text-only Gemini Live session | text | L0 wire |
| **voice-chat** | Native audio voice chat with Gemini Live | voice, transcription | L0 wire |
| **tool-calling** | Function calling with Gemini Live | text, tools | L1 runtime |

These examples demonstrate the basics: connecting to Gemini Live, sending text or audio, and receiving responses. Start here to understand the wire protocol and session lifecycle.

### Walk (Intermediate)

| App | Description | Features | Crate Level |
|-----|-------------|----------|-------------|
| **transcription** | All configurable Gemini Live API properties | transcription, VAD, affective dialog, context compression | L0 wire |
| **guardrails** | Policy monitoring + corrective injection for live conversations | voice, transcription, guardrails | L2 fluent |
| **playbook** | State machine + text agent evaluation for customer support | voice, transcription, state-machine, evaluation | L2 fluent |
| **all-config** | Configuration playground -- every Gemini Live option | text, voice, tools, transcription | L2 fluent |

These examples introduce L2 fluent API features: `RegexExtractor` for structured data extraction, `PhaseMachine` for conversation flow, and guardrail policies.

### Run (Advanced)

| App | Description | Features | Crate Level |
|-----|-------------|----------|-------------|
| **support-assistant** | Multi-agent handoff with billing + technical support flows | voice, transcription, state-machine, evaluation, guardrails, multi-agent | L2 fluent |
| **debt-collection** | FDCPA-compliant debt collection with compliance gates, emotional monitoring, and payment negotiation | phase-machine, compliance-gates, temporal-patterns, llm-extraction, tool-response-redaction, numeric-watchers, computed-state, turn-boundary-injection | L2 fluent |
| **weather-agent** | Standalone agent with typed tool calling | tools | L2 fluent |
| **research-pipeline** | Multi-agent research pipeline | tools, multi-agent | L2 fluent |

These examples demonstrate the full L2 pipeline: `PhaseMachine` with multiple phases, `ComputedRegistry` for derived state, `WatcherRegistry` for state-change reactions, `TemporalRegistry` for time-based pattern detection, and LLM-based extraction with `TextAgentTool`.
