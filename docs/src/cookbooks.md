# Examples

`gemini-rs` ships **40 runnable cookbook examples** plus interactive web demos.
The cookbook is organized around the **higher-order, governed-agent
capabilities** — the powerful primitives this SDK is built on — with the
composition foundations beneath them.

---

## Governed Agents — the higher-order capabilities

These primitives make an agent *governed*: a declarative control DAG,
deterministic fact extraction, agent orchestration, and the safety/connection
capabilities around them. They compose **multiplicatively** (see the
[Governed Agents synthesis](../plans/2026-05-30-governed-agents-synthesis.md) and
the Flow / Extraction-kit / Orchestration RFCs in `docs/plans/`). **Start here.**

| Capability | Example | What it shows |
|---|---|---|
| **Flow** — governed conversation/tool DAG | `37_governed_flow` | one DAG gates tools, projects postures, enforces order, `once`/`never…until`, mermaid (debt collection) |
| **Extract** — deterministic facts (no LLM) | `38_extraction` | CPU recognizers fill `State` → drive a `Flow` guard with no model in the control loop |
| **Orchestration** — `call`/`dispatch`/`background` | `19_agent_tool`, `26_dispatch_join`, `27_race_timeout` | invoke sub-agents sync/async; results land in `State` |
| **Tool governance** — `confirm`/`timeout`/`cached` | `34_tool_policies` | per-tool policies + `ConfirmationProvider` enforcement; commit-tool safety |
| **Session persistence** | `35_session_persistence` | snapshot/resume + the session store |
| **MCP tools** | `36_mcp_tools` | stdio/SSE MCP integration |
| **Capstones** *(combine all three lenses)* | `39_booking`, `40_screening` | Flow × Extract × Orchestration on real use cases |

```bash
cargo run -p example-cookbook --bin 37-governed-flow
cargo run -p example-cookbook --bin 38-extraction
cargo run -p example-cookbook --bin 34-tool-policies
```

---

## Composition foundations (`examples/cookbook/`)

The building blocks the governed capabilities compose: the builder API, the
S·C·T·P·M·A operator algebra, and the text-agent combinators — a structured
**Crawl → Walk → Run** path. Each example is a self-contained binary with
detailed doc comments.

```bash
cargo run -p example-cookbook --bin 01-foundations
cargo run -p example-cookbook --bin 21-full-algebra
```

### Crawl (01–03) — Foundations

The core builder API and composition primitives, grouped into three combined
examples. No async runtime required — they are construction-only and run without
credentials.

| # | Binary | What it covers |
|---|--------|----------------|
| 01 | `01_foundations` | `AgentBuilder` (model, sampling, thinking, copy-on-write, data contracts); `SimpleTool`/`TypedTool`/`ToolDispatcher` + built-in tools; `M::` callbacks/middleware (model/tool hooks, taps, resilience) |
| 02 | `02_combinators` | `>>` sequential pipelines, `\|` parallel fan-out, `* N` / `* until(pred)` loops; `review_loop`/`fan_out_merge`/`supervised`; `check_contracts` |
| 03 | `03_composition` | The operator algebra: `S::` state transforms (`>>`), `P::` prompt sections (`+`), `T::` tools (`\|`), `G::` output guards (`\|`) |

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
| 28 | `28_a2a_remote` | Agent-to-agent protocol: `RemoteAgent`, `A2aServer`, `A2aRegistry`, remote agents behind local proxy tools |
| 29 | `29_live_voice` | Full `Live::builder()` API: phases, tools, extraction, watchers, steering, repair, persistence |
| 30 | `30_production_pipeline` | End-to-end production pipeline: telemetry, middleware, evaluation, artifact publishing |

### Fly (31–38) — Higher-order capabilities

Connection helpers, callback surfaces, macros, governance, and the
**governed-agent primitives** (see the capability track at the top).

| # | Binary | What it covers |
|---|--------|----------------|
| 34 | `34_tool_policies` | `T::cached`, `T::timeout`, `T::confirm`, nested policies, `ConfirmationProvider` enforcement |
| 35 | `35_session_persistence` | `MemoryPersistence`, `FsPersistence`, `SessionSnapshot` round-trips; `InMemorySessionService` event log |
| 36 | `36_mcp_tools` | `McpConnectionParams::Stdio/Sse`, `McpSessionManager`, `McpToolset`, `T::mcp()`, JSON-RPC 2.0 lifecycle |
| 37 | `37_governed_flow` | **`Flow`** — a governed conversation/tool DAG: gate tools, project postures, `once`/`never…until`, mermaid |
| 38 | `38_extraction` | **`Extract`** — CPU recognizers (`integer`/`money`/`one_of`/`fuzzy`/`yes_no`) fill State and drive a Flow guard, no LLM |
| 39 | `39_booking` | **Capstone** — Flow × Extract × Orchestration on a table-booking scenario |
| 40 | `40_screening` | **Capstone** — governed call screening: spam verdict gates the transfer |

---

## Flow Studio cookbooks (`SessionSpec` as JSON)

Six industry scenarios ship as complete JSON documents in the
[Flow Studio](./flow-studio.md) gallery — mock tools, governance constraints,
and embedded conformance tests that replay through the real flow monitor in
CI, no API key needed:

| Cookbook | Industry | Highlights |
|----------|----------|------------|
| Debt collection | Financial services | compliance gates, `once` payment, declarative extraction |
| Patient intake | Healthcare | conditional emergency edge + `any` join |
| Line support | Telecom | `reset` loop — a re-test reopens the diagnostic |
| Call screening | Front desk | spam-verdict-gated transfer |
| Returns desk | E-commerce | eligibility-gated single refund |
| Table booking | Hospitality | ambient memory, computed state |

The documents live in `apps/gemini-adk-web-rs/static/examples/flows/`; import
them into the Studio or feed them to `SessionSpec` directly.

## Telephony examples

| Example | What it shows |
|---------|---------------|
| `examples/telephony` | A phone agent behind **Twilio Media Streams**: axum TwiML webhook + media WebSocket, G.711 bridged to the voice pump, one Live session per call |
| `examples/sip-agent` | A **carrier-free SIP agent** (feature `sip`): binds UDP 5060, answers INVITEs with SDP + symmetric RTP — dial it from any softphone or PBX |

See the [Telephony guide](./user-guide/telephony.md).

---

## ADK Web UI (`gemini-adk-web-rs`)

The interactive multi-app web UI runs at `http://localhost:25125` and bundles all demo apps into a single server with a shared DevTools panel.

```bash
cargo run -p gemini-adk-web-rs    # http://127.0.0.1:25125
```

For more on the web UI design system, dark/light mode, and DevTools panels, see the [ADK Web UI](./web-ui.md) guide.

### Standalone Examples

These run independently outside of `gemini-adk-web-rs`, each with their own Axum server.

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
