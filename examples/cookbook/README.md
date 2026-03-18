# Cookbook — 30 Progressive Examples

Thirty numbered examples organized in three tiers of increasing complexity. Each is a standalone binary.

## Tiers

### Crawl (01-10): Foundations

| # | Example | What it covers |
|---|---------|----------------|
| 01 | Simple Agent | `AgentBuilder::new().instruction().build()` |
| 02 | Agent with Tools | `google_search()`, `code_execution()` |
| 03 | Callbacks | `on_text`, `on_audio`, `on_turn_complete` |
| 04 | Sequential Pipeline | `agent_a >> agent_b` |
| 05 | Parallel Fan-out | `agent_a \| agent_b` |
| 06 | Loop Agent | `agent * 3`, `agent * until(pred)` |
| 07 | State Transforms | `S::pick`, `S::rename`, `S::merge` |
| 08 | Prompt Composition | `P::role + P::task + P::format` |
| 09 | Tool Composition | `T::simple \| T::google_search()` |
| 10 | Guards | Conditional execution with guard predicates |

### Walk (11-20): Multi-Agent Patterns

| # | Example | What it covers |
|---|---------|----------------|
| 11 | Route Branching | `RouteTextAgent` with rules |
| 12 | Fallback Chain | `agent_a / agent_b` |
| 13 | Review Loop | `review_loop` pattern |
| 14 | Map Over | `MapOverTextAgent` across items |
| 15 | Middleware Stack | `M::` middleware composition |
| 16 | Context Engineering | `C::window + C::user_only` |
| 17 | Evaluation Suite | `E::` evaluation composition |
| 18 | Artifacts | `A::json_output + A::text_input` |
| 19 | Agent Tool | `agent_tool()` — agent as a callable tool |
| 20 | Supervised | Supervised agent patterns |

### Run (21-30): Production Patterns

| # | Example | What it covers |
|---|---------|----------------|
| 21 | Full Algebra | All S.C.T.P.M.A operators together |
| 22 | Contract Testing | `check_contracts` validation |
| 23 | Deep Research | Multi-step research pipeline |
| 24 | Customer Support | Phase-driven support agent |
| 25 | Code Review | Code analysis pipeline |
| 26 | Dispatch and Join | `DispatchTextAgent` + `JoinTextAgent` |
| 27 | Race and Timeout | `RaceTextAgent` + `TimeoutTextAgent` |
| 28 | A2A Remote | Agent-to-Agent protocol |
| 29 | Live Voice | Full Live session with voice |
| 30 | Production Pipeline | Everything combined |

## Run any example

```bash
export GOOGLE_GENAI_API_KEY="your-key"
cargo run -p example-cookbook --bin 01-simple-agent
cargo run -p example-cookbook --bin 15-middleware-stack
cargo run -p example-cookbook --bin 30-production-pipeline
```
