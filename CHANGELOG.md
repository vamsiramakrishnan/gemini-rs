# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-03-18

### Added
- **Namespace parity** (~70 new methods across all composition namespaces):
  - Guards (`G::`): `rate_limit`, `toxicity`, `grounded`, `hallucination`, `llm_judge`
  - Tools (`T::`): `agent`, `mcp`, `a2a`, `mock`, `openapi`, `search`, `schema`, `transform`
  - Middleware (`M::`): `fallback_model`, `cache`, `dedup`, `metrics`, agent/model hooks
  - Prompt (`P::`): `reorder`, `only`, `without`, `compress`, `adapt`, `scaffolded`, `versioned`
  - Context (`C::`): `summarize`, `relevant`, `extract`, `distill`, `priority`, `fit`, `project`
  - State (`S::`): `log`, `unflatten`, `zip`, `group_by`, `history`, `validate`, `branch`
  - Eval (`E::`): `from_file`, `persona`
  - Artifacts (`A::`): `publish`, `save`, `load`, `list`, `delete`, `version`, JSON/text ops
- **30 cookbook examples** — progressive Crawl (01–10), Walk (11–20), Run (21–30) learning path:
  - Crawl: `simple_agent`, `agent_with_tools`, `callbacks`, `sequential_pipeline`, `parallel_fanout`, `loop_agent`, `state_transforms`, `prompt_composition`, `tool_composition`, `guards`
  - Walk: `route_branching`, `fallback_chain`, `review_loop`, `map_over`, `middleware_stack`, `context_engineering`, `evaluation_suite`, `artifacts`, `agent_tool`, `supervised`
  - Run: `full_algebra`, `contract_testing`, `deep_research`, `customer_support`, `code_review`, `dispatch_join`, `race_timeout`, `a2a_remote`, `live_voice`, `production_pipeline`
- **Web UI redesign**: Design system (80+ CSS tokens, Inter + JetBrains Mono), dark/light mode, animated landing page, architecture diagram, cookbook browser, operator algebra showcase, glassmorphism navigation
- **Cookbook browser panel** in DevTools UI
- **`gemini-adk-cli-rs` manifest fields**: `description`, `license`, `keywords`, `categories`, `repository` for crates.io compliance

### Changed
- All crate versions bumped from `0.4.0` → `0.5.0`
- Internal dependency versions updated (`gemini-genai-rs` and `gemini-adk-rs` constraints in downstream crates)
- Cookbook-to-example renaming across docs, configs, and source files
- Release workflow: publish steps now check crates.io API before uploading, skip if version already exists

### Fixed
- `cargo fmt` violations across cookbook examples and compose modules
- `gemini-adk-cli-rs` crates.io manifest verification failure (missing required fields)

## [0.4.0] - 2026-03-18

### Added
- **Workspace restructure**: Organized examples under `examples/` and interactive web UI under `apps/gemini-adk-web-rs/` to match upstream ADK convention
- **`gemini-adk-api-rs`**: Standalone REST API server for headless agent deployments
- **`gemini-adk-server-rs`**: Shared server library (agent loading, REST handlers, session management) used by both `gemini-adk-web-rs` and `gemini-adk-api-rs`
- **`gemini-adk-cli-rs`**: Full CLI tool with `create`, `run`, `web`, `eval`, `deploy`, and `api_server` subcommands
- **Evaluation framework** (`gemini-adk-rs`):
  - `EvalsetParser` — TOML-based eval set configuration
  - `HallucinationEvaluator` — detect hallucinated content in agent output
  - `RubricEvaluator` — score agent responses against grading rubrics
  - `SafetyEvaluator` — check agent output for safety policy violations
  - `UserSimulatorEvaluator` — simulate multi-turn user interactions
  - `TrajectoryMatchType` — exact, in-order, and any-order tool call sequence matching
  - `TestConfig` — test case configuration and execution
- **Session backends** (`gemini-adk-rs`): Postgres and Vertex AI session persistence
- **Agent configuration** (`gemini-adk-rs`): `AgentConfig` with full serialization support
- **Middleware module** (`gemini-adk-rs`): Middleware trait and composition pipeline
- **Telemetry** (`gemini-adk-rs`): Structured logging, metrics collection, span management, and setup utilities
- **Context module** (`gemini-adk-rs`): `InvocationContext` for agent execution context
- **Run configuration** (`gemini-adk-rs`): `RunConfig` for agent run parameters
- **Config-driven construction** (`gemini-adk-fluent-rs`): `AgentBuilder::from_config()` and `AgentBuilder::config()`
- Documentation: Comprehensive READMEs for `gemini-adk-web-rs`, `gemini-adk-api-rs`, and `gemini-adk-cli-rs`
- DevTools UI: Artifact panel, eval panel, event inspector panel, and trace panel

### Changed
- Workspace layout: standalone examples in `examples/`, web UI in `apps/gemini-adk-web-rs/`
- `gemini-adk-web-rs` now depends on `gemini-adk-server-rs` instead of inlining server logic
- All crate versions bumped from `0.1.0` → `0.4.0`

### Fixed
- `clippy::derivable_impls` on `TrajectoryMatchType` — replaced manual impl with `#[derive(Default)]`
- `clippy::print_literal` in `gemini-adk-cli-rs` eval output formatting
- Dead code warnings across workspace
- `cargo fmt` violations

## [0.1.0] - 2026-03-03

### Added
- Initial release of three-crate workspace
- **gemini-genai-rs** (L0): Wire protocol, WebSocket transport, `Codec`/`Transport`/`AuthProvider` traits, `SessionWriter`/`SessionReader`, structured errors, `Role` enum, `Content`/`Part` builders
- **gemini-adk-rs** (L1): Agent runtime with three-lane processor (fast/control/telemetry), `State` with prefix scoping (`session:`, `derived:`, `turn:`, `app:`, `user:`), `PhaseMachine` for conversation flow control, `ToolDispatcher` with `SimpleTool`/`TypedTool`, `ComputedRegistry` for derived state, `WatcherRegistry` for state change watchers, `TemporalRegistry` for temporal pattern detection, `SessionSignals` with atomic counters, `SessionTelemetry`, `BackgroundToolTracker`
- **gemini-adk-fluent-rs** (L2): Fluent builder API, S-C-T-P-M-A operator algebra for agent composition, `Middleware` trait and `MiddlewareChain`, pre-built patterns and contract validation
- ADK Web UI framework: multi-app Axum WebSocket tester with devtools panel
- Standalone examples: `text-chat`, `voice-chat`, `tool-calling`, `transcription`
- Agents examples: `weather-agent` and `research-pipeline` demos
- Support for both Google AI (API key) and Vertex AI (OAuth token) authentication
- Voice Activity Detection (VAD) with configurable settings
- Audio buffer management for bidirectional streaming
- `ConnectBuilder` for ergonomic session construction with generic `Transport` and `Codec`
