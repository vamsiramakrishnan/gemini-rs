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

### Crawl (01–03): Foundations

Three combined examples that each cover a cluster of related building blocks.

| # | Example | What it covers |
|---|---------|----------------|
| 01 | Foundations | `AgentBuilder` (model, sampling, copy-on-write, contracts); `SimpleTool`/`TypedTool`/`ToolDispatcher` + built-in tools; `M::` callbacks/middleware |
| 02 | Combinators | `a >> b` sequential, `a \| b` parallel fan-out, `a * N` / `a * until(pred)` loops; `review_loop`/`fan_out_merge`/`supervised`; `check_contracts` |
| 03 | Composition | The `S::` (state), `P::` (prompt), `T::` (tools), `G::` (guards) operator algebra |

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

### Fly (34–40): Higher-order capabilities

See the capability table at the top — examples 34–40 cover tool governance,
persistence, MCP, **Flow**, **Extract**, and the booking/screening capstones.
(Connection helpers, the Live callback catalog, and the `#[tool]` macro now live
in the user guide: `auth-and-connecting`, `live-callbacks`, and `tools`.)

## Run any example

```bash
# Most governed-agent examples run with no credentials:
cargo run -p example-cookbook --bin 37-governed-flow
cargo run -p example-cookbook --bin 38-extraction

# Live examples read auth from the environment (see connect_from_env):
export GEMINI_API_KEY="your-key"
cargo run -p example-cookbook --bin 01-foundations
```
