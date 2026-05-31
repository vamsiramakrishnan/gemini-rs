# RFC: Agent Orchestration — `call` / `dispatch` / `background`

Status: proposed · Author: library team · 2026-05-30

## Motivation

Agents need to invoke *other* agents — a verifier, a pricing pipeline, a remote
A2A service, a fulfilment worker — sometimes **synchronously** (the conversation
waits on the answer) and sometimes **asynchronously** (fire-and-forget, or slow
work the model weaves in later). Today the pieces exist but as three unrelated
APIs (`TextAgentTool`, `BackgroundAgentDispatcher`, `ToolExecutionMode::Background`).
This RFC unifies them into **one invocation model**: an agent is a value; you
invoke it in one of three **Modes**; the result always lands in governed `State`,
so coordination is reactive and uniform regardless of who triggered it.

## Agents are values

Any of these is a `TextAgent` and plugs in identically:
- a local agent,
- a **composed pipeline** via the existing combinators (`a >> b`, `a | b`,
  `race(a, b)`, `a / b` fallback, `route(..)`, `a * until(..)`),
- a **remote A2A agent** (`RemoteA2aAgent`).

Topology (sequential, fan-out/join, race, fallback, supervisor, loop) is the
combinators' job. Orchestration is just *how* and *when* you invoke the result.

## The three Modes (the whole vocabulary)

| Mode | Sync? | Who waits | Result lands | Use when |
|---|---|---|---|---|
| **`call`** | sync | caller blocks | returned + State | the next step/utterance *depends* on it (and it's fast) |
| **`dispatch`** | async, fire-and-forget | nobody | State (+ may trigger more) | a side-effect the conversation doesn't wait on |
| **`background`** | async, model-aware | model keeps talking | delivered to the model via `FunctionResponseScheduling` + State | slow work the model should weave in |

## Any invoker, same Modes

| Invoker → | `call` | `dispatch` | `background` |
|---|---|---|---|
| **Model** | `.agent_tool("verify", …)` | — | `.agent_tool(…).tool_background()` |
| **Flow** (step) | `on_enter(run(agent, Call))` awaits before advancing | `on_enter(run(agent, Dispatch))` | `on_enter(run(agent, Background))` |
| **Extract** (`on_complete`) | — | `dispatch("risk", agent)` | — |
| **Watcher** | — | `watch(k).then(dispatch(agent))` | — |

## Coordination is reactive (the elegant part)

Every Mode writes its result to governed `State` under a predictable key
(`agent:<name>:result`) and emits `LiveEvent::AgentResult` on the trace. So
**multi-agent coordination is reactive, not a bespoke graph**:

- `dispatch` agent A → it sets `risk_score` → a `Flow` guard
  (`done(is_true("risk_cleared"))`) or another `Extract` reacts. No "A then B" wiring.
- Need an *explicit* sync topology (fan-out + join, race, fallback)? Compose with
  the **combinators** into one agent, then `call`/`dispatch`/`background` it.

> **Combinators for explicit topology · State for reactive coordination · Modes
> for sync/async.** Sequential, parallel-join, race, fallback, supervisor, and
> recursive loops all fall out of `combinators × Mode × State`.

## Voice nuance — sync blocking is dead air

In a live voice session, `call` (sync) stalls into silence. So: `call` only for
*fast* dependencies; `background` (with an immediate "checking…" ack + scheduled
delivery) for anything slow the model should report; `dispatch` for anything the
conversation never waits on. The default for a *slow* agent in a voice session is
`background`, not `call`.

## Closed vocabulary

Nouns: `Agent` (= `TextAgent`) · `Mode` (`Call` / `Dispatch` / `Background`) ·
`Result`. Verbs: `call` · `dispatch` · `background` · `run(agent, mode)` (the
generic form used by Flow/Effects).

🚫 Ban-list: `spawn`, `task`, `thread`, `future` — orchestration is expressed in
Modes, never raw concurrency.

## Lowering / anti-leakage

No new runtime — each Mode compiles onto an existing mechanism:

| Mode | Lowers to |
|---|---|
| `call` | `TextAgentTool` (agent-as-tool), awaited inline |
| `dispatch` | `BackgroundAgentDispatcher::dispatch` |
| `background` | an agent-tool with `ToolExecutionMode::Background` + a scheduling mode |
| `run(agent, mode)` from Flow/Effect | the above, invoked from `on_enter`/`on_complete` |
| result + `AgentResult` event | `State` (`agent:<name>:result`) + the `LiveEvent` trace |

Remote agents reuse this verbatim because `RemoteA2aAgent: TextAgent`.

## Decisions (resolved — win-win)

| # | Decision | Win-win |
|---|---|---|
| O1 | One API or three | **One `Mode` vocabulary**; the three existing APIs become its lowering |
| O2 | Where results go | **Always governed `State`** (+ trace event) → reactive coordination, observable |
| O3 | Voice default for slow agents | **`background`**, never sync `call` |
| O4 | Topology | **Combinators compose agents**; orchestration only picks the `Mode` |
| O5 | Errors/timeouts | Reuse `race`/`timeout`/`fallback` combinators + `on_tool_error`; a failed `dispatch` writes `agent:<name>:error` |
| O6 | Remote (A2A) | Uniform — A2A agents are `TextAgent`, same Modes |

## Worked example — booking

```rust
Live::builder()
    .agent_tool("availability", "Check open slots", availability_agent)   // call (sync, fast)
    .extract(Extract::record::<Booking>()
        .on_complete(dispatch("confirm_email", email_agent)))             // dispatch (fire-and-forget)
    .agent_tool("crm_history", "Fetch prior visits", crm_agent)           // ...
    .tool_background("crm_history")                                       // background (slow, model-aware)
    .govern(booking_flow)
    .connect_from_env().await?;
```

## Build plan

- **v1:** the `Mode` surface (`call`/`dispatch`/`background`) over the three
  existing mechanisms; the `agent:<name>:result` + `AgentResult` convention; the
  `Flow` step effect `on_enter(run(agent, mode))`. (Pairs with `Extract`'s
  `on_complete(dispatch(..))`.)
- **v2:** structured join/await on dispatched results from a `Flow` guard
  (`done(resolved("agent:risk:result"))`); cancellation/supersede on interruption.
