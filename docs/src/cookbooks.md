# Examples

The repository contains two sets of runnable examples:

1. **`examples/cookbook/`** — 30 progressive text-based examples demonstrating SDK composition patterns (no server required)
2. **`gemini-adk-web` apps** — Interactive voice/text demos bundled into a devtools-enabled web UI

---

## Cookbook Examples (`examples/cookbook/`)

A structured **Crawl → Walk → Run** learning path. Each example is a self-contained Rust binary with detailed doc comments explaining every API used.

```bash
# Run any example directly
cargo run -p example-cookbook --example 01_simple_agent
cargo run -p example-cookbook --example 17_evaluation_suite
cargo run -p example-cookbook --example 30_production_pipeline
```

### Crawl (01–10) — Foundations

The core builder API and composition primitives. No async runtime required for most examples.

| # | Binary | What it covers |
|---|--------|----------------|
| 01 | `01_simple_agent` | `AgentBuilder`: name, model, instruction, temperature, thinking budget, copy-on-write semantics |
| 02 | `02_agent_with_tools` | `SimpleTool` with raw JSON args; `TypedTool` with auto-generated JSON Schema from `schemars::JsonSchema` |
| 03 | `03_callbacks` | Event callbacks: `on_text`, `on_audio`, `on_thought`, `on_tool_call`, `on_interrupted`, `on_turn_complete` |
| 04 | `04_sequential_pipeline` | `>>` operator: multi-step pipelines, state passing between agents, `SequentialTextAgent` |
| 05 | `05_parallel_fanout` | `\|` operator: concurrent fan-out, `ParallelTextAgent`, merging results |
| 06 | `06_loop_agent` | `* N` fixed loop; `* until(predicate)` conditional loop; `LoopTextAgent` |
| 07 | `07_state_transforms` | `S::pick`, `S::rename`, `S::merge`, `S::flatten`, `S::set`, `S::defaults`, `S::drop`, `S::map` |
| 08 | `08_prompt_composition` | `P::role`, `P::task`, `P::constraint`, `P::format`, `P::example`, `P::guidelines`, `P::with_state`, `P::when` |
| 09 | `09_tool_composition` | `T::simple \| T::google_search \| T::code_execution \| T::url_context` — `\|` operator for tools |
| 10 | `10_guards` | `G::rate_limit`, `G::toxicity`, `G::grounded`, `G::hallucination`, `G::llm_judge` — input/output validation |

### Walk (11–20) — Multi-Agent Patterns

Compound agent topologies, evaluation, artifacts, and advanced state management.

| # | Binary | What it covers |
|---|--------|----------------|
| 11 | `11_route_branching` | `RouteTextAgent`, `FnTextAgent`, `RouteRule`, `S::is_true`, `S::eq` — deterministic state-driven routing |
| 12 | `12_fallback_chain` | `/` operator: graceful degradation, `FallbackTextAgent`, primary/secondary chains |
| 13 | `13_review_loop` | Reviewer + writer feedback cycle, `* until(predicate)` convergence, inter-agent state sharing |
| 14 | `14_map_over` | `MapOverTextAgent`: parallel item-level processing, collecting and aggregating results |
| 15 | `15_middleware_stack` | `M::cache`, `M::dedup`, `M::metrics`, `M::fallback_model` — composing middleware with `\|` |
| 16 | `16_context_engineering` | `C::window`, `C::user_only`, `C::model_only`, `C::summarize`, `C::priority`, `C::exclude_tools`, `C::dedup` |
| 17 | `17_evaluation_suite` | `E::suite`, `E::response_match`, `E::contains_match`, `E::trajectory`, `E::safety`, `E::semantic_match`, `E::hallucination`, `E::custom`, `E::persona` |
| 18 | `18_artifacts` | `A::json_output`, `A::text_output`, `A::publish`, `A::save`, `A::load`, `A::version` — artifact I/O schemas |
| 19 | `19_agent_tool` | `TextAgentTool`: wrapping a `TextAgent` as a callable tool; agent-as-tool dispatch |
| 20 | `20_supervised` | Human-in-the-loop approval: `TapTextAgent`, approval callbacks, blocking and resuming pipelines |

### Run (21–30) — Production Patterns

Full-system compositions covering real-world architectures and every SDK capability.

