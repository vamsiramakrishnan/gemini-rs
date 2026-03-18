# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-03-18

### Added
- **Namespace parity**: ~70 new methods across G, T, M, P, C, S, E, A composition namespaces
  - Guards (`G::`): `rate_limit`, `toxicity`, `grounded`, `hallucination`, `llm_judge`
  - Tools (`T::`): `agent`, `mcp`, `a2a`, `mock`, `openapi`, `search`, `schema`, `transform`
  - Middleware (`M::`): `fallback_model`, `cache`, `dedup`, `metrics`, agent/model hooks
  - Prompt (`P::`): `reorder`, `only`, `without`, `compress`, `adapt`, `scaffolded`, `versioned`
  - Context (`C::`): `summarize`, `relevant`, `extract`, `distill`, `priority`, `fit`, `project`
  - State (`S::`): `log`, `unflatten`, `zip`, `group_by`, `history`, `validate`, `branch`
  - Eval (`E::`): `from_file`, `persona`
  - Artifacts (`A::`): `publish`, `save`, `load`, `list`, `delete`, `version`
- **30 cookbook examples**: Progressive Crawl (01–10), Walk (11–20), Run (21–30) learning path
- **Web UI redesign**: Design system with 80+ CSS tokens, dark/light mode, animated hero, architecture diagrams, cookbook browser, glassmorphism navigation
- Cookbook browser panel in DevTools UI

### Changed
- All crate versions bumped from `0.4.0` → `0.5.0`
- Cookbook-to-example renaming across docs, configs, and source files

### Fixed
- `cargo fmt` violations across cookbook examples and compose modules

## [0.4.0] - 2026-03-18

### Added
- **Workspace restructure**: Organized examples under `examples/` and interactive web UI under `apps/adk-web/` to match upstream ADK convention
- **`adk-api-server`**: Standalone REST API server for headless agent deployments
- **`adk-server-core`**: Shared server library (agent loading, REST handlers, session management) used by both `adk-web` and `adk-api-server`
- **`adk-cli`**: Full CLI tool with `create`, `run`, `web`, `eval`, `deploy`, and `api_server` subcommands
- **Evaluation framework** (`rs-adk`):
  - `EvalsetParser` — TOML-based eval set configuration
  - `HallucinationEvaluator` — detect hallucinated content in agent output
  - `RubricEvaluator` — score agent responses against grading rubrics
  - `SafetyEvaluator` — check agent output for safety policy violations
  - `UserSimulatorEvaluator` — simulate multi-turn user interactions
  - `TrajectoryMatchType` — exact, in-order, and any-order tool call sequence matching
  - `TestConfig` — test case configuration and execution
- **Session backends** (`rs-adk`): Postgres and Vertex AI session persistence
- **Agent configuration** (`rs-adk`): `AgentConfig` with full serialization support
- **Middleware module** (`rs-adk`): Middleware trait and composition pipeline
- **Telemetry** (`rs-adk`): Structured logging, metrics collection, span management, and setup utilities
- **Context module** (`rs-adk`): `InvocationContext` for agent execution context
- **Run configuration** (`rs-adk`): `RunConfig` for agent run parameters
- **Config-driven construction** (`adk-rs-fluent`): `AgentBuilder::from_config()` and `AgentBuilder::config()`
- Documentation: Comprehensive READMEs for `adk-web`, `adk-api-server`, and `adk-cli`
- DevTools UI: Artifact panel, eval panel, event inspector panel, and trace panel

### Changed
- Workspace layout: standalone examples in `examples/`, web UI in `apps/adk-web/`
- `adk-web` now depends on `adk-server-core` instead of inlining server logic
- All crate versions bumped from `0.1.0` → `0.4.0`

### Fixed
- `clippy::derivable_impls` on `TrajectoryMatchType` — replaced manual impl with `#[derive(Default)]`
- `clippy::print_literal` in `adk-cli` eval output formatting
- Dead code warnings across workspace
- `cargo fmt` violations

## [0.1.0] - 2026-03-03

### Added
- Initial release of three-crate workspace
- **rs-genai** (L0): Wire protocol, WebSocket transport, `Codec`/`Transport`/`AuthProvider` traits, `SessionWriter`/`SessionReader`, structured errors, `Role` enum, `Content`/`Part` builders
- **rs-adk** (L1): Agent runtime with three-lane processor (fast/control/telemetry), `State` with prefix scoping (`session:`, `derived:`, `turn:`, `app:`, `user:`), `PhaseMachine` for conversation flow control, `ToolDispatcher` with `SimpleTool`/`TypedTool`, `ComputedRegistry` for derived state, `WatcherRegistry` for state change watchers, `TemporalRegistry` for temporal pattern detection, `SessionSignals` with atomic counters, `SessionTelemetry`, `BackgroundToolTracker`
- **adk-rs-fluent** (L2): Fluent builder API, S-C-T-P-M-A operator algebra for agent composition, `Middleware` trait and `MiddlewareChain`, pre-built patterns and contract validation
- ADK Web UI framework: multi-app Axum WebSocket tester with devtools panel
- Standalone examples: `text-chat`, `voice-chat`, `tool-calling`, `transcription`
- Agents examples: `weather-agent` and `research-pipeline` demos
- Support for both Google AI (API key) and Vertex AI (OAuth token) authentication
- Voice Activity Detection (VAD) with configurable settings
- Audio buffer management for bidirectional streaming
- `ConnectBuilder` for ergonomic session construction with generic `Transport` and `Codec`
