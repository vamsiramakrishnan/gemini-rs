# Synthesis: Flow × Extract × Orchestration — one multiplicative core

Status: proposed (refinement of the Flow, Extract, and Orchestration RFCs)
· 2026-05-30

This refines the three specs so they don't just *coexist* — they **multiply**.
The thesis: there is **one small core**, and Flow / Extract / Orchestration are
three *lenses* on it. Because every lens reads and writes the same substrate,
combining them yields a combinatorial space of governed behaviors with no glue
code.

## The realization: one core, three lenses

Everything reduces to **six concepts** over a shared substrate:

| Concept | One-line meaning | Shared by |
|---|---|---|
| **State** | typed key-value facts (the worldview) | all three |
| **Trace** | the ordered event log (the history) | all three |
| **Guard** | the *only* predicate: `(State, Trace, Marking) -> bool` | Flow gates · Extract triggers · `never…until` · watchers |
| **Resolver** | given inputs bound from State, produce a value/effect | Extract field sources · Flow step effects · tool/MCP fetches · sub-agents |
| **Mode** | execution discipline of an async Resolver: `Call` / `Dispatch` / `Background` | Orchestration · Flow effects · fetch/agent sources |
| **Effect** | what to do with an outcome: `set` / `ground` / `dispatch` / `emit` | Extract `on_complete` · Flow `on_enter` · watchers |

> **Flow** orders Resolvers and gates tools. **Extract** binds Resolvers to typed
> fields. **Orchestration** runs Resolvers in a Mode. All coordinate through
> **State + Trace** — reactively.

## The four unifying refinements

### R1 — `Resolver` unifies "where a value comes from"
A recognizer, an OOB-LLM extraction, a tool fetch, an MCP call, and a sub-agent
are all the same shape: *inputs (state keys) → result → written to State (with
provenance)*. Collapse them into one `Resolver` enum:

| Resolver | Sync? | Inputs | Notes |
|---|---|---|---|
| `Recognizer(..)` | CPU sync | transcript | the only non-async one |
| `Llm(model, prompt)` | async | transcript window | today's `extract_turns` |
| `Fetch(tool, args)` | async | State keys | a registered tool |
| `Mcp(server, tool, args)` | async | State keys | MCP toolset |
| `Agent(agent)` | async | State | a `TextAgent` / pipeline / A2A |
| `NativeFn(name)` | model-driven | — | a generated capture-tool the model calls |

This is the big collapse: **an `Extract` field source, a `Flow` step effect, and
an "agent call" are all `run(Resolver, Mode)`.** "Fetch from a system" and "call a
sub-agent" become the *same* operation.

### R2 — `Guard` is the single predicate, everywhere
Flow `gate`/`done`, Extract `trigger`, `never…until`, and watcher conditions are
all `Guard`. Atoms are the closed serializable set (`is_true`, `eq`, `captured`,
`called_ok`, `done`, `resolved`, …) + a `custom` closure escape hatch. One
predicate language across the whole system.

### R3 — `Mode` applies to *any* async Resolver, from *any* invoker
`Call` / `Dispatch` / `Background` are not per-primitive — they're the execution
discipline of a Resolver, whether the model, a Flow step, an Extract, or a
watcher invokes it. Results always land in `State` (`agent:<name>:result`,
`<record>:<field>`, …) + a `Trace` event → reactive coordination.

### R4 — `Effect` is the single outcome vocabulary
`set(key)` · `ground(template)` (curated projection → steering) · `dispatch(agent)`
· `emit(event)`. Used identically by Extract `on_complete`, Flow `on_enter`, and
watchers. No per-primitive effect dialects.

## The multiplicative interplay

| Combination | What it produces |
|---|---|
| **Flow × Extract** | a step `done(captured([...]))` completes when an Extract lands its fields → deterministic, LLM-free stage advancement + grounded repair |
| **Extract × Orchestration** | a field whose `Resolver` is `Agent(..)` / `Fetch(..)`; `on_complete(dispatch(..))` triggers a downstream agent → extraction is a *router* |
| **Flow × Orchestration** | step `on_enter(run(agent, Mode))`; the agent's result → a `Guard` → the next step → governed multi-agent stages |
| **all three** | a gated, grounded, multi-agent conversation declared in one block (below) |

Each lens *consumes the others' outputs through State*, so capability isn't
additive — `N` resolvers × `M` steps × `K` guards × `3` modes compose into a
combinatorial space, declaratively, with zero orchestration glue.

## The proof — booking, all three lenses, one block

