# RFC: Typed turn pipeline & unified tool lifecycle

Status: proposed · 2026-06-07 · addresses code-review redline #4 and #7

## Problem

Two functions own the live control plane as procedural conveyor belts:

- `control_plane/lifecycle.rs::handle_turn_complete` runs ~20 sequential steps
  (reset turn state → finalize transcript → extractors → derived → phase eval →
  flow governance → repair → steering → deliver context → persist …) in one body.
- `control_plane/tool_handler.rs` owns phase filtering, callback override, flow
  admission, background ack, middleware, transcript mutation, event emission, tool
  send, background spawn, tracker registration, cancel cleanup, after-tool
  extractors — in one body.

Two concrete problems fall out:

1. **No execution grammar.** You can't answer "which stage may mutate state / send
   to the model / spawn / is recoverable / aborts the turn" without reading local
   control flow. Sequencing scars (double-send, frame bursts, deferred context)
   live as comments, not contracts.
2. **Tool-lifecycle fracture (#7).** Inline tools update `FlowMonitor` directly;
   background tools can't reach the synchronous monitor from their spawned task, so
   they're gated *indirectly* on delivered result-state. Two code paths for one
   invariant — "a tool completion advances the governed flow exactly once, iff the
   tool actually completed" — which is exactly the kind of thing that should be
   centralized.

## Design

### A. `TurnPipeline` — typed stages with explicit capabilities

```rust
/// What a stage is allowed to do — declared, not discovered.
struct StageCaps {
    mutates_state: bool,
    may_send_model: bool,   // instructions/context to the model
    may_spawn: bool,
    recoverable: bool,      // a stage error logs + continues vs aborts the turn
}

#[async_trait]
trait TurnStage: Send + Sync {
    fn name(&self) -> &'static str;
    fn caps(&self) -> StageCaps;
    async fn run(&self, ctx: &mut TurnCtx) -> StageOutcome; // Continue | AbortTurn
}

struct TurnPipeline { stages: Vec<Box<dyn TurnStage>> }
```

`handle_turn_complete` becomes `pipeline.run(&mut ctx).await`. The existing steps
become stages: `ClearTurnState`, `FinalizeTranscript`, `RunExtractors`,
`RecomputeDerived`, `EvaluatePhase`, `EvaluateFlow`, `EvaluateRepair`,
`ComposeSteering`, `DeliverContext`, `FireWatchers`, `PersistSnapshot`, …

Each stage's `caps()` is the contract: e.g. `DeliverContext.may_send_model = true`
is the *only* stage allowed to send; `RunExtractors.recoverable = true` (an
extractor failure logs and continues); `PersistSnapshot.recoverable = true`. A
debug `TurnTrace` records `(stage, outcome, elapsed)` — feeds the devtools.

**Migration is behavior-preserving:** lift each numbered block verbatim into a
`TurnStage::run`, keep the order, keep the comments-as-rationale. No new behavior;
unit tests stay the assertion of record. Stages get extracted one at a time.

### B. `ToolLifecycle` — one model for inline and background

```rust
enum ToolPhase { Admitted, Acknowledged, Started, CompletedOk, CompletedErr, Cancelled }

/// The single place a completed tool advances the governed flow — exactly once.
struct ToolLifecycle { /* tool, call_id, phase, observed_by_flow: bool */ }
```

Both inline and background tools publish `ToolPhase` transitions to one sink. The
flow-advance ("observe completion") happens in **one** place keyed by `call_id`
with an idempotency guard (`observed_by_flow`), so:

- inline completion and background-delivered completion go through the same gate;
- the flow advances once and only once, iff `CompletedOk`/`CompletedErr` was seen;
- background tools no longer need the "gate indirectly on delivered state" hack —
  the delivered result *is* a `CompletedOk` transition.

This makes the review's invariant a centralized, testable property instead of two
divergent conventions.

## Staged plan (each step green, unit-tests as the contract)

1. Introduce `TurnCtx`, `TurnStage`, `StageCaps`, `TurnPipeline` (no behavior).
2. Extract `handle_turn_complete` steps into stages **one at a time**, asserting
   the existing tests pass after each — the safe, boring refactor.
3. Introduce `ToolLifecycle` + a single `observe_completion(call_id)` gate; route
   inline tools through it (behavior-preserving).
4. Route background-tool completion through the same gate; delete the
   indirect-on-state hack (#7 closed).
5. Emit `TurnTrace`/`ToolPhase` events → wire into `adk flow` devtools (replay/why).

## Risk & why RFC-first

This is the live hot path with **no in-repo integration test** (it needs a
connected Gemini Live socket). The only safety net is the existing unit tests, so
the migration must be *strictly behavior-preserving* and *incremental* — lift-and-
shift per stage, never "rewrite the ceremony." That discipline is the whole point
of doing it as staged commits behind this RFC rather than one big change.

## Non-goals

- No new turn behavior, ordering change, or scheduling policy in the migration.
- Lane backpressure policy (lossy/coalesce/lossless) is a separate concern
  (tracked under the reactive-substrate milestone), not this RFC.
