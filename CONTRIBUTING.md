# Contributing to gemini-rs

Thank you for your interest in contributing. This guide covers the codebase layout, development workflow, and conventions.

## Prerequisites

- **Rust 1.75+** (edition 2021)
- **Google Cloud credentials** for Vertex AI testing: `gcloud auth print-access-token` must work, or set `GOOGLE_ACCESS_TOKEN` in your environment
- **A `.env` file** at the repo root (see [Environment Setup](#environment-setup))

## Environment Setup

Copy the `.env` file and fill in your values:

```bash
# Required: Google Cloud project and Vertex AI location
GOOGLE_CLOUD_PROJECT=your-project-id
GOOGLE_CLOUD_LOCATION=us-central1

# Required: Enable Vertex AI backend
GOOGLE_GENAI_USE_VERTEXAI=TRUE

# Default model for Live sessions (override per-app via browser Start message)
GEMINI_MODEL=gemini-live-2.5-flash-native-audio
```

Alternatively, for Google AI (non-Vertex) usage, set `GOOGLE_GENAI_API_KEY` or `GEMINI_API_KEY` instead.

## How the Codebase Works

### Crate Architecture

The workspace follows a layered architecture where each crate has a single responsibility and depends only on the layer below it.

| Crate | Layer | Purpose | Key Directories |
|-------|-------|---------|-----------------|
| `gemini-live` | L0 (Wire) | Protocol types, WebSocket transport, auth, VAD, audio buffers | `src/protocol/`, `src/transport/`, `src/session/`, `src/vad/`, `src/buffer/` |
| `gemini-adk` | L1 (Runtime) | Agent lifecycle, tools, state, phases, extractors, live sessions | `src/live/`, `src/tool/`, `src/state.rs`, `src/agents/`, `src/session/` |
| `gemini-adk-fluent` | L2 (DX) | Fluent builder API, operator algebra, composition primitives | `src/builder.rs`, `src/compose/`, `src/live/`, `src/live_builders.rs` |

Additionally:

| Path | Purpose |
|------|---------|
| `apps/gemini-adk-web/` | Axum WebSocket tester with browser frontend and multiple demo apps |
| `examples/agents/` | Standalone agent examples |
| `examples/text-chat/` | Text-only chat example |
| `examples/voice-chat/` | Voice chat example |
| `examples/tool-calling/` | Tool calling example |
| `examples/transcription/` | Audio transcription example |
| `tools/gemini-adk-transpiler/` | Code generator that transpiles ADK-JS type definitions to Rust |

### Dependency Flow

```
gemini-adk-fluent (L2)
    |
    +---> gemini-adk (L1)
              |
              +---> gemini-live (L0)
```

Examples depend on `gemini-adk-fluent` (L2) and get the entire stack transitively.

### Hand-Written vs Generated Code

Most code in the workspace is hand-written. The exception:

| File | Status | How to Regenerate |
|------|--------|-------------------|
| `crates/gemini-adk/src/agents/generated.rs` | Auto-generated | `cargo run -p gemini-adk-transpiler -- transpile --source <path> --output crates/gemini-adk/src/agents/generated.rs` |

Do **not** edit `generated.rs` directly. Modify the transpiler in `tools/gemini-adk-transpiler/` or the source definitions instead.

### Three-Lane Processor Architecture

The live session processor (`crates/gemini-adk/src/live/processor.rs`) routes messages across three lanes:

- **Fast lane** -- Audio, text, and VAD events. Sync callbacks only, targeting <1ms latency. No locks, no allocations on the hot path.
- **Control lane** -- Tool calls, interruptions, lifecycle events, transcript accumulation, extractors, and phase transitions. Owns `TranscriptBuffer` directly (no `Arc<Mutex<>>`).
- **Telemetry lane** -- `SessionSignals` and `SessionTelemetry` on their own broadcast receiver. Atomic counters (~1ns), debounced 100ms flush.

The router (`crates/gemini-adk/src/live/mod.rs`) is a zero-work dispatcher that never touches session signals or telemetry on the hot path.

### Key Traits

| Trait | Location | Purpose |
|-------|----------|---------|
| `Transport` | `crates/gemini-live/src/transport/ws.rs` | WebSocket send/recv abstraction (`TungsteniteTransport`, `MockTransport`) |
| `Codec` | `crates/gemini-live/src/transport/codec.rs` | Message serialization (`JsonCodec`) |
| `AuthProvider` | `crates/gemini-live/src/transport/auth/` | Authentication (`GoogleAIAuth`, `VertexAIAuth`) |
| `SessionWriter` / `SessionReader` | `crates/gemini-live/src/session/mod.rs` | Session I/O abstraction |
| `Agent` | `crates/gemini-adk/src/agent.rs` | Agent trait for composition |
| `CookbookApp` | `apps/gemini-adk-web/src/app.rs` | Trait for UI demo apps |

## Getting Started

We use [just](https://github.com/casey/just) as our task runner. Install it with `cargo install just`.

```bash
git clone https://github.com/vamsiramakrishnan/gemini-rs.git
cd gemini-rs

just setup          # Build workspace
just test           # Run library tests (no credentials needed)
just docs           # Build and open documentation
just run-ui         # Run the ADK Web UI (requires .env with credentials)
just check          # Run full quality check (fmt + lint + docs + tests)
```

Or use plain cargo directly:

```bash
cargo build --workspace
cargo test --workspace --lib
cargo doc --no-deps --workspace --open
```

## Making Changes

### Bug Fixes

1. Create a branch: `git checkout -b fix/description`
2. Write a failing test that reproduces the bug
3. Fix the bug
4. Verify: `cargo test --workspace --lib`
5. Run checks: `cargo fmt --check && cargo clippy -- -D warnings`
6. Submit a PR

### New Features

1. Write a design doc in `docs/plans/YYYY-MM-DD-feature-name.md` (see existing docs for format)
2. Create a branch: `git checkout -b feat/description`
3. Implement with tests
4. Update the relevant example if the feature is user-facing
5. Verify: `cargo test --workspace --lib`
6. Submit a PR

### Adding a Web UI Demo App

1. Create a new file in `apps/gemini-adk-web/src/apps/` (e.g., `my_example.rs`)
2. Implement the `CookbookApp` trait:

```rust
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::app::{AppCategory, AppError, CookbookApp, ServerMessage, WsSender, ClientMessage};

pub struct MyExample;

#[async_trait]
impl CookbookApp for MyExample {
    fn name(&self) -> &str { "my-example" }
    fn description(&self) -> &str { "Description of what this example demonstrates" }
    fn category(&self) -> AppCategory { AppCategory::Basic }
    fn features(&self) -> Vec<String> { vec!["voice".into()] }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        // Implementation here
        Ok(())
    }
}
```

3. Add the module to `apps/gemini-adk-web/src/apps/mod.rs`
4. Register it in `apps/gemini-adk-web/src/apps/mod.rs` inside `register_all()`
5. Test with `cargo run -p gemini-adk-web`

## Code Style

- **Formatting**: `cargo fmt` -- enforced, run before every commit
- **Linting**: `cargo clippy -- -D warnings` -- all warnings are errors
- **Doc comments**: All public items require `///` doc comments. Keep them concise and technical. One-liners are preferred for simple items.
- **Documentation CI**: The `docs.yml` workflow runs `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` on every PR. Broken doc links and missing docs fail the build.
- **Error handling**: Use `thiserror` for public error types (see `crates/gemini-adk/src/error.rs`)
- **Async**: Use `tokio` as the async runtime. Use `async_trait` for async trait methods.

### Feature Flags

Feature flags are used to keep the default build lean. Key flags:

| Crate | Flag | What It Enables |
|-------|------|-----------------|
| `gemini-live` | `live` (default) | Live session WebSocket support |
| `gemini-live` | `vad` (default) | Voice activity detection |
| `gemini-live` | `generate`, `embed`, `files`, etc. | REST API modules (require `http`) |
| `gemini-live` | `all-apis` | All REST API modules |
| `gemini-adk` | `gemini-llm` | Gemini LLM integration via HTTP |
| `gemini-adk` | `database-sessions` | Persistent session storage |
| `gemini-adk` | `tracing-support` | Structured logging via `tracing` |
| `gemini-adk-fluent` | `gemini-llm` | Passthrough to `gemini-adk/gemini-llm` |

## Testing

```bash
# All library tests (fast, no credentials needed)
cargo test --workspace --lib

# Tests for a single crate
cargo test -p gemini-live --lib
cargo test -p gemini-adk --lib
cargo test -p gemini-adk-fluent --lib

# With all features enabled
cargo test -p gemini-live --all-features --lib

# Check formatting + linting + docs (the full CI check)
cargo fmt --check \
  && cargo clippy --workspace -- -D warnings \
  && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

Tests live alongside the code they test using `#[cfg(test)] mod tests` blocks. The `crates/gemini-adk/src/test_helpers.rs` module provides shared test utilities.

## Commit Message Format

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(gemini-live): add opus codec support
fix(gemini-adk): prevent double phase transition on interrupt
docs: update wire protocol examples
refactor(gemini-adk-fluent): simplify builder chain
test(gemini-adk): add extractor concurrency tests
ci: add clippy to PR checks
```

The scope should be the crate name (`gemini-live`, `gemini-adk`, `gemini-adk-fluent`) or a top-level area (`examples`, `ci`, `docs`).

## Design Documents

Significant changes should start with a design document in `docs/plans/`. Use the naming convention:

```
docs/plans/YYYY-MM-DD-short-description.md
```

Existing design docs cover the full architecture and can be referenced for context. They are not published externally but serve as internal design records.

## Pull Request Process

1. Ensure the full check suite passes:
   ```bash
   cargo fmt --check \
     && cargo clippy --workspace -- -D warnings \
     && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace \
     && cargo test --workspace --lib
   ```
2. Add or update tests for your changes
3. If the change is user-facing, update the relevant example
4. Write a clear PR description explaining **what** changed and **why**
5. Request review

## Architecture Notes for Contributors

A few things worth knowing before diving into the code:

- **Vertex AI sends Binary WebSocket frames** (not Text). The transport layer in `crates/gemini-live/src/transport/ws.rs` handles this transparently.
- **The native audio model only supports AUDIO output modality**, not TEXT. You cannot get text responses from `gemini-live-2.5-flash-native-audio`.
- **Tool definitions cannot be updated mid-session** in Live API. Only system instructions can be updated after the session is established. Phase-scoped tool filtering works by rejecting tool calls on the SDK side, not by changing the API configuration.
- **State keys use prefixes** (`session:`, `derived:`, `turn:`, `app:`, `bg:`, `user:`, `temp:`) to control scope and lifecycle. See `crates/gemini-adk/src/state.rs`.
- **`SessionWriter` is behind `Arc<dyn SessionWriter>`** so that multiple components (phases, extractors, background tools) can share a session handle without ownership conflicts.
