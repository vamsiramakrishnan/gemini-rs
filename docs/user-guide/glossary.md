# Glossary

Core vocabulary across the three layers. Types link to where they live; see the
[API reference](../api-reference.md) for full signatures.

## Layers

- **L0 — `gemini-genai-rs`** — the wire protocol: WebSocket transport, the
  session handle, `SessionEvent`/`SessionCommand`, auth, and the raw message
  types (`Content`, `Part`, `Role`, `FunctionCall`, `FunctionResponse`).
- **L1 — `gemini-adk-rs`** — the agent runtime: the three-lane Live processor,
  `State`, tools and dispatch, phases, extraction, watchers, governed flow, and
  the text-agent runtime.
- **L2 — `gemini-adk-fluent-rs`** — the fluent DX: copy-on-write builders, the
  `Live` session builder, the `S·C·T·P·M·A·E·G` operator algebra, and the
  composition combinators. Its `prelude` is the **kernel** (see
  [migration guide](./migration.md) for the kernel/submodule map).

## Sessions & the processor

- **Live session** — a full-duplex streaming connection to the Gemini Multimodal
  Live API. Built with `Live::builder()` (L2) or `LiveSessionBuilder` (L1).
- **`LiveHandle`** — the runtime handle returned by `connect_*`: `send_audio`,
  `send_text`, `state()`, `telemetry()`, `extracted()`, `disconnect()`.
- **Three-lane processor** — the runtime splits incoming events into a **fast
  lane** (sync, <1 ms callbacks), a **control lane** (async, may block), and a
  **telemetry lane** (metrics, off the hot path).
- **Fast lane / control lane** — see
  [Live Callbacks](./live-callbacks.md). Fast-lane callbacks must not allocate,
  lock, or await; control-lane callbacks may.
- **`SessionPlan` / `build_runtime` / `spawn_lanes`** — the staged, internal
  startup pipeline behind `connect()`: derive the resolved plan (pure), wire the
  runtime, then spawn the lanes.
- **Delivery policy** — per-event-class backpressure for the fast lane
  (`Lossless` by default; lossy variants drop frames instead of stalling the
  router under a slow consumer).

## State

- **`State`** — concurrent, typed key-value store with automatic serde
  (de)serialization shared across the session.
- **Prefix scopes** — `app:`, `user:`, `session:`, `turn:` (cleared each turn),
  `bg:`, `temp:`, and the read-only `derived:` scope. `state.get("k")` falls back
  to `derived:k`.
- **`StateKey<T>`** — a compile-time typed key constant that prevents typos.
- **Delta tracking** — transactional `with_delta_tracking()` →
  `commit()`/`rollback()`.

## Tools

- **`SimpleTool`** — a tool defined from a name, description, JSON Schema, and an
  async closure over `serde_json::Value`.
- **`TypedTool`** — a tool whose schema is auto-derived from a
  `schemars::JsonSchema` struct (prevents schema drift).
- **`#[tool]`** — attribute macro that turns an `async fn` into a registrable tool.
- **`ToolDispatcher`** — routes incoming `FunctionCall`s to registered tools.
- **`Toolset`** — a dynamic collection of tools (e.g. MCP, OpenAPI); resolved at
  connect time.
- **Background tool / scheduling** — `ToolExecutionMode::Background` sets
  `NonBlocking` on the wire and delivers results with a `FunctionResponseScheduling`
  mode (`Interrupt`/`WhenIdle`/`Silent`). Google AI only.

## Composition

- **Operator algebra (`S·C·T·P·M·A·E·G`)** — composition namespaces for state
  transforms, context, tools, prompts, middleware, artifacts, evaluation, and
  guards. See [the algebra chapter](./composition.md).
- **Combinators** — `>>` sequential, `|` parallel/fan-out, `*` loop, `/`
  fallback, plus `until(pred)`. The underlying node type is `Composable`.
- **Patterns** — pre-built shapes: `review_loop`, `fan_out_merge`, `supervised`.
- **Contract validation** — `check_contracts` / `ContractViolation` catch
  reads/writes wiring bugs at build time.

## Conversation control

- **Phase / `PhaseMachine`** — declarative conversation states with instructions,
  transitions, guards, and on-enter actions.
- **Steering mode** — how phase instructions reach the model:
  `InstructionUpdate`, `ContextInjection`, or `Hybrid`.
- **Context delivery** — when model-role context turns hit the wire: `Immediate`
  or `Deferred`.
- **Watcher / temporal pattern** — react to state changes (`.watch(...)`) or
  sustained/recurring conditions (`when_sustained`, `when_turns`).
- **Conversation repair** — nudge/escalate when required information isn't
  gathered (`RepairConfig`, `NeedsFulfillment`).
- **Governed flow** — a declarative conversation/tool DAG (`Flow`) enforced live
  via `Live::govern(flow)`: gates tools, projects postures, drives repair.

## Extraction & telemetry

- **`TurnExtractor` / `LlmExtractor`** — out-of-band extraction of structured
  state from the conversation; triggered by `ExtractionTrigger`.
- **Generation-complete vs turn-complete** — generation-complete fires before
  interruption truncation (captures full intent); turn-complete fires at the
  turn boundary after truncation.
- **`SessionTelemetry` / `SessionSignals`** — auto-collected metrics and derived
  timing signals.

## Persistence

- **`SessionPersistence` / `SessionSnapshot`** — snapshot and resume a session
  across process restarts. Built-in backends: `FsPersistence`, `MemoryPersistence`.
