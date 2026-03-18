# adk-server-core

Shared server core powering all ADK server surfaces (`adk-web`, `adk-api-server`, `adk-cli api`).

## What it provides

- **`AgentRegistry`** — Unified agent discovery from `agent.toml` / `agent.json` / programmatic registration
- **REST API router** — All upstream ADK API endpoints (`/apps`, `/run`, `/sessions`, etc.)
- **`SessionStore` trait** — Pluggable session persistence (in-memory default, swap for DB-backed)
- **Shared types** — Request/response types used across all server surfaces

## Usage

This crate is a library — it's never run directly. Import it from your server binary:

```rust
use adk_server_core::{AgentRegistry, ServerState, build_api_router};

let mut registry = AgentRegistry::new();
registry.discover(&agent_dir);

let state = ServerState::new(registry);
let app = build_api_router(state);
```

## Architecture

```
adk-cli (web/api) ──┐
adk-api-server ─────┤──► adk-server-core ──► rs-adk (L1) ──► rs-genai (L0)
adk-web ────────────┘
```
