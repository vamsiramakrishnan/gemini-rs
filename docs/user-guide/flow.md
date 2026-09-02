# Governed Flows (conversation/tool DAGs)

A `Flow` describes a workflow as a single DAG that governs **both** conversation
stages and tool-call sequences, and the runtime **enforces** it live: it blocks
inadmissible tool calls, steers the model with the active stage's instruction at
each turn boundary, and surfaces what still has to happen.

It is one declarative value in place of the four mechanisms you'd otherwise
hand-wire (a phase guard + a watcher + a `before_tool` check + repair text).

## The model in one breath

- A **Step** is the only node type. A step is *done* when its completion
  [`Guard`] latches true; `after` declares dependencies (the DAG edges).
- The same `Step` noun covers a **conversation stage** (it has a `posture`) and a
  **tool milestone** (its `done` is `called_ok(tool)`).
- A **Guard** is the only predicate type — over session state and the flow
  marking. Atoms are serializable; `Guard::custom(..)` is a code escape hatch.
- The runtime keeps a **Marking** (which steps are done) by replaying the trace,
  and answers tool admissibility + projects active postures.

## Define a flow

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let flow = Flow::new()
    .step("verify")
        .posture("Verify the caller's identity before anything else.")
        .allow(["lookup_account"])
        .done(Guard::is_true("identity_verified"))
    .step("disclose").after("verify")
        .posture("Give the required disclosure.")
        .done(Guard::is_true("disclosure_given"))
    .step("negotiate").after("disclose")
        .posture("Negotiate an affordable payment.")
        .allow(["lookup_balance", "payment_plans"])
        .done(Guard::captured(["ptp_amount", "ptp_date"]))
    .step("take_payment").after("negotiate")
        .allow(["charge_card"])
        .done(Guard::called_ok("charge_card"))      // a tool milestone — same `Step` noun
    .step("close").after("negotiate").terminal()
    // cross-cutting constraints
    .never("charge_card").until(Guard::is_true("ptp_confirmed"))
    .once("charge_card")
    .require(["close"])
    .build()
    .expect("valid flow");
```

`build()` validates referential integrity and acyclicity, so a malformed flow
fails fast rather than misbehaving live.

## Govern a session

```rust,ignore
let handle = Live::builder()
    .tools(dispatcher)
    .govern(flow)              // enforce — block inadmissible tools, steer per active step
    .connect_from_env()
    .await?;
```

Use `.observe(flow)` instead of `.govern(flow)` to record deviations for audit
without blocking anything.

## Compile, then govern

`Flow::compile()` turns a class of runtime surprises into load-time errors on
top of `build()`'s checks: unreachable steps, effectively-unguarded commit
tools, `never…until` guards whose `done(step)` references a step that doesn't
exist (the tool would be forbidden forever), and ordering cycles closed by
`before(a, b)` edges (every step on the cycle deadlocks). It returns a
`CompiledFlow` — proof the flow passed compilation.

`Flow::compile_with_tools(&[..])` additionally validates every tool name the
flow references (`allow`/`deny`/`once`/`never…until`/commit) against a registry
of known tools, catching typos and drift between a flow script and the tools
actually registered on the session.

```rust,ignore
// Compile once at load time — diagnostics surface here, not mid-call.
let compiled = flow.compile_with_tools(&["lookup_account", "charge_card"])?;

// Govern many sessions; connect does NOT re-validate or re-compile.
let handle = Live::builder()
    .tools(dispatcher)
    .govern_compiled(compiled)     // or .observe_compiled(compiled)
    .connect_from_env()
    .await?;
