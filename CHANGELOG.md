# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking)

- **`State` writes are now fallible.** `State::set`, `set_committed`, `set_key`,
  `modify`, and `PrefixedState::set` return `Result<_, StateError>` instead of
  panicking via `expect` on non-serializable input — a public SDK write no longer
  aborts the host process. Call sites must handle the `Result`.
- **`flow::Mode` renamed to `flow::Enforcement`** (`Enforce`/`Observe`) to remove
  the collision with `orchestration::Mode` (`Call`/`Dispatch`/`Background`). A
  deprecated `flow::Mode` alias is kept for one release; the `FlowMode` prelude
  alias now points at `Enforcement`.

### Fixed

- **`State::modify` is now atomic.** It performs the read-modify-write under a
  per-key map lock (`DashMap::entry`) instead of a racy `get`→`f`→`set`, so
  concurrent increments no longer lose updates.
- **Delta rollback is now correct.** Delta tracking uses tombstones
  (`DeltaOp::Put`/`Delete`): `remove()` and `clear_prefix()` no longer mutate the
  committed store, so `rollback()` reliably restores the base state after removals
  and prefix clears, and `commit()` applies removals.
- **`Flow` `Before` constraint is now enforced.** `before(a, b)` gates step
  eligibility (`b` cannot start until `a` is done); previously it was validated but
  never consulted at runtime.
- **Custom guards inside `Guard::all`/`any` are no longer silently dropped.** A
  nested `Guard::custom` is preserved as a runtime closure (making the combinator
  non-serializable) instead of being lowered to `Pred::Always`, which had silently
  deleted composed safety guards.
- **Metadata truth.** Crate READMEs and the main README license section corrected
  to MIT (matching `LICENSE`); install snippets bumped to `0.7`; documented MSRV
  aligned with CI (`rust-version = "1.93"`, README badge `1.93+`).

### Added

- `State` property test (rollback always restores base) and regression tests for
  atomic `modify`, rollback-after-remove, and rollback-after-clear-prefix; `Flow`
  regression tests for `Before` enforcement and custom-guard preservation.
- **`ROADMAP.md`** — milestone-based plan for post-0.7.0 work, reframed around
  hardening the primitives into contracts.
- **Eval REST endpoint wired to `gemini_adk_rs::evaluation`** — `POST /eval/run` now loads an `EvalSet` (inline JSON or file path), maps criteria → deterministic evaluators (`response_match`/`exact_match`/`tool_trajectory`[`_any_order`], with optional `name=threshold`), scores each case (pre-recorded actuals, or live agent runs when actuals are absent), and aggregates a real `EvalResultSummary`. Results are stored on `ServerState` and served from `GET /eval/results`.

## [0.7.0] - 2026-05-31

### Added

- **Governed Agents** — three lenses over a shared State+result core (`{name}:result` + `state_meta:` provenance), composing multiplicatively:
  - **Flow** — governed conversation/tool DAG (`Flow`/`Step`/`Guard`, closed serializable predicate atoms). `FlowMonitor` keeps a token-replay `Marking`, gates tool calls (`once`/`never…until`/allow-deny), projects active-step postures as steering, surfaces unmet `require`s as repair, and exposes `verdict`/`violations`/`to_mermaid()`. Wired into `Live` via `.govern()` / `.observe()`.
  - **`Effect::ground(template)`** — serializable, `State`-interpolated fact line (`{key}`, `{key?yes:no}`) projected while a step is active (anti-hallucination), via `render_ground`.
  - **Flow `on_enter(run(agent, mode))`** — a step runs an agent on activation (fire-once); result → `{step}:result`, completing a downstream step via `Guard::resolved`.
  - **Extract** — deterministic recognizers (`integer`/`integer_near`/`money`/`regex`/`one_of`/`fuzzy`/`yes_no`/`datetime`) fill `State` on the CPU; **`#[derive(Extract)]`** builds a record from struct fields.
  - **Async resolver field sources** — `Extract::field_resolve(name, args, ttl, fetch)` binds args from `State`, caches by `(field, args)` for a TTL, runs concurrently with recognizers; `TurnExtractor::extract_with_state` threads `State` through the pipeline.
  - **`Extract::on_complete(agent, mode)`** — dispatch a downstream agent when a record lands fields.
  - **Orchestration** — `Mode` (`Call`/`Dispatch`/`Background`) + **`Resolver`** (`agent`/`fetch`/`llm`): a named async value source whose inputs come from `State`; `resolve`/`dispatch`. **Provenance** recorded under `state_meta:{name}:result` and readable via `provenance(state, key)`.
- **Cookbook** re-centered on the higher-order capabilities; capstones `39_booking` and `40_screening` combine all three lenses (run with no credentials).
- **mdbook** — new *Agent Orchestration* chapter; updated *Extraction* and *Governed Flows* chapters; RFCs in `docs/plans/`.

## [0.6.0] - 2026-03-19

### Bug Fixes

- fix: drop --all-targets from release validation (avoids openssl-sys bench dep)
- fix: release script publish dry-run tolerance for first-time crate publishes
- fix: release script _section crash under set -e with empty changelog sections
### Refactors

- refactor: rename crates with -rs suffix for crates.io namespace clarity
### Style

- style: cargo fmt --all

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
