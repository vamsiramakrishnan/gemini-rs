# gemini-rs Roadmap

> Status legend: ✅ shipped · 🚧 in progress · 📋 planned · 💭 exploratory
>
> Current release: **0.7.0** (2026-05-31).

## Thesis: turn the primitives into hard contracts

gemini-rs already has the rare thing most agent SDKs lack — a coherent
control-plane: **State · Flow · Extract · Resolver** over a shared `State`/`Trace`
spine. The highest-value work now is **not another DSL**. It is making those
primitives *impossible to misuse*: correctness, feature-boundary discipline, and
runtime-policy enforcement. The milestones below are ordered by value/effort, with
correctness first.

Each item states the **gap** (what's true in the code today) and the **value**
(why fixing it matters). Line references are to the state at the time of writing.

## North star: the conversation compiler

The ~100× direction (full design in
[`docs/plans/2026-06-06-conversation-compiler-rfc.md`](docs/plans/2026-06-06-conversation-compiler-rfc.md)):
let developers author voice behavior in terms of **slots, confirmations,
interruptions, repairs, digressions, commitments, and handoffs**, and have the
SDK *compile* that into Flow + Extract + Resolver + Reactor + ToolPolicy. **Flow
becomes the bytecode, not the authoring language.** Milestones 1–4 below harden
the substrate the compiler targets; the compiler arc proper is:

- ✅ **Phase 0 — keystone:** `CompiledFlow` (validated IR + `FlowErrors` +
  `ToolPolicy`) and `FlowMonitor::explain()`/`why_blocked()`. *(L1 landed; L2
  surface + richer checks remain — see Milestone 2.)*
- ✅ **Phase 1 — compiler MVP:** landed.
  - serializable `ConversationSpec` + `Conversation` builder lowering
    `stage/say/ground/collect/commit/next/require` → `CompiledFlow` (JSON
    round-trip; `monitor()` yields a `FlowMonitor`).
  - typed frames: `#[derive(Frame)]` + `FrameSpec`/`SlotSpec`/`ConfirmPolicy`/
    `SlotRecognizer` (prompt/reprompt/confirm/state/pii + `#[recognize(..)]`);
    `FrameSpec::to_extract()`; `Conversation::collect_frame::<F>()` lowers both the
    completion guard and the extractor (`CompiledConversation::extractors()`).
  - slot **evidence**: `State::evidence(key)` over the mutation journal +
    `state_meta:` provenance + confidence; deterministic extraction now records
    confidence into `state_meta`, so evidence confidence is real end-to-end.
  - `Live::converse(&convo)` / `converse_observe` — one-liner that governs + registers
    the conversation's extractors.
  - slot **validation**: serializable `SlotValidator` (`Range`/`NonEmpty`/`Regex`/
    `OneOf`) + `#[slot(min=…, max=…, non_empty)]`; invalid recognized values are
    rejected.
  - **resolver-filled slots**: `Conversation::resolve_slot(name, args, ttl, fetch)`
    fills a slot from an async fetch/agent, lowering to an Extract resolver field.

  **Phase 1 is complete.**

### Remaining sequence (reconciled with the original 13-item vision)

Authored as focused, green commits, in this order:

1. ✅ **Hierarchical statecharts + digressions above the DAG** — `OverlaySpec`
   (serializable) + `Conversation::overlay/trigger/resume/done_overlay`, each
   lowering to its own `CompiledFlow`; runtime `FlowStack`
   (`CompiledConversation::stack`) with push-on-trigger / resume-on-completion
   (`Resume::Previous`/`Restart`/`Terminate`); active-layer delegation for
   admission/postures/`explain()`. RFC:
   `docs/plans/2026-06-07-statecharts-digressions-rfc.md`. *(MVP: depth-1; nested
   overlays, slot-reopen-on-correction, and `Live`-drives-`FlowStack` are
   follow-ups — today `Live::converse` registers overlay extractors and governs
   the main flow.)*
2. ✅ **Model-free flow simulation** *(pulled up from #6)* — deterministic `Sim`
   (fake user via recognizers, direct slot set, tool latency) + serializable
   `Scenario`/`SimStep` runner (YAML/JSON, reports failing step). Makes motifs,
   repair, and policy testable without live API.
3. ✅ **Motif stdlib** — `Motif::collect_frame`/`confirm_then_commit`/
   `identity_verification`/`disclosure`/`say`/`handoff` (→ `StageSpec`) +
   `faq_digression` (→ `OverlaySpec`), composed via `Conversation::add_stage`/
   `add_overlay`; all lower through the validated IR (fail-loud).
4. 📋 **Repair flows first-class** — no-input / no-match / low-confidence /
   correction / barge-in / tool-timeout policies as flow-level concepts.
5. 📋 **Typed graph macros** — a `voice_flow!` macro generating typed step/slot/
   tool constants + `build()`.
6. 📋 **Bidirectional visual devtools loop** — `inspect`/`graph`/`simulate`/
   `replay`/`why` CLI over the spec + traces.
7. 📋 **Policy overlays as reusable aspects** — PCI/commit/safety policies that
   lower to tool gates, redaction, confirmation, idempotency, compensation.
8. 📋 **Voice timing in the graph** — per-stage filler/reprompt/interrupt/
   endpointing/context-delivery as declarative policy lowering to Live settings.
9. 📋 **NL→flow codegen as a skill** — a harness (Claude Code skill) that drafts a
   `ConversationSpec` + tests from a call-center script (authoring assistant; the
   model drafts, never governs).

Already shipped from the vision: frames/slots first-class (#2, Phase 1) and
`explain()`/`why_blocked()` (#11, Phase 0).

Locked decisions: the spec is serializable-first (builder is sugar; YAML nearly
free), and higher layers only ever emit lower-layer constructs (no privileged
backdoors).

---

## Milestone 1 — State correctness `0.8.0` ✅ shipped (Unreleased)

`State` is the spine everything else reads and writes; its advertised
transactional guarantees now hold.

- ✅ **Atomic `modify`.** Now a per-key locked read-modify-write
  (`DashMap::entry`); concurrent increments no longer lose updates. Covered by a
  multi-thread regression test.
- ✅ **Delta tracking that can actually roll back.** `delta` is now
  `DashMap<String, DeltaOp>` (`Put`/`Delete` tombstones); `remove()`/`clear_prefix()`
  no longer touch the committed store, so `rollback()` restores the base after
  removals/clears and `commit()` applies removals.
- ✅ **Fallible setters.** `set`/`set_committed`/`set_key`/`modify`/`PrefixedState::set`
  return `Result<_, StateError>` instead of panicking on non-serializable input.
- ✅ **Property + regression tests** for the transaction invariants.

## Milestone 2 — Flow: compile, don't just build `0.8.0`

Flow is the most valuable product surface — a serializable governed DAG. The two
verified correctness bugs are fixed; the full compile-time validator remains.

- ✅ **Enforce `Constraint::Before`.** `before(a, b)` now gates step eligibility
  (`b` cannot start until `a` is done).
- ✅ **Never silently erase a custom guard.** A `Guard::custom` nested in
  `all`/`any` is preserved as a runtime closure (the combinator becomes
  non-serializable, surfacing as a serialize error) instead of lowered to
  `Pred::Always`.
- ✅ **Rename `flow::Mode` → `Enforcement`.** Removes the collision with
  `orchestration::Mode`; deprecated alias kept one release.
- ✅ **`CompiledFlow` (L1).** `Flow::compile() -> Result<CompiledFlow, FlowErrors>`
  reports unreachable steps and effectively-unguarded commit tools on top of
  `validate()`, and precomputes a `ToolPolicy`. `FlowMonitor::compiled`/`try_new`
  construct from it. Plus `FlowMonitor::explain()`/`why_blocked()` → a serializable
  `FlowExplanation` ("why did the assistant ask that?"). *(Remaining: richer
  checks — unsatisfiable guards, dangling tool names vs a tool registry — and L2
  `Live::govern(CompiledFlow)` + `handle.why_blocked()`.)*

## Milestone 3 — Reactive substrate `0.9.0`

The pieces for a single deterministic reaction loop already exist; they're
half-wired.

- 📋 **Finish the reactor effect scheduler.** `EffectPolicy` carries `timeout`,
  `dedupe_key`, and `cancel_scope`, but the executor only honors blocking/concurrent
  + timeout, `LiveEffect::TransitionPhase` is a no-op, and concurrent effect errors
  are fire-and-forget. Honor dedupe/cancel scopes, supervise spawned effects, emit
  structured reaction failures, make `TransitionPhase` real (or unconstructable). →
  **Value:** unifies phases, watchers, temporal patterns, repair, and tool gating
  into one reaction loop instead of policy scattered across callbacks.
- 💭 **Promote the mutation journal into the substrate.** `State` already records
  `StateMutation` (seq, old/new, origin, ts) with cursors/drain. Have
  watchers/computed/extractors consume cursors instead of re-snapshotting; add an
  optional durable sink. → **Value:** time-travel debugging, deterministic session
  replay, prefix subscriptions, lower CPU, a clean devtools-timeline bridge.
- 💭 **Explicit lane backpressure.** Define event classes — lossy/coalesce
  (audio/VAD/progress) vs lossless (tool calls, interruptions, resume, turn
  boundaries) — use `try_send` + counters for lossy, `send().await` for lossless,
  expose lane-saturation metrics. → **Value:** makes the "three-lane" promise
  operational, not just descriptive; correct behavior under voice load.

## Milestone 4 — Make misuse impossible to ship `0.8.x`

Mechanical guardrails so Milestones 1–2 can't regress and the crate stays honest.

- 📋 **Feature diet.** L0 defaults pull ML VAD (`vad-wavekat`), `tokio/full`,
  `tracing-subscriber`, and a default-TLS stack; L1/L2 also use `tokio/full` and
  unconditional `reqwest`/`tracing`. Split: `default = ["live"]`, opt-in
  `vad-wavekat`, `http` behind the REST features, rustls/native-tls mutually
  selectable, tracing facade separate from subscriber. → **Value:** for an SDK,
  default compile weight *is* API surface.
- 📋 **CI/release ratchet.** Add `cargo hack --feature-powerset`,
  `--all-features` + `--no-default-features` tests, `cargo semver-checks`,
  `cargo deny`, and package-from-tarball verification (replace `publish
  --no-verify`). Replace the global `await_holding_lock = allow` with targeted
  `#[allow(reason=…)]`. → **Value:** catches exactly the feature/lock regressions
  multi-crate async SDKs ship.
- 📋 **Golden-wire protocol tests.** JSON fixtures + round-trip serde for setup,
  server messages, tool calls, audio/thinking/transcription parts, and
  Vertex-vs-Google-AI differences; a model/voice catalog with `GeminiModel::Custom`
  escape hatch. → **Value:** the wire layer *will* drift with Gemini releases; make
  staleness obvious instead of silently wrong.
- ✅ **Metadata truth.** Crate READMEs + README license section corrected to MIT
  (matching `LICENSE`); install snippets bumped to `0.7`; MSRV made explicit
  (`rust-version = "1.93"`, badge `1.93+`). *(Remaining: generate crate READMEs
  from one source + test snippets as doctests/trycmd.)*
- 📋 **Macro hardening.** Add `trybuild` compile-fail tests (bad signatures,
  non-serializable args, duplicate/undescribed tools) and `insta` schema snapshots
  for `#[tool]`/`#[derive(Extract)]`. → **Value:** the tool-call surface is too
  important to leave as "JSON plus a closure."

## Milestone 5 — Server & DX polish `0.8.x` (quick wins)

- ✅ Eval REST wired to `gemini_adk_rs::evaluation` (deterministic + LLM-judge
  criteria); `GET /eval/results` served.
- ✅ `GET /debug/trace/:id` serves a real recorded span tree; `POST /run` returns
  `trace_id`.
- 📋 **Real SSE streaming.** `run_agent_sse` returns hardcoded
  `"Streaming response for: …"` (`handlers.rs`). Stream actual agent output. →
  **Value:** a public endpoint currently returns fake data.
- 📋 **`GET /debug/traces` list** — `TraceStore::list()` already exists; wire the
  symmetric endpoint (4 lines).
- 📋 **Input validation** — `get_artifact_version` does `version.parse().unwrap_or(0)`;
  return 400 on malformed input. Add limit/offset to `/eval/results` for parity.
- 📋 Subsume `extract_turns` into the unified `Extract` API (deprecated shim);
  MCP/A2A/OpenAPI/Search tool sources currently error at connect — implement or
  document.
- 📋 `extraction.md` user guide + Extract↔Flow interplay section in `flow.md`.

---

## Recently shipped (0.7.0)

- **Governed Agents**: Flow (governed DAG + `FlowMonitor`), Extract (recognizers +
  `#[derive(Extract)]` + async resolvers), Orchestration (`Mode` + `Resolver` +
  provenance), wired into `Live` via `.govern()`/`.observe()`.

See [`CHANGELOG.md`](CHANGELOG.md) for full history.