| # | Binary | What it covers |
|---|--------|----------------|
| 21 | `21_full_algebra` | All operators together (`>>`, `\|`, `*`, `/`, `* until`), all six composition namespaces |
| 22 | `22_contract_testing` | Schema validation, JSON contract tests, `A::json_output` with schema enforcement |
| 23 | `23_deep_research` | Multi-source research pipeline with `T::google_search`, synthesis agent, and result merging |
| 24 | `24_customer_support` | Routing, escalation state machine, `RouteTextAgent`, multi-phase support flow |
| 25 | `25_code_review` | Automated code review: linting agent, security agent, summary agent in a `>>` pipeline |
| 26 | `26_dispatch_join` | `DispatchTextAgent` + `JoinTextAgent`: fire-and-forget dispatch with join synchronization |
| 27 | `27_race_timeout` | `RaceTextAgent`: first-to-finish wins; `TimeoutTextAgent`: deadline enforcement |
| 28 | `28_a2a_remote` | Agent-to-agent protocol: remote agent declaration, `T::a2a` tool composition |
| 29 | `29_live_voice` | Full `Live::builder()` API: phases, tools, extraction, watchers, steering, repair, persistence |
| 30 | `30_production_pipeline` | End-to-end production pipeline: telemetry, middleware, evaluation, artifact publishing |

---

## ADK Web UI (`gemini-adk-web`)

The interactive multi-app web UI runs at `http://localhost:3000` and bundles all demo apps into a single server with a shared DevTools panel.

```bash
cargo run -p gemini-adk-web    # http://127.0.0.1:3000
```

For more on the web UI design system, dark/light mode, and DevTools panels, see the [ADK Web UI](./web-ui.md) guide.

### Standalone Examples

These run independently outside of `gemini-adk-web`, each with their own Axum server.

```bash
cargo run -p example-text-chat       # http://127.0.0.1:3001
cargo run -p example-voice-chat      # http://127.0.0.1:3002
cargo run -p example-tool-calling    # http://127.0.0.1:3003
cargo run -p example-transcription   # http://127.0.0.1:3004
```

| Example | Layer | Features |
|---------|-------|----------|
| `text-chat` | L0 | Text-only session, streaming deltas, turn lifecycle |
| `voice-chat` | L0 | Bidirectional audio, input/output transcription, VAD events |
| `tool-calling` | L1 | `TypedTool`, `ToolDispatcher`, `NonBlocking` behavior, `WhenIdle` scheduling |
| `transcription` | L0 | Every Gemini Live config option: VAD, compression, resumption, affective dialog |

### Web UI Apps

#### Crawl

**text-chat** — Minimal text session. `Live::builder().text_only()`, streaming.

**voice-chat** — Native audio. `Modality::Audio`, voice selection, transcription.

**tool-calling** — Three demo tools (`get_weather`, `get_time`, `calculate`). `NonBlocking` + `WhenIdle`.

#### Walk

**all-config** — Configuration playground. Every Gemini Live option exposed as a JSON config:
modality, temperature, Google Search, code execution, session resumption, context compression.

**guardrails** — Policy monitoring with real-time corrective injection. `RegexExtractor`, `.watch()`,
`.instruction_amendment()`. Policies: PII (SSN, credit cards), off-topic, negative sentiment.

**playbook** — 6-phase customer support state machine. `.phase()`, `.transition_with()`, `.greeting()`,
`.with_context()`, `RegexExtractor`, `.watch()`.

#### Run

**support-assistant** — Multi-agent handoff between billing and technical support. 10-phase dual state
machine, `.computed()` derived state, `.watch()` escalation, cross-agent transitions, telemetry.

**call-screening** — Incoming call screening with sentiment analysis and smart routing.
`NonBlocking` tools: `check_contact_list`, `check_calendar`, `take_message`, `transfer_call`, `block_caller`.

**clinic** — HIPAA-aware telehealth appointment scheduling. 8 tools with `NonBlocking` behavior.
Patient intake, department routing, insurance check, appointment booking.

**restaurant** — Reservation assistant with menu context. 6 tools, dietary and occasion tracking.

**debt-collection** — FDCPA-compliant debt collection. `StateKey<T>` typed state, compliance watchers,
identity verification, cease-and-desist handling.

---

## Platform Support

All examples work with both **Google AI** (API key) and **Vertex AI** (project/location).

| Feature | Google AI | Vertex AI |
|---------|-----------|-----------|
| Async tool calling (`NonBlocking`) | ✓ Supported | Stripped automatically |
| Response scheduling (`WhenIdle` / `Silent`) | ✓ Supported | Stripped automatically |
| WebSocket frames | Text | Binary (handled automatically) |
| Thinking config | ✓ Supported | Stripped automatically |

The SDK detects your authentication method and strips unsupported wire fields transparently —
no code changes needed when switching platforms.
