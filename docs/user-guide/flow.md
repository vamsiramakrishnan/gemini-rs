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
    .model(GeminiModel::Gemini2_0FlashLive)
    .tools(dispatcher)
    .govern(flow)              // enforce — block inadmissible tools, steer per active step
    .connect_from_env()
    .await?;
```

Use `.observe(flow)` instead of `.govern(flow)` to record deviations for audit
without blocking anything.

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
the same convention as a `Resolver` (`call`/`dispatch`/`background`) or a
deterministic [`Extract`](./extraction.md) field. That shared
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
- **Speech is shaped softly, proactively.** The active step's `posture` is
  injected as turn-boundary steering *before* the model speaks — you never block
  speech mid-stream in a voice session.
- **Repair from real gaps.** Unmet `require` steps are surfaced at the turn
  boundary so the model gathers what's missing.

## Verbs (the closed vocabulary)

| Verb | Meaning |
|---|---|
| `step(id)` | declare a node |
| `after(dep)` | add a dependency (call repeatedly for several) |
| `gate(Guard)` | extra eligibility beyond dependencies |
| `done(Guard)` | completion condition (required for non-terminal steps) |
| `posture(text)` | instruction imposed while active |
| `ground(template)` | curated, `State`-interpolated fact line projected while active (anti-hallucination) — `{key}` / `{key?yes:no}` |
| `allow([tools])` / `deny([tools])` | tool whitelist/blacklist while active |
| `terminal()` | a step that completes on eligibility |
| `once(tool)` | a tool may run at most once |
| `before(a, b)` | ordering invariant |
| `never(tool).until(Guard)` | forbid a tool until a guard holds |
| `require([steps])` | steps that must be done for completion |
| `commit(tool, until)` | sugar: `once` + `never…until` + confirmation |

Guard atoms: `is_true`, `is_set`, `eq`, `captured`, `called_ok`, `done`, `all`,
`any`, `not`, and `custom(closure)`.

## Data-driven flows

Because every guard atom is a named, parameterized predicate, a `Flow` is fully
serializable — so the script can be authored as data (e.g. RON/JSON) and edited
by compliance or ops without a recompile. `flow.to_mermaid()` renders the DAG.

## Observability

The monitor publishes status into state (`flow:done`, `flow:active`) and exposes
`verdict(step)` (`Pending · Active · Done · Skipped`), `unmet_requirements()`,
`is_complete()`, and `violations()` — so watchers and dashboards can react, and
real traces can be scored for conformance.

## See also

- [Per-Tool Policies](./tool-policies.md) — `confirm`/`timeout`/`cached`; commit-tools
- [Phase System](./phases.md) — the lower-level stage machine `Flow` lowers onto
- [Tool System](./tools.md) — defining the tools a flow gates
- cookbook [37 — governed flow](../../examples/cookbook/src/37_governed_flow.rs)
