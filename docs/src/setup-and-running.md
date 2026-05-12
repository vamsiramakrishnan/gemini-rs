# Setup and Running

This guide gets the workspace, cookbook examples, and ADK Web UI running from a fresh checkout.

## 1. Install Prerequisites

### Ubuntu or Debian

```bash
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev libasound2-dev build-essential
```

### macOS

```bash
xcode-select --install
brew install pkg-config openssl
```

You also need a stable Rust toolchain:

```bash
rustup update stable
rustup default stable
```

## 2. Choose Authentication

Create `.env` in the repository root.

### Google AI

Use this for the fastest local setup.

```env
GOOGLE_API_KEY=your-api-key
```

### Vertex AI

Use this for project-scoped Google Cloud usage.

```env
GOOGLE_CLOUD_PROJECT=your-project-id
GOOGLE_CLOUD_LOCATION=us-central1
GOOGLE_GENAI_USE_VERTEXAI=TRUE
```

Then authenticate application default credentials:

```bash
gcloud auth application-default login
```

## 3. Run ADK Web

```bash
cargo run -p gemini-adk-web-rs
```

Open:

```text
http://localhost:25125
```

The landing page lists every bundled app. Open a voice app such as `voice-chat`, `call-screening`, or `debt-collection`, allow microphone access, and use the DevTools panel on the right to inspect state, phases, metrics, tools, traces, and cookbook guidance.

## 4. Run Cookbook Examples

The cookbook examples are plain Rust binaries:

```bash
cargo run -p example-cookbook --example 01_simple_agent
cargo run -p example-cookbook --example 17_evaluation_suite
cargo run -p example-cookbook --example 29_live_voice
```

The learning path is:

| Tier | Examples | Focus |
|------|----------|-------|
| Crawl | `01`-`10` | Single-agent foundations, tools, callbacks, state, guards |
| Walk | `11`-`20` | Routing, fallback, middleware, context, evaluation, artifacts |
| Run | `21`-`30` | Production compositions, testing, voice, dispatch, telemetry |

## 5. Verify the Workspace

Useful checks while developing:

```bash
cargo test -p gemini-adk-rs
cargo test -p gemini-adk-fluent-rs
cargo test -p gemini-adk-web-rs
```

For frontend-only changes:

```bash
node --check apps/gemini-adk-web-rs/static/js/app.js
node --check apps/gemini-adk-web-rs/static/js/devtools.js
```

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Web UI does not open | Confirm the server printed `http://localhost:25125` and no firewall is blocking the port. |
| Microphone is silent | Browser microphone permission must be allowed; Linux also needs `libasound2-dev`. |
| Live API auth fails | Confirm `.env` is in the repository root and contains either `GOOGLE_API_KEY` or Vertex AI settings. |
| Vertex AI rejects setup fields | The SDK strips Google AI-only fields automatically; confirm `GOOGLE_GENAI_USE_VERTEXAI=TRUE`. |
| Linker fails with `ld terminated` | Retry after closing other large builds; this is usually local linker memory pressure, not Rust code. |

## What to Inspect in DevTools

| Panel | Use it for |
|-------|------------|
| Timeline | Event ordering, interruptions, tool calls, turn boundaries |
| Events | Raw JSON payloads for exact debugging |
| State | Canonical state, raw extractor output, `state_meta:*` provenance |
| Phases | Current phase, requirements, transitions, state promotion decisions |
| Metrics | Latency, tokens, interruptions, playback buffer health |
| Traces | Span timing across model, tools, and runtime work |
| Cookbook | Source path, run command, and app-specific inspection checklist |
