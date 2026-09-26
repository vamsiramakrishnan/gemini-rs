# gemini-adk-server-rs

Shared server core powering all ADK server surfaces (`gemini-adk-web-rs`, `gemini-adk-api-rs`, `gemini-adk-cli-rs api`).

## What it provides

- **`ServerAgentRegistry`** — Unified agent discovery from `agent.toml` / `agent.json` / programmatic registration
- **REST API router** — All upstream ADK API endpoints (`/apps`, `/run`, `/sessions`, etc.)
- **`SessionStore` trait** — Pluggable session persistence (in-memory default, swap for DB-backed)
- **Shared types** — Request/response types used across all server surfaces

## Usage

The crate also ships `adk-runtime`, the production server for session-spec
bundles: it serves bundles from a bundle store over a WebSocket and Twilio
Media Streams, with token auth, session caps, probes and graceful drain.
See [Deploying the runtime](../../docs/user-guide/deploy.md).

```bash
ADK_BUNDLES=./bundles ADK_SERVE=booking:prod ADK_RUNTIME_TOKENS=dev-token \
  cargo run -p gemini-adk-server-rs --bin adk-runtime
```

As a library, import it from your server binary:

```rust
use gemini_adk_server_rs::{ServerAgentRegistry, ServerState, build_api_router};

let mut registry = ServerAgentRegistry::new();
registry.discover(&agent_dir);

let state = ServerState::new(registry);
let app = build_api_router(state);
```

## Architecture

```
gemini-adk-cli-rs (web/api) ──┐
gemini-adk-api-rs ─────┤──► gemini-adk-server-rs ──► gemini-adk-rs (L1) ──► gemini-genai-rs (L0)
gemini-adk-web-rs ────────────┘
```
