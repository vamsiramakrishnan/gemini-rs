# RFC: Hierarchical statecharts & digressions above the DAG

Status: proposed · 2026-06-07 · extends the conversation-compiler RFC
(`2026-06-06-conversation-compiler-rfc.md`)

## Problem

A `Flow` is a DAG — excellent for **governance** (ordering, tool gating,
completion), but real voice conversations are not pure DAGs. Users interrupt to
ask a side question, correct an earlier slot, cancel, repeat, ask for help, or
escalate — then expect to **resume where they were**. Modeling every such path as
explicit DAG edges causes edge explosion (every stage × every digression).

## Approach: keep Flow as the conformance graph; add a resume stack above it

The DAG stays the compiled conformance/governance IR. Above it we add a small
**hierarchical** layer:

- A **main flow** (the `ConversationSpec` we already compile).
- Named **overlays** (digressions): self-contained sub-flows triggered by a
  guard/intent that **suspend** the main flow, run to completion, then **resume**.
- A **`FlowStack`** runtime: the main flow plus at most one active overlay
  (nesting depth 1 for the MVP), with explicit push/resume.

This is the Harel-statechart move (hierarchy + history states) without inventing a
third authored control structure: an overlay is *just another compiled flow*, and
the stack composes `FlowMonitor`s. Higher layers still only emit lower-layer
constructs (the no-backdoors invariant).

## Authoring surface (serializable-first)

```rust
Conversation::new("support")
    .stage("identify")./* … */
    .stage("triage")./* … */
    .stage("resolve").terminal()
    // Digressions: each is a named sub-flow with a trigger and a resume policy.
    .overlay("faq")
        .trigger(Guard::is_true("intent:ask_policy"))
        .stage("answer").say("Answer the policy question.").terminal()
        .resume(Resume::Previous)
        .done_overlay()
    .overlay("cancel")
        .trigger(Guard::is_true("intent:cancel"))
        .stage("confirm_cancel").terminal()
        .resume(Resume::Restart)
        .done_overlay()
    .compile()?;
```

`OverlaySpec { name, trigger: Guard, stages, require, resume }` is serializable
(it embeds `StageSpec`s and a serializable `Guard`), so the whole conversation —
overlays included — round-trips through JSON/YAML.

## Resume semantics

```rust
enum Resume {
    /// Resume the main flow exactly where it was suspended (history state).
    Previous,
    /// Re-enter the main flow from its start.
    Restart,
    /// End the conversation (e.g. a cancel/handoff overlay).
    Terminate,
}
```

The main flow's `Marking` is **not advanced** while an overlay is active, so
`Resume::Previous` is automatic: popping the overlay leaves the main monitor
exactly as it was. Open question (resolve in impl): how in-progress *slots* are
treated on resume (preserved by default; a `correction` overlay explicitly
reopens the slot it targets).

## Runtime: `FlowStack`

```rust
pub struct FlowStack {
    main: FlowMonitor,
    overlays: Vec<CompiledOverlay>,        // name + trigger + monitor factory
    active: Option<ActiveOverlay>,         // at most one (MVP: depth 1)
}

impl FlowStack {
    fn on_turn(&mut self, state) {
        match &mut self.active {
            None => {
                // Enter the first overlay whose trigger holds.
                if let Some(ov) = self.triggered(state) { self.push(ov, state); }
                else { self.main.on_turn(state); }
            }
            Some(active) => {
                active.monitor.on_turn(state);
                if active.monitor.is_complete() { self.resume(state); } // pop per Resume
            }
        }
    }
    fn admits_tool(&self, tool, state) -> Result<(), String> {
        // The active overlay's gate takes precedence; else the main flow's.
        self.active.as_ref().map(|a| &a.monitor).unwrap_or(&self.main).admits_tool(tool, state)
    }
    fn explain(&self, state) -> FlowExplanation { /* active layer */ }
}
```

Tool admission, active postures/grounds, and `explain()` delegate to the **active
layer** (overlay if present, else main) — so governance and "why did it ask
that?" stay correct inside a digression.

## Lowering

`Conversation::compile()` produces, in addition to the main `CompiledFlow` +
extractors:

```rust
CompiledConversation {
    flow: CompiledFlow,                 // main
    extractors: Vec<Extract>,           // main
    overlays: Vec<CompiledOverlay>,     // each: name, trigger, CompiledFlow, extractors, resume
    spec: ConversationSpec,
}
```

`CompiledConversation::stack(mode) -> FlowStack` builds the runtime. `Live::converse`
registers the main + all overlay extractors and drives the stack.

## Scope

- **MVP (this milestone):** depth-1 overlays; `Resume::{Previous,Restart,Terminate}`;
  trigger = `Guard`; serializable `OverlaySpec`; `FlowStack` with deterministic
  unit tests (trigger → suspend → resume); `explain()`/`admits_tool` delegate to
  the active layer.
- **Later:** nested overlays (depth > 1), slot-reopen on `correction`, intent
  classification feeding `intent:*` flags, per-overlay tool scoping layered over
  the suspended step.

## Invariants carried forward

- An overlay is a *compiled flow* — no privileged runtime; same validation
  (`Flow::compile`) and fail-loud discipline.
- Deterministic + model-free testable: the stack is driven by `State`/guards, not
  the model.