```

## Why is it blocked? (`handle.explain()`)

A governed session's handle answers the common debugging question directly —
which steps are active, which tools are admitted vs blocked (with reasons), and
what's still required — as a serializable `FlowExplanation` snapshot computed
against the live session state:

```rust,ignore
if let Some(ex) = handle.explain() {            // None when not governed
    println!("active: {:?}", ex.active);
    println!("blocked: {:?}", ex.blocked_tools); // tool -> reason
    println!("missing: {:?}", ex.missing_requirements);
}
```

`handle.explain()` is the same view under its descriptive name. Both are cheap,
synchronous snapshots of the monitor the control lane maintains.

### Driving orchestration on step entry

A step can run an agent the moment it becomes active — the flow *drives*
orchestration in-session:

```rust,ignore
let handle = Live::builder()
    .tools(dispatcher)
    .govern(booking_flow)
    // when `check` activates, run the availability agent; its result lands in
    // `check:result`, which the step completes on via `done(resolved("check"))`.
    .on_enter("check", availability_agent, AgentMode::Call)
    .connect_from_env()
    .await?;
```

`AgentMode::Call` resolves inline at the turn boundary; `Dispatch`/`Background`
run detached so a slow agent never blocks speech. The result is written to
`{step}:result`, so a downstream step reads it with `Guard::resolved(step)` —
the same convention as a [`Resolver`](./orchestration.md) (`call`/`dispatch`/
`background`) or a deterministic [`Extract`](./extraction.md) field. That shared
`State`-result convention is what makes the three lenses compose
multiplicatively: extraction fills slots, a step's `on_enter` orchestrates a
sub-agent or fetch, and guards gate on either — all reading the same `State`.

## Enforcement semantics

- **Tools are gated hard.** A call that no active step allows, that a
  `never…until` forbids, or that a `once` has spent, is denied at the
  `before_tool` boundary and an error is returned to the model. (This shares the
  seam used by middleware vetoes and the [`ConfirmationProvider`](./tool-policies.md).)
  - Note: a step's `allow`/`deny` only applies *while that step is active*. For a
    **cross-cutting** gate that must hold regardless of which step is active
    (e.g. "never transfer a spam caller", and recall that a `terminal()` step
    latches done immediately and is therefore never active), use a global
    `never(tool).until(guard)` constraint instead of a step `deny`.
  - **`allow` excludes by omission**, which is what you want for the domain tools
    a step is *about* and not for infrastructure no step is about. Name those
    with `ambient([tools])` — see below.

### Ambient tools

A step's `allow` list is a whitelist, so every tool the author did not think to
name is denied while that step is active. Writing `.allow(["book_table"])` means
"book here, don't search the catalogue" — it does not mean "stop remembering who
the caller is", but that is what it did to any cross-cutting tool.

`ambient` names those tools once, at flow level:

```rust
Flow::new()
    .ambient(["recall_context", "manage_memory"])
    .step("book")
        .allow(["book_table"])     // recall still available here
        .done(Guard::called_ok("book_table"))
