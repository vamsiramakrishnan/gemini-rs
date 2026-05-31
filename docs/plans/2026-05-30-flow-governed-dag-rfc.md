# RFC: `Flow` — a governed conversation/tool DAG

Status: accepted (core landed) · Author: library team · 2026-05-30

> Refined by the [Governed Agents synthesis](./2026-05-30-governed-agents-synthesis.md):
> `Guard` is the shared predicate across Flow / Extract / watchers; steps gain
> `on_enter(run(resolver, mode))` and `done(resolved(..))` so a Flow can
> orchestrate sub-agents and complete on resolved system/agent data.

## Motivation

Authoring a non-trivial voice agent today means hand-wiring four separate
mechanisms to express one intent ("verify before refund"): a phase guard, a
state watcher, a `before_tool` check, and manual repair text. That is ceremony,
it is error-prone, and the concepts overlap. We want **one declarative primitive**
that describes a workflow — spanning *both* conversation stages and tool-call
sequences in a single DAG — and have the runtime **observe and enforce** it.

A hard requirement from the design discussion: **a closed, orthogonal vocabulary**
so concepts cannot overload or leak. One node type, one predicate type, one
execution model.

## Formal model

The session is effectively **event-sourced** (`LiveEvent` broadcast + the
state-mutation journal + the tool-call trace). Every primitive here is the same
shape: *a declarative spec + a monitor over that trace.*

- **Trace** `σ`: the ordered session event log. Closed atom set: `Tool(name, ok)`,
  `Set(key, val)`, `Turn`.
- **Step**: the only node type. `⟨id, after, gate, done, posture, allow, deny, terminal⟩`.
- **Marking** `M`: the set of *done* steps + per-tool success counts. The runtime
  position in the flow.
- Semantics (evaluated on each event):
  - `eligible(s) ≡ after(s) ⊆ M.done ∧ gate(s)(σ)`
  - `active(s) ≡ eligible(s) ∧ s∉M.done ∧ ¬done(s)(σ)`
  - **latch** (monotone): `eligible(s) ∧ done(s)(σ) ⇒ M.done ∪= {s}` (terminal steps
    latch on eligibility alone). Re-latched to a fixpoint.
  - **admissible(tool t)**: `¬once-violated ∧ ¬never-until-blocked ∧ (active allow/deny permit t)`.
  - **complete** ≡ `require(...) ⊆ M.done`.

This is **token-replay conformance** (process mining; van der Aalst) + a
**runtime-verification monitor**.

## Cemented vocabulary

### Nouns (closed)
`Flow` · `Step` · `Guard` · `Posture` · `Marking` · `Verdict`.

### Verbs (each → exactly one formal field)
- Step: `step(id)` · `after(dep)` · `gate(Guard)` · `done(Guard)` · `posture(text)`
  · `allow([tools])` · `deny([tools])` · `terminal()`
- Constraints: `once(tool)` · `before(a,b)` · `never(tool).until(Guard)` · `require([steps])`
  · `commit(tool, until)` (sugar)
- Guard atoms (serializable): `is_true` · `is_set` · `eq` · `captured` · `called_ok`
  · `done` · `all` · `any` · `not` · plus `custom(closure)` (code-only, non-serializable).
- Attach (L2, integration stage): `govern(flow).mode(Enforce | Observe)`.

### 🚫 Ban-list at the `Flow` surface
`phase`, `transition`, `watch`, `needs`, `nudge`, `steer`. These are *lowering
details*, never typed by a flow author. (`phase` ≡ an active Step's posture;
`needs` ≡ `done(captured(...))`; `watch` ≡ a reactive Step/constraint.)

## Decisions (resolved against the use cases)

1. **Set marking; exactly-once via `once`/`commit`.** A step latches done on the
   *first* `called_ok`; cardinality is a constraint, not node identity. Reads are
   repeatable; commit-tools (`charge_card`, `book_appointment`, `transfer`) get
   `once`.
2. **Gate tools hard; shape speech proactively; never block speech.** Tool denial
   is deterministic at the `before_tool` seam. Compliant speech comes from the
   active Step's `posture` injected as turn-boundary steering *before* the model
   speaks; deviation is detected (deterministic extraction) and corrected next
   turn + audited.
3. **Builder + serde data-driven `Flow`; no proc-macro.** Because the vocabulary
   is closed, a `Flow` serializes — compliance/ops can edit scripts without a
   recompile. `flow.to_mermaid()` gives the diagram view.
4. **One unified `Guard`** over `(state, marking)`; closed serializable atoms +
   a `custom` closure escape hatch.

Folded in without new nouns: `commit` ≡ `once` + `never…until` + confirmation;
off-ramps ≡ terminal Steps reachable by a guard; timeouts ≡ a `within(Turns(n))`
qualifier (future).

## Lowering / integration (the anti-leakage guarantee)

`Flow` adds **no execution engine** — it is a thin monitor + projector that drives
the machinery already present:

| Flow concept | Lowers to |
|---|---|
| active steps' `posture` | model-role context in the turn-boundary `context_buffer` (`SteeringMode::ContextInjection`) |
| `done(captured(...))` unmet | `RepairAction::Nudge` at `handle_turn_complete` |
| `allow`/`deny`/`never…until`/`once` | the `before_tool` / confirmation seam |
| `commit` tools | + the `ConfirmationProvider` |
| `Marking`/`Verdict` | `State` keys (`flow:done:*`, `flow:active`) + `LiveEvent` |
| the DAG | `flow.to_mermaid()` |

The `FlowMonitor` keeps its own marking (Petri-style) rather than forcing the
linear `PhaseMachine` into a DAG engine; `PhaseMachine`/`Watcher`/`Temporal`/
`needs` become lowering targets / power-user escape hatches. `Flow` is the one
top-level primitive new users learn.

## Status & next

- **Landed:** `crates/gemini-adk-rs/src/flow` — `Flow`/`Step`/`Guard`/`Constraint`,
  the builder verbs, `FlowMonitor` (latch, `admits_tool`, `active_postures`,
  `unmet_requirements`, `verdict`), serde, `to_mermaid`, `commit` sugar, validation,
  10 unit tests.
- **Next:** L2 `Live::govern(flow).mode(...)`; wire `admits_tool` into the Live
  `before_tool` gate and `active_postures`/`unmet_requirements` into the
  turn-boundary steering/repair; a debt-collection cookbook; a `flow.md` user-guide
  chapter; mermaid in docs.