```rust
#[derive(Extract)]
struct Booking {
    #[extract(datetime)]                                   when: Option<DateTime>,    // Recognizer
    #[resolve(fetch = "calendar.is_open", args = { when }, ttl = "30s")]
    available: Option<bool>,                               // Fetch resolver, reactive
    #[resolve(agent = "crm")]                              prior_visits: Option<u32>, // Agent resolver
}

let flow = Flow::new()
    .step("collect").posture("Ask for a preferred time.").done(Guard::captured(["when"]))
    .step("check").after("collect")
        .on_enter(run("calendar.is_open", Mode::Call))     // sync, fast → gate the next step
        .done(Guard::eq("available", true))
    .step("book").after("check").allow(["book_appointment"])
        .done(Guard::called_ok("book_appointment"))
    .step("close").after("book").terminal()
    .commit("book_appointment", Guard::is_true("available"))
    .require(["close"]).build()?;

Live::builder()
    .extract(Extract::record::<Booking>()
        .ground("{when} is {available?open:taken}; {prior_visits} prior visits.")
        .on_complete(dispatch("notify", reminder_agent)))  // async, fire-and-forget
    .agent_tool("crm", "Fetch visit history", crm_agent).tool_background("crm")  // background
    .govern(flow)
    .connect_from_env().await?;
```

One declaration: facts resolved from transcript/system/agent, the model grounded
on validated State, stages gated on those facts, a commit-tool guarded, and two
sub-agents orchestrated (sync gate + async notify + background CRM) — coordinating
purely through State.

## The unified closed glossary

`State` · `Trace` · `Guard` · `Resolver` (`Recognizer`/`Llm`/`Fetch`/`Mcp`/`Agent`/`NativeFn`)
· `Mode` (`Call`/`Dispatch`/`Background`) · `Effect` (`set`/`ground`/`dispatch`/`emit`)
· `Flow`/`Step` · `Extract`/`Record`/`Field` · `Marking`/`Verdict` · `Provenance`.

🚫 One ban-list: `phase`, `transition`, `watch`, `needs`, `extractor`, `prompt`,
`tool` (as a noun in a spec), `spawn`/`task`/`thread`/`future`. All are lowering
details.

## Ergonomic refinements (consistency = elegance)

- **One prelude** exports the core (`Flow`, `Guard`, `Extract`, `Resolver`,
  `Mode`, `dispatch`, …). Same import for everything.
- **Consistent builder idiom** across Flow/Extract: declare a node/field, set its
  `Guard`/`Resolver`/`Effect`/`Mode`; no colliding verbs.
- **Derive where typed** (`#[derive(Extract)]`, `#[tool]`), **serialize where
  deterministic** (Flow + recognizer/fetch/mcp resolvers → data-driven scripts),
  **closure escape hatch where bespoke** (`Guard::custom`, `Resolver::Agent`).
- **One observability convention:** `State` keys (`flow:*`, `agent:*`, record
  fields) + `Provenance` in `state_meta` + `LiveEvent` on the `Trace`.
- **Lineage diagrams:** `flow.to_mermaid()` for control; an `Extract` record can
  render its resolver graph (field ← resolver ← inputs) — the data lineage.

## Alignment changes to fold into the three RFCs

- **Extract RFC:** call field "sources" **Resolvers**; arg-binding is from State
  keys; `Mode` applies to async resolvers; add `Resolver::Agent`.
- **Flow RFC:** add `done(resolved("agent:x:result"))` and
  `on_enter(run(resolver, mode))`; state that `Guard` is the shared predicate.
- **Orchestration RFC:** an agent invocation *is* `run(Resolver::Agent, Mode)`;
  fold `call`/`dispatch`/`background` under the shared `Mode`.
- **Guard:** one type across Flow steps, Extract triggers, `never…until`, watchers.

## Build sequencing (shared core first → multiplicative payoff)

1. **Shared core:** `Guard` (have it in Flow) + `Resolver` + `Mode` + `Effect` +
   arg-binding-from-State + result-to-State + `Provenance`. This is the multiplier.
2. **Extract lens:** recognizer kit + `#[derive(Extract)]` + resolvers-as-sources
   (subsumes `extract_turns`).
3. **Orchestration lens:** `Mode` over the three existing mechanisms + Flow
   `on_enter(run(..))`.
4. **Cookbooks:** booking (all three) + screening; `extraction.md` + update
   `flow.md` with the interplay.

Build the core once; the three lenses then compose for free — that's the
multiplicative win.
