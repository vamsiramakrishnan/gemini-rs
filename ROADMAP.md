# gemini-rs Roadmap

> Status legend: ✅ shipped · 🚧 in progress · 📋 planned · 💭 exploratory
>
> Current release: **0.7.0** (2026-05-31). This roadmap is the living plan for
> what comes after. It is organized into milestones, each a coherent, shippable
> increment. The headline theme — **Governed Agents** — landed in 0.7.0; the
> work ahead is about *unifying its core*, *completing its L2 surface*, and
> *finishing the server/tooling story* so the whole stack is production-grade.

---

## North Star

One small core — **State · Trace · Guard · Resolver · Mode · Effect** — exposed
through three composable lenses:

- **Flow** orders Resolvers and gates tools.
- **Extract** binds Resolvers to typed fields.
- **Orchestration** runs Resolvers in a Mode.

All three coordinate through the same `State` + `Trace` substrate, so capability
is *multiplicative*, not additive. The design is captured in
[`docs/plans/2026-05-30-governed-agents-synthesis.md`](docs/plans/2026-05-30-governed-agents-synthesis.md)
and the three RFCs alongside it. Everything below either realizes that synthesis
or hardens the surrounding stack.

---

## Milestone 1 — Core unification polish (`0.8.0`)

The three lenses shipped in 0.7.0 but with implementation seams the synthesis
asks us to close. This milestone pays down that divergence so the core is *one*
thing, not three that happen to share `State`.

- 📋 **Disambiguate the two `Mode` enums.** `orchestration::Mode`
  (`Call`/`Dispatch`/`Background`) and `flow::Mode` (`Enforce`/`Observe`) are
  unrelated concepts sharing a name. Rename `flow::Mode` → `GovernMode` (or
  `Enforcement`) and reserve `Mode` for the resolver execution discipline, as the
  synthesis glossary specifies. *(breaking — batch with other renames)*
- 📋 **Promote `Effect` to a first-class type.** Today `set`/`ground`/`dispatch`
  are realized ad hoc (`Step::posture`, `Step::ground`, `on_complete`). Introduce
  a single `Effect` vocabulary (`set` · `ground` · `dispatch` · `emit`) used
  identically by Extract `on_complete`, Flow `on_enter`, and watchers.
- 📋 **Unify the `Resolver` surface.** `orchestration::Resolver` covers
  `agent`/`fetch`/`llm`; `Recognizer`, `Mcp`, and `NativeFn` live elsewhere. Fold
  them under one conceptual `Resolver` so "extract a field", "Flow step effect",
  and "call a sub-agent" are visibly the same `run(Resolver, Mode)` operation.
- 📋 **One prelude, one glossary.** Re-export the unified core from a single
  `governed` prelude; enforce the synthesis ban-list (`phase`, `transition`,
  `watch`, `needs`, `extractor` as public spec nouns) in favor of the new
  vocabulary, keeping the old names as deprecated aliases for one release.

## Milestone 2 — Flow L2 completion (`0.8.0`)

Flow enforcement is wired through the Live tool gate; finish the ergonomic and
observability surface.

- ✅ `Live::govern()` / `Live::observe()` and `admits_tool` → `before_tool` gate.
- ✅ `FlowMonitor` token-replay marking, active postures/grounds, `to_mermaid()`.
- 📋 **Lineage rendering for Extract.** Mirror `flow.to_mermaid()` with a record
  resolver-graph render (`field ← resolver ← inputs`) for data lineage.
- 📋 **Turn-boundary repair from unmet `require`s.** Surface unmet step
  requirements as structured repair nudges (today they are exposed but not
  auto-driven at the turn boundary).
- 💭 **Structured join/await on dispatched results** from Flow guards
  (`done(resolved("agent:x:result"))` blocking variant) + cancellation/supersede
  on interruption.

## Milestone 3 — Extract v2 (`0.9.0`)

- ✅ Recognizer kit (`integer`/`money`/`datetime`/`one_of`/`regex`/`fuzzy`/`yes_no`),
  `#[derive(Extract)]`, async resolver field sources with TTL cache.
- 📋 **Subsume `extract_turns` into the unified Extract API.** Keep
  `extract_turns` as a deprecated thin shim over `extract_record(... llm source)`.
- 📋 **MCP resolver field sources** as a first-class `Resolver::Mcp` source.
- 💭 **Demand-driven (lazy) field resolution** — resolve a field only when a guard
  reads it; Flow-driven `prefetch` to parallelize independent resolutions.
- 💭 **Native capture tools** — model-driven opt-in capture with `Silent`/`WhenIdle`
  scheduling.

## Milestone 4 — Server & tooling completion (`0.8.x`)

Close the three concrete loose ends so the server and CLI are feature-complete.

- ✅ **Wire `POST /eval/run` to `gemini_adk_rs::evaluation`.** Maps criteria →
  evaluators, runs the agent (or evaluates pre-recorded invocations), aggregates to
  a real `EvalResultSummary`, and serves it from `GET /eval/results`. Deterministic
  evaluators only for now (`response_match`/`exact_match`/`tool_trajectory`); LLM-judge
  criteria (safety/hallucination/rubric) are reported as skipped — see
  `apps/gemini-adk-server-rs/src/eval.rs`. *(LLM-judge wiring tracked below.)*
- 📋 **LLM-judge eval criteria over REST** — let `safety`/`hallucination`/`rubric`/
  `llm_judge` criteria resolve a judge LLM from the agent registry/env.
- 📋 **Wire `GET /debug/trace/:id` to a span store.** Add an in-memory finished-span
  collector keyed by trace id behind the `tracing-support` feature; serve real
  spans instead of `[]`.
- 📋 **Vertex AI Agent Engine deploy** (`adk deploy`). Package the agent, call the
  Agent Engine API, poll the operation to completion.

## Milestone 5 — Hardening & DX (`ongoing`)

- 📋 **L2 governed-agents tests.** Add a `tests/` suite for `Live::govern` /
  `extract_record` end-to-end (today coverage is L1-inline + cookbook examples).
- 📋 **Cookbook capstones** combining all three lenses (39/40 exist; add a booking
  walkthrough wiring fetch + agent + flow in one block, per the synthesis proof).
- 📋 **`extraction.md` user guide** + interplay section in `flow.md`.
- 💭 **Transpiler completion** — fill the Python→Rust ADK callback/agent-trait
  stubs in `tools/gemini-adk-transpiler-rs`.

---

## Recently shipped (0.7.0)

- **Governed Agents**: Flow (governed DAG + `FlowMonitor`), Extract (recognizers +
  `#[derive(Extract)]` + async resolvers), Orchestration (`Mode` + `Resolver` +
  provenance), wired into `Live` via `.govern()`/`.observe()`.
- Cookbook capstones `39_booking` / `40_screening`; mdbook *Agent Orchestration*
  chapter; RFCs in `docs/plans/`.

See [`CHANGELOG.md`](CHANGELOG.md) for the full history.
