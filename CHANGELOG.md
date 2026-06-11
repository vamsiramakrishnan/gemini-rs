# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Turn lifecycle decomposed into named, tested stages (#4).** The live hot-path
  `handle_turn_complete` god-function was lifted, one behavior-preserving block at
  a time, into named async stage helpers (`run_turn_extractors`,
  `evaluate_phase_transition`, `project_tool_advisory`, `evaluate_repair`,
  `project_steering_context`, `govern_flow`, `deliver_instruction_and_context`). A
  deterministic harness drives the real `handle_turn_complete` through a recording
  `SessionWriter`, turning the documented ordering "scars" (single-send + dedup,
  batched/deferred context, turn reset) and each stage's effect into asserted
  invariants. No behavior change. See
  `docs/plans/2026-06-07-turn-tool-pipeline-rfc.md`.
- **Background tools advance the governed flow (#7).** Tool completions now pass
  through a single `ToolGate::observe_completion(call_id, …)` — idempotent per
  `call_id` — for both inline and background tools. Background tools (which run
  detached and can't reach the synchronous `FlowMonitor`) post a
  `ControlEvent::ToolCompleted` back to the control lane, which routes them through
  the same gate. This closes the prior fracture where background tools could only
  be gated indirectly on delivered state: `done(called_ok(..))` now works for
  background tools too. A `before_tool` veto posts no completion, so vetoed tools
  never advance the flow — matching the inline path.
- **CI: feature-boundary checks.** Added a job that builds the workspace with
  `--no-default-features` and `--all-features` (both verified green), so the
  feature-heavy SDK can't regress at the extremes. Also: `await_holding_lock` is
  now enforced (removed from the workspace lint allow-list).
- **CI/release ratchet.** New `cargo hack check --each-feature` job (every
  feature of the published crates compiles in isolation), a `cargo deny` job
  (RustSec advisories, permissive-license allow-list, source whitelist — config
  in `deny.toml`), `cargo semver-checks` in the release validate job (declared
  bump must cover the real API delta), and crates are now published **with**
  tarball verification — the `--no-verify` escape hatch is gone (dependencies
  are already live on crates.io when each crate publishes).
- **Proc-macro hygiene.** The `#[tool]` macro now routes its generated code
  through `gemini_adk_rs::__macros` (re-exporting `serde`/`schemars`/`async_trait`/
  `serde_json`) and sets `#[serde(crate = ..)]`, so downstream crates no longer
  need those upstream crates as direct dependencies under those exact names.

### Changed (breaking)

- **Feature diet: slim defaults, selectable TLS, targeted tokio.**
  `gemini-genai-rs` default features are now `["live", "tls-native"]` — the ML
  VAD model (`vad-wavekat`) and the tracing *subscriber* are no longer pulled by
  default. The TLS backend is selectable (`tls-native` default, `tls-rustls`
  opt-in; `reqwest` follows the same choice), and all three published crates
  depend on targeted `tokio` features instead of `tokio/full` (tests keep `full`
  via dev-dependencies). The `tracing` facade is now an unconditional (tiny)
  dependency — transport spans/events always compile and are no-ops without a
  subscriber — and the new `tracing-subscriber` feature gates the fmt/EnvFilter
  machinery behind `TelemetryConfig::init`. `tracing-support` is now a
  deprecated no-op feature kept one release for manifest compatibility.
  `gemini-adk-rs` explicitly requires `gemini-genai-rs/vad` (it always used it),
  and L1/L2 grew `vad-wavekat`/`tls-rustls` passthrough features so applications
  don't need a direct lower-layer dependency to opt in.
- **`reqwest` is now optional; the REST modules are feature-gated.** The default
  `gemini-adk-rs` build no longer compiles `reqwest`. A new `http` feature pulls it,
  and the REST-backed areas now actually gate their modules (fixing "feature
  declared but not wired"): `vertex-ai-code-executor`, `vertex-ai-sessions`,
  `vertex-ai-rag` (new — RAG retrieval tool + memory service), `mcp-http` (the SSE
  transport; stdio MCP still works without it), and `gcs-artifacts` each enable
  `http`. `VertexAiCodeExecutor`, `VertexAiRag*`, and the MCP HTTP path are behind
  their features; enable the feature (or `--all-features`) to use them.
- **Reactor: dead effect nouns removed.** Dropped `EffectPolicy::dedupe_key` and
  `cancel_scope` (never set or read by any rule) and `LiveEffect::TransitionPhase`
  (never produced; executor no-op'd it) — per the "make it real or delete it"
  principle, they were deleted rather than left as aspirational fields. Concurrent
  effect failures are now **supervised**: an error surfaces as `LiveEvent::Error`
  instead of being silently discarded.
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

- **Server: real SSE streaming + debug/eval polish.** `POST /run_sse` now
  streams real execution milestones (`started`, `agent_started/completed`,
  `tool_call_started/completed/failed`, final `response`) instead of returning a
  hardcoded fake string; granularity note: `BaseLlm` has no token-level
  streaming API, so the endpoint streams real lifecycle events rather than
  fabricated token chunks. Also: `GET /debug/traces` (list recorded traces),
  HTTP 400 on malformed artifact versions (was `unwrap_or(0)`), and
  `limit`/`offset` pagination on `GET /eval/results`. Covered by integration
  tests with a mock LLM.
- **`adk flow` devtools.** A CLI command group over a serializable
  `ConversationSpec`: `adk flow inspect <spec.json>` (stages/tools/digressions/
  policies/redaction summary), `adk flow graph <spec.json>` (Mermaid diagram), and
  `adk flow simulate <spec.json> <scenario.json>` (run a model-free scenario, PASS/
  FAIL). Closes the draft → inspect → simulate authoring loop with no live API.
- **`conversation-from-script` skill.** A Claude Code skill
  (`.claude/skills/conversation-from-script/`) that drafts a serializable
  `ConversationSpec` + simulation `Scenario` tests from a call-center script/SOP —
  an authoring assistant (the model drafts; the deterministic control plane
  governs). Its example spec/scenario JSON are validated by an integration test so
  the guidance can't drift from what the compiler accepts.
- **Policy aspects.** Reusable, cross-cutting governance attached to a whole
  conversation via `Conversation::policy(..)`: `Policy::safety_handoff([intents])`
  (lowers to a `safety` digression that terminates on `intent:{name}`),
  `Policy::redact([keys])` (recorded for the runtime's logging; surfaced via
  `CompiledConversation::redacted_fields()`), and `Policy::commit(tool)
  .idempotency_key(..).compensate_with(..)` (commit governance metadata). All
  serializable and round-trip through JSON.
- **Typed graph macro.** `voice_flow! { mod booking { steps: [..]; tools: [..];
  slots: [..]; } }` generates a module of compile-time-checked `&str` name
  constants, so flow code references `booking::collect` etc. — a typo'd name is a
  build error, not a silently never-matching guard. (Full declarative DSL body is a
  follow-up; this is the name-checking core.)
- **Repair flows first-class.** A serializable per-stage `RepairPolicy`
  (`reprompt_after`/`escalate_after`/`escalate_to`) via `Conversation::repair(..)`.
  The runtime raises `repair:{stage}:reprompt` once a stage has been active too long
  without completing and `repair:{stage}:escalate` after the escalate threshold;
  when `escalate_to` is set, escalation also completes the stage and routes there
  (deterministic "give up and hand off"). Signals clear when the stage leaves.
- **Motif stdlib.** `Motif` factories for high-confidence flow fragments —
  `collect_frame::<F>` / `confirm_then_commit` / `identity_verification` /
  `disclosure` / `say` / `handoff` (→ `StageSpec`) and `faq_digression`
  (→ `OverlaySpec`) — composed via new `Conversation::add_stage` / `add_overlay`.
  Motifs lower through the validated IR (a mis-built commit motif fails `compile()`
  like a hand-written one).
- **Model-free simulation harness.** A deterministic `Sim` drives a compiled
  conversation with no live API: a fake user speaks (`sim.user(text)` runs the
  conversation's recognizers to fill slots, respecting validators), slots can be
  set directly, tools succeed on demand or after a latency (`schedule_tool`), and
  the `FlowStack` advances turn by turn. Introspect with `active`/`allowed`/
  `denied`/`slot`/`is_complete`/`explain`. A serializable `Scenario` (`SimStep`s:
  `user`/`set`/`tool_ok`/`turn`/`expect_*`) runs as a data-driven test (YAML/JSON)
  and reports the failing step. `Extract::field_state_keys()` exposes the
  field→state-key mapping for promotion.
- **Hierarchical digressions / statecharts above the DAG.** Conversations can now
  declare **overlays** — named sub-flows triggered by a guard that suspend the main
  flow, run, and resume: `Conversation::overlay(name).trigger(g).stage(..).resume(..)
  .done_overlay()`. A serializable `OverlaySpec` (round-trips through JSON) lowers to
  its own validated `CompiledFlow`. A new runtime `FlowStack` (`CompiledConversation::
  stack(mode)`) drives the main flow plus at most one active digression with
  push-on-trigger / resume-on-completion (`Resume::Previous`/`Restart`/`Terminate`);
  tool admission, postures, and `explain()` delegate to the active layer.
  `FlowMonitor::eval(guard, state)` exposes guard evaluation for triggers.
- **`Live::converse(&conversation)`** — one-liner that governs a Live session with
  a compiled conversation's flow and registers the extractors that fill its
  frames' slots (`converse_observe` for observe mode).
- **Slot validation.** A serializable `SlotValidator` (`Range`/`NonEmpty`/`Regex`/
  `OneOf`) on slots; `#[slot(min=…, max=…, non_empty)]` in the derive. A recognized
  value failing its validator is rejected (the slot stays unfilled). Extract gains
  `ExtractBuilder::validate(predicate)` to attach a post-recognition check.
- **Resolver-filled slots.** `Conversation::resolve_slot(name, args, ttl, fetch)`
  fills a slot from an async fetch/agent (bound from `State`), lowering to an
  Extract resolver field. The closure stays builder-only, so `ConversationSpec`
  remains serializable.
- **Typed frames & slots.** A `Frame` trait + `FrameSpec`/`SlotSpec`/`ConfirmPolicy`/
  `SlotRecognizer` and a `#[derive(Frame)]` macro: declare a struct with
  `#[slot(prompt=…, reprompt=…, confirm=…, state=…, pii)]` and `#[recognize(…)]`
  fields and get the slot definition (keys, prompts, confirmation policy, PII
  flags, recognizers). `FrameSpec::to_extract()` lowers recognizer-bearing slots
  to an `Extract` record. `Conversation::collect_frame::<F>()` collects a frame's
  slots in a stage (drives the `captured` completion) **and** lowers its
  extractor — `CompiledConversation::extractors()` exposes the extractors that
  fill the slots from the transcript each turn.
- **Recognizer confidence reaches state.** Deterministic extraction now records
  `state_meta:{key}` = `{source: "extraction", confidence}` when a recognizer
  matches, so `State::evidence()` surfaces real per-slot confidence.
- **Slot evidence.** `State::evidence(key) -> SlotEvidence` aggregates a slot's
  current value, provenance (`state_meta:{key}.source`), confidence, and the most
  recent journal write — the basis for principled confirmations ("I heard 6,
  right?") and stale/low-confidence repair. `StateMutationOrigin` is now serde.
- **Conversation compiler (Phase 1 MVP).** A serializable `ConversationSpec` and
  a fluent `Conversation` builder (sugar over the spec) that **compile down to a
  governed `CompiledFlow`** via `Conversation::compile() -> CompiledConversation`.
  Authors describe stages that `say`/`ground`, `collect` slots, `commit` tools
  behind confirmation, and advance via `next(to, when)`; the compiler lowers these
  to Flow steps, gates, postures, grounding, tool whitelists, and commit
  constraints. The spec round-trips through JSON (YAML/hot-reload follows for free)
  and `CompiledConversation::monitor()` yields a ready `FlowMonitor`.
- **`Flow::compile() -> Result<CompiledFlow, FlowErrors>`** — the validated flow
  IR the conversation compiler targets. On top of `validate()` it reports
  unreachable steps and effectively-unguarded commit tools (`FlowError`), and
  precomputes a `ToolPolicy` (the tool universe). `FlowMonitor::compiled` /
  `try_new` construct from it; `new` remains for in-process trusted flows.
- **`FlowMonitor::explain()` / `why_blocked()`** returning a serializable
  `FlowExplanation` (active steps, allowed/blocked tools with reasons, unmet
  requirements) — the deterministic answer to "why did the assistant ask that?".
- **Conversation-compiler RFC** (`docs/plans/2026-06-06-conversation-compiler-rfc.md`)
  — the plan to author voice behavior (slots/confirm/repair/digress/commit) and
  compile it down to Flow + Extract + Resolver + Reactor; locks
  serializable-spec-first and the one-control-structure rule.
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
