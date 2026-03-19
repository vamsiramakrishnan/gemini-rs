# gemini-adk-server-rs

Shared server core powering all ADK server surfaces (`gemini-adk-web-rs`, `gemini-adk-api-rs`, `gemini-adk-cli-rs api`).

## What it provides

- **`AgentRegistry`** — Unified agent discovery from `agent.toml` / `agent.json` / programmatic registration
- **REST API router** — All upstream ADK API endpoints (`/apps`, `/run`, `/sessions`, etc.)
- **`SessionStore` trait** — Pluggable session persistence (in-memory default, swap for DB-backed)
- **Shared types** — Request/response types used across all server surfaces

## Usage

This crate is a library — it's never run directly. Import it from your server binary:

```rust
use gemini_adk_server_rs::{AgentRegistry, ServerState, build_api_router};

let mut registry = AgentRegistry::new();
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