```

Ambient is an exemption from *exclusion by omission* and nothing more. Anything
that **names** the tool still binds, because naming it is a deliberate act:

| Still applies to an ambient tool | Why |
|---|---|
| `deny([tool])` | the step named it |
| `once(tool)` | the constraint named it |
| `never(tool).until(guard)` | the constraint named it |

So a flow can hold `ambient(MEMORY_TOOLS)` *and* `never("manage_memory").until(verified)`
— reads stay available, writes wait for identity. Ambient tools also join the
flow's tool universe, so `compile_with_tools` still catches a registry that does
not cover them.

Extensions register their own: `Live::with_memory(..)` calls
`.ambient_tools(MEMORY_TOOLS)` for you, and the registration is merged into the
flow at connect, so it composes with `.govern(..)` written on either side of it.
- **Speech is shaped softly, proactively.** The active step's `posture` is
  injected as turn-boundary steering *before* the model speaks — you never block
  speech mid-stream in a voice session.
- **Repair from real gaps.** Unmet `require` steps are surfaced at the turn
  boundary so the model gathers what's missing.

## Phases and flows together

A `Flow` does **not** compile down to a `PhaseMachine`. They are independent
subsystems — the control plane holds each as its own `Option` — and configuring
both is supported and sometimes right. What you need to know is the cadence and
the order, because both steer the same model on the same turn.

**Different cadences.** A flow's active-step `posture` is re-projected on
*every* turn boundary. A phase's `instruction` is seeded only when a
*transition* fires. So a quiet turn carries the flow's posture and no phase
instruction — which is what stops the two from churning against each other.

**Deterministic order.** Everything projected at a turn boundary is accumulated
into one batch and sent as a single frame, in this order:

```
1. tool availability advisory      (on phase transition, if enabled)
2. repair nudge / escalation       (unmet `needs`)
3. phase steering context          (modifiers, under ContextInjection)
4. flow posture → ground → unmet requirements
5. resolved phase instruction      (transition turns, under ContextInjection)
```

The phase instruction lands **last**, nearest the user's next turn, so on a
transition the phase persona is the most recent framing the model reads. Under
the default `SteeringMode::InstructionUpdate` step 5 goes to the *system
instruction* instead of the batch, so the two never share a channel at all.

**Which to reach for.** Use phases when the conversation has personas or stages
that change how the assistant *speaks*. Use a flow when there are obligations
and orderings that must be *enforced* — gated tools, required steps, an audit
trail. Reach for both when you have both, and expect them to add rather than
arbitrate: nothing resolves a contradiction between a posture and a phase
instruction, so do not write one.

## Verbs (the closed vocabulary)

| Verb | Meaning |
|---|---|
| `step(id)` | declare a node |
| `after(dep)` | add a dependency (call repeatedly for several) |
| `after_when(dep, Guard)` | a **conditional edge** — satisfied only while the guard holds (branching) |
| `join_any()` | any one satisfied edge makes the step eligible (the merge after a branch) |
| `gate(Guard)` | extra eligibility beyond dependencies |
| `done(Guard)` | completion condition (required for non-terminal steps) |
| `posture(text)` | instruction imposed while active |
| `ground(template)` | curated, `State`-interpolated fact line projected while active (anti-hallucination) — `{key}` / `{key?yes:no}` |
| `allow([tools])` / `deny([tools])` | tool whitelist/blacklist while active |
| `ambient([tools])` | cross-cutting tools exempt from every step's `allow` whitelist |
| `terminal()` | a step that completes on eligibility |
| `once(tool)` | a tool may run at most once |
| `before(a, b)` | ordering invariant |
| `never(tool).until(Guard)` | forbid a tool until a guard holds |
| `require([steps])` | steps that must be done for completion |
| `reset([steps]).when(Guard)` | un-latch steps on the guard's rising edge — the loop primitive (`called_ok` evidence for those steps is forgiven; the DAG stays acyclic) |
| `commit(tool, until)` | sugar: `once` + `never…until` + confirmation |

Guard atoms: `is_true`, `is_set`, `eq`, `captured`, `called_ok`, `done`, `all`,
`any`, `not`, and `custom(closure)`.

## Data-driven flows

Because every guard atom is a named, parameterized predicate, a `Flow` is fully
serializable — so the script can be authored as data (e.g. RON/JSON) and edited
by compliance or ops without a recompile. `flow.to_mermaid()` renders the DAG.

See [Flows as JSON](./flow-json.md) for the JSON format reference, the
`FlowAppSpec` document that packages a flow into a runnable application (with
declarative mock tools), and the **Flow Studio** — the drag-and-drop editor at
`/flows` in `gemini-adk-web-rs` that authors, validates, and live-runs these
documents.

## Observability

The monitor publishes status into state (`flow:done`, `flow:active`) and exposes
`verdict(step)` (`Pending · Active · Done · Skipped`), `unmet_requirements()`,
`is_complete()`, and `violations()` — so watchers and dashboards can react, and
real traces can be scored for conformance.

## See also

- [Per-Tool Policies](./tool-policies.md) — `confirm`/`timeout`/`cached`; commit-tools
- [Phase System](./phases.md) — the other steering mechanism; independent of
  `Flow` and composable with it (see [Phases and flows together](#phases-and-flows-together))
- [Tool System](./tools.md) — defining the tools a flow gates
- cookbook [37 — governed flow](../../examples/cookbook/src/37_governed_flow.rs)
