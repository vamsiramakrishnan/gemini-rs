# Cookbook — Governed Agents & Composition Examples

38+ standalone, runnable examples. The cookbook is organized around the
**higher-order, governed-agent capabilities** this SDK is built on, with the
composition foundations beneath them.

## Governed Agents — start here

The primitives that make an agent *governed*. They compose **multiplicatively**
(see the RFCs in `docs/plans/`: Flow, Extraction kit, Orchestration, and the
synthesis).

| Capability | Example | What it shows |
|---|---------|----------------|
| **Flow** — governed conversation/tool DAG | `37-governed-flow` | gate tools, project postures, enforce order, `once`/`never…until`, mermaid |
| **Extract** — deterministic facts (no LLM) | `38-extraction` | CPU recognizers fill `State` → drive a `Flow` guard |
| **Orchestration** — `call`/`dispatch`/`background` | `19-agent-tool`, `26-dispatch-join`, `27-race-timeout` | sub-agents sync/async; results → `State` |
| **Tool governance** | `34-tool-policies` | `confirm`/`timeout`/`cached` + `ConfirmationProvider` |
| **Persistence** | `35-session-persistence` | snapshot/resume + session store |
| **MCP tools** | `36-mcp-tools` | stdio/SSE MCP integration |
| **Capstones** *(combine all three)* | `39-booking`, `40-screening` | Flow × Extract × Orchestration |

```bash
cargo run -p example-cookbook --bin 37-governed-flow
cargo run -p example-cookbook --bin 38-extraction
```

## Composition foundations

The building blocks the governed capabilities compose — a **Crawl → Walk → Run**
path through the builder API, the S·C·T·P·M·A operator algebra, and the
text-agent combinators.

### Crawl (01–10): Foundations

| # | Example | What it covers |
|---|---------|----------------|
| 01 | Simple Agent | `AgentBuilder::new().instruction().build()` |
| 02 | Agent with Tools | `SimpleTool`, `TypedTool`, `google_search()` |
| 03 | Callbacks | `on_text`, `on_audio`, `on_turn_complete` |
| 04 | Sequential Pipeline | `agent_a >> agent_b` |
| 05 | Parallel Fan-out | `agent_a \| agent_b` |
| 06 | Loop Agent | `agent * 3`, `agent * until(pred)` |
| 07 | State Transforms | `S::pick`, `S::rename`, `S::merge` |
| 08 | Prompt Composition | `P::role + P::task + P::format` |
| 09 | Tool Composition | `T::simple \| T::google_search()` |
| 10 | Guards | `G::` input/output validation |

### Walk (11–22): Multi-Agent Patterns

| # | Example | What it covers |
|---|---------|----------------|
| 11 | Route Branching | `RouteTextAgent` with rules |
| 12 | Fallback Chain | `agent_a / agent_b` |
| 13 | Review Loop | `review_loop` convergence |
| 14 | Map Over | `MapOverTextAgent` across items |
| 15 | Middleware Stack | `M::` middleware composition |
| 16 | Context Engineering | `C::window + C::user_only` |
| 17 | Evaluation Suite | `E::` evaluation composition |
| 18 | Artifacts | `A::json_output + A::text_input` |
| 19 | Agent Tool | `agent_tool()` — agent as a callable tool (orchestration) |
| 20 | Supervised | Human-in-the-loop approval |
| 21 | Full Algebra | All S·C·T·P·M·A operators together |
| 22 | Contract Testing | `check_contracts` validation |

### Run (23–30): Production Patterns

| # | Example | What it covers |
|---|---------|----------------|
| 23 | Deep Research | Multi-step research pipeline |
| 24 | Customer Support | Phase-driven support agent |
| 25 | Code Review | Code analysis pipeline |
| 26 | Dispatch and Join | `DispatchTextAgent` + `JoinTextAgent` (async orchestration) |
| 27 | Race and Timeout | `RaceTextAgent` + `TimeoutTextAgent` |
| 28 | A2A Remote | Agent-to-Agent protocol (remote orchestration) |
| 29 | Live Voice | Full `Live::builder()` session |
| 30 | Production Pipeline | Everything combined |

### Fly (31–38): Higher-order capabilities

See the capability table at the top — examples 31–38 cover connection,
callbacks, the `#[tool]` macro, tool governance, persistence, MCP, **Flow**, and
**Extract**.

## Run any example

```bash
# Most governed-agent examples run with no credentials:
cargo run -p example-cookbook --bin 37-governed-flow
cargo run -p example-cookbook --bin 38-extraction

# Live examples read auth from the environment (see connect_from_env):
export GEMINI_API_KEY="your-key"
cargo run -p example-cookbook --bin 01-simple-agent
```
