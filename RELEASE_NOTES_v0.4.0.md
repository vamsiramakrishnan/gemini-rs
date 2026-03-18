# Release v0.4.0

> **gemini-rs** — A Rust SDK for the Gemini Multimodal Live API

This release brings the workspace in line with the upstream ADK project structure, adds a full evaluation framework, new server apps, a CLI tool, and production-readiness features like session persistence backends and observability.

---

## Highlights

### Workspace Restructure

The project layout now mirrors upstream ADK conventions:

```
examples/          standalone examples
apps/adk-web/      interactive web UI (formerly cookbooks/ui/)
                    apps/adk-api-server (new — headless REST API)
                    apps/adk-server-core (new — shared server library)
                    tools/adk-cli/      (new — CLI tool)
```

### `adk-cli` — Full CLI Tool

A new `adk` binary with six subcommands for the complete agent development lifecycle:

| Command | Description |
|---------|-------------|
| `adk create` | Scaffold a new agent project from templates |
| `adk run` | Run an agent from a manifest file |
| `adk web` | Launch the interactive web UI |
| `adk eval` | Run evaluation suites against agents |
| `adk deploy` | Deploy agents to Cloud Run or Vertex AI |
| `adk api_server` | Start a headless REST API server |

### Evaluation Framework

Five evaluators for comprehensive agent testing, all in `rs-adk`:

- **`HallucinationEvaluator`** — Detect hallucinated content by comparing agent output against ground truth
- **`RubricEvaluator`** — Score responses against structured grading rubrics
- **`SafetyEvaluator`** — Check output for safety policy violations
- **`UserSimulatorEvaluator`** — Simulate multi-turn user interactions and score end-to-end behavior
- **`TrajectoryMatchType`** — Compare tool call sequences with exact, in-order, or any-order matching

Plus `EvalsetParser` for TOML-based eval set configuration and `TestConfig` for test case execution.

### Server Apps

- **`adk-api-server`** — Standalone REST API server for headless agent deployments, suitable for production use behind a load balancer
- **`adk-server-core`** — Shared library extracting agent loading, REST handlers, and session management so that `adk-web`, `adk-api-server`, and `adk-cli` share the same backend logic

### Production Features

- **Session backends**: Postgres and Vertex AI session persistence (`rs-adk`)
- **Agent configuration**: `AgentConfig` with full serde serialization/deserialization
- **Middleware**: Trait-based middleware pipeline for request/response interception
- **Telemetry**: Structured logging, Prometheus metrics, OpenTelemetry span management, one-call setup utilities
- **Config-driven construction**: `AgentBuilder::from_config()` in `adk-rs-fluent`

---

## Crates

| Crate | Version | crates.io |
|-------|---------|-----------|
| `rs-genai` | 0.4.0 | [crates.io/crates/rs-genai](https://crates.io/crates/rs-genai) |
| `rs-adk` | 0.4.0 | [crates.io/crates/rs-adk](https://crates.io/crates/rs-adk) |
| `adk-rs-fluent` | 0.4.0 | [crates.io/crates/adk-rs-fluent](https://crates.io/crates/adk-rs-fluent) |
| `adk-cli` | 0.4.0 | [crates.io/crates/adk-cli](https://crates.io/crates/adk-cli) |

## Install

```bash
# Library (add to Cargo.toml)
cargo add adk-rs-fluent    # Full fluent DX (recommended)
cargo add rs-adk            # Runtime only
cargo add rs-genai           # Wire protocol only

# CLI
cargo install adk-cli
```

## Upgrade Guide

Update your `Cargo.toml` dependencies from `0.1.0` to `0.4.0`. No breaking API changes — this is an additive release. The workspace layout uses `examples/` for standalone examples and `apps/adk-web/` for the interactive web UI.

## What's Changed (commits)

- `376d975` feat: add missing ADK capabilities and restructure workspace to upstream convention
- `7f6ed8b` feat: consolidate server code into adk-server-core shared library
- `0f868d3` docs: add comprehensive READMEs for adk-web, adk-api-server, and adk-cli
- `7428cdb` chore: add adk-server-core to workspace members
- `2dedecb` chore: bump all crate versions to 0.4.0
- `dec514d` docs: add v0.4.0 changelog and update release workflow for adk-cli
- `af54e14` fix: resolve clippy errors — derivable_impls and print_literal
- `5ae89e8` fix: resolve CI errors — dead code warnings and stale doc link
- `b7f7f9a` style: apply cargo fmt across workspace

## Full Changelog

See [CHANGELOG.md](./CHANGELOG.md) for the complete changelog.
