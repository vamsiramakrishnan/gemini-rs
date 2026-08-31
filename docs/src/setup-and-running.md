# Setup and Running

Two ways in. **Path A** adds the published crates to your own project — start
here if you want to build something. **Path B** clones this repository — start
there if you want to run the examples, the Web UI, or contribute.

Either way you need a stable Rust toolchain (1.93+) and, on Linux, the TLS and
audio headers:

```bash
# Ubuntu / Debian
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev libasound2-dev build-essential

# macOS
xcode-select --install
```

## Authentication (both paths)

Pick **one** platform. The same variables serve the whole stack — Live voice
sessions and text agents both accept the `GEMINI_API_KEY` /
`GOOGLE_GENAI_API_KEY` / `GOOGLE_API_KEY` chain.

### Google AI — fastest

```bash
export GEMINI_API_KEY=your-api-key   # https://aistudio.google.com/apikey
```

### Vertex AI — project-scoped Google Cloud

```bash
export GOOGLE_GENAI_USE_VERTEXAI=true
export GOOGLE_CLOUD_PROJECT=your-project-id
export GOOGLE_CLOUD_LOCATION=us-central1
gcloud auth application-default login   # or export GOOGLE_ACCESS_TOKEN=…
```

Repo examples also read these from a `.env` at the workspace root
(`cp .env.example .env`).

You normally don't pick a model: connect resolves a default the target platform
actually serves, and `GEMINI_MODEL=…` (or `.model(…)` in code) overrides it.

## Path A — build your own project

```bash
cargo new my-agent && cd my-agent
```

```toml
[dependencies]
gemini-adk-fluent-rs = { version = "1.0", features = ["gemini-llm", "voice-io"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Two feature flags matter on day one — the crate ships `default = []`:

| Feature | Enables | Without it |
|---|---|---|
| `gemini-llm` | Text generation via `GeminiLlm` | Compiles, then errors at runtime: *"requires the 'gemini-llm' feature flag"* |
| `voice-io` | `talk()` microphone/speaker duplex | No `talk()` method on the handle |

Then copy either Quickstart program from the
[workspace README](https://github.com/vamsiramakrishnan/gemini-rs#quickstart)
into `src/main.rs` — both are complete files, compiled in CI exactly as
printed — and `cargo run`.

Writing typed tools later adds three dependencies:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "0.8"   # the 0.8 pin matters — schemars 1.x is a different trait
```

Prefer scaffolding? `cargo install gemini-adk-cli-rs` then `adk create my-agent`.

## Path B — run this repository

```bash
git clone https://github.com/vamsiramakrishnan/gemini-rs
cd gemini-rs
cp .env.example .env    # fill in credentials from the Authentication section
```

### The quickstart programs

```bash
cargo run -p example-quickstart --bin hello-text    # first token, no audio needed
cargo run -p example-quickstart --bin hello-voice   # first sound, mic + speakers
```

### The ADK Web UI and Flow Studio

```bash
cargo run -p gemini-adk-web-rs
```

Open `http://localhost:25125`. The landing page lists every bundled app — open
a voice app such as `voice-chat`, `call-screening`, or `debt-collection`, allow
microphone access, and use the DevTools panel on the right to inspect state,
phases, metrics, tools, and traces. `/flows` is the Flow Studio.

### The cookbook

Forty progressive binaries, `01-foundations` through `40-screening` — most run
offline with no credentials:

```bash
cargo run -p example-cookbook --bin 01-foundations
cargo run -p example-cookbook --bin 17-evaluation-suite
cargo run -p example-cookbook --bin 37-governed-flow
```

| Tier | Binaries | Focus |
|------|----------|-------|
| Crawl | `01`–`10` | Single-agent foundations, tools, callbacks, state, guards |
| Walk | `11`–`20` | Routing, fallback, middleware, context, evaluation, artifacts |
| Run | `21`–`40` | Production compositions, voice, tool policies, MCP, governed flows |

The full list with descriptions is [`examples/INDEX.md`](https://github.com/vamsiramakrishnan/gemini-rs/blob/main/examples/INDEX.md).

### Verify the workspace

```bash
cargo test --workspace    # ~2,500 tests, no credentials required
```

For frontend-only changes:

```bash
node --check apps/gemini-adk-web-rs/static/js/app.js
node --check apps/gemini-adk-web-rs/static/js/devtools.js
```

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Connect fails: *"not found for API version v1beta"* / setup closes without `setupComplete` | The model isn't in your platform's catalog. Leave `.model()` unset for a platform-appropriate default, or list what your key reaches: `curl "https://generativelanguage.googleapis.com/v1beta/models?key=$GEMINI_API_KEY"` and look for `bidiGenerateContent` (Live) or `generateContent` (text) under `supportedGenerationMethods`. |
| *"GeminiLlm requires the 'gemini-llm' feature flag"* | Add `features = ["gemini-llm"]` — it is off by default. |
| No `talk()` method | Add `features = ["voice-io"]`; Linux also needs `libasound2-dev`. |
| `JsonSchema` bound errors / "multiple versions of crate schemars" | Pin `schemars = "0.8"`. |
| Web UI does not open | Confirm the server printed `http://localhost:25125` and no firewall blocks the port. |
| Microphone is silent | Browser microphone permission must be allowed; Linux also needs `libasound2-dev`. |
| Live API auth fails | `.env` at the repository root (or exported vars) with `GEMINI_API_KEY` or the Vertex AI trio. |
| Vertex AI rejects setup fields | The SDK strips Google AI-only fields automatically; confirm `GOOGLE_GENAI_USE_VERTEXAI=true`. |
| Linker fails with `ld terminated` | Retry after closing other large builds; usually linker memory pressure, not Rust code. |

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
