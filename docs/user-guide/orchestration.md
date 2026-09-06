# Agent Orchestration

Choose how to invoke a sub-agent, service function, or separate model call,
and how the next step observes its result. The mechanisms below publish
success or failure into shared state under named keys.

Check completion, cancellation, and error handling separately. A dispatched
task is not complete merely because its invocation returned.

## Mode — how an agent is invoked

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;   // AgentMode, call_agent, Resolver

// Call — synchronous; the caller awaits. Use only for fast dependencies.
let verdict = call_agent("availability", agent, &state).await?;   // → availability:result

// Dispatch — fire-and-forget; the conversation does not wait.
// Background — model-aware; runs detached, result delivered back to the model.
```

| Mode | Sync? | Lowers to |
|------|-------|-----------|
| `Call` | sync — caller awaits | agent run inline, result → `State` |
| `Dispatch` | async, fire-and-forget | `BackgroundAgentDispatcher::dispatch` |
| `Background` | async, model-aware | an agent-tool marked `ToolExecutionMode::Background` |

## Resolver — a named async value source

A `Resolver` generalizes `call` from "a sub-agent" to **any** async source whose
inputs come from `State`. It is the async sibling of the deterministic
[`Recognizer`](./extraction.md):

```rust,ignore
use gemini_adk_rs::Resolver;

// A sub-agent (its String output becomes the result):
Resolver::agent("availability", availability_agent).resolve(&state).await?;

// Any async system — a tool call, HTTP fetch, or MCP request — bound from State:
Resolver::fetch("availability", |s: State| async move {
    let slot = s.get::<String>("slot").unwrap_or_default();
    Ok(serde_json::json!({ "open": slot == "afternoon" }))
}).resolve(&state).await?;          // or .dispatch(state) to run detached

// A one-shot OOB LLM over a {key}-interpolated prompt:
Resolver::llm("summary", flash_llm, "Summarize the {topic} issue").resolve(&state).await?;
```

All three write `{name}:result` and record **provenance** under
`state_meta:{name}:result` (`source: agent | fetch | llm`), readable with
`provenance(&state, "name:result")`.

## Flow-driven orchestration

A governed `Flow` drives orchestration in-session: a step's `on_enter` runs a
resolver/agent the moment it activates, and an `Extract` record can dispatch a
downstream agent `on_complete`:

```rust,ignore
Live::builder()
    .govern(booking_flow)
    .on_step_enter("check", availability_agent, AgentMode::Call)   // result → check:result
    .extract_record(
        Extract::record("triage")
            .field("intent", Recognizer::one_of(["refund", "status"]))
            .on_complete(router_agent, AgentMode::Dispatch)    // result → triage:result
            .build(),
    )
    .connect_from_env().await?;
```

Because every result lands in `State` under the same convention, the three
lenses — [extraction](./extraction.md), orchestration, and
[flow](./flow.md) — compose multiplicatively: extraction fills slots, a step
orchestrates a sub-agent or fetch, and guards gate on either.

## See also

- [Extraction Pipeline](./extraction.md) — `Recognizer`/`Resolver` field sources
- [Governed Flows](./flow.md) — `on_enter`, `ground`, `Guard::resolved`
- [Text Agent Combinators](./text-agents.md) — building the agents you orchestrate
- cookbook [39 — booking](../../examples/cookbook/src/39_booking.rs) (Flow × Extract × Orchestration)
