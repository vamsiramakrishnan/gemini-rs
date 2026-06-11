# gemini-rs Roadmap

> Status legend: ✅ shipped · 🚧 in progress · 📋 planned · 💭 exploratory
>
> Current release: **0.7.0** (2026-05-31).

> **Strategy:** the full competitive analysis and sequencing live in
> [`docs/plans/2026-06-11-100x-strategy-memo.md`](docs/plans/2026-06-11-100x-strategy-memo.md).
> One line: *Rasa CALM's enforcement guarantees, on a native speech-to-speech
> substrate, with the deterministic testing story nobody has — open source, in
> Rust.* Gemini-native by decision (2026-06-11).

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
  `ToolPolicy`) and `FlowMonitor::explain()`/`why_blocked()`. *(L1, L2 surface
  (`govern_compiled`/`handle.why_blocked()`), and richer checks all landed — see
  Milestone 2.)*
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
4. ✅ **Repair flows first-class** — serializable per-stage `RepairPolicy`
   (`reprompt_after`/`escalate_after`/`escalate_to`); the runtime raises
   `repair:{stage}:reprompt`/`:escalate` signals and escalation routes to a handoff
   stage. *(MVP: turn-based stalling; barge-in/tool-timeout variants are
   follow-ups.)*
5. ✅ **Typed graph macros** — `voice_flow!` generates compile-time-checked
   step/tool/slot name constants (typos fail the build). *(Full declarative DSL
   body — stages/transitions/guards in macro syntax — is a follow-up.)*
6. ✅ **Visual devtools loop** — `adk flow inspect`/`graph`/`simulate` over a
   ConversationSpec JSON (summary / Mermaid / model-free scenario run). *(`replay`/
   `why` over recorded traces are follow-ups; `explain()` already answers "why" in
   library.)*
7. ✅ **Policy overlays as reusable aspects** — `Policy::safety_handoff` (→ a
   terminating `safety` digression), `Policy::redact` (redaction set), and
   `Policy::commit(..).idempotency_key/.compensate_with` (commit governance), all
   serializable + applied via `Conversation::policy(..)`. *(Redaction/idempotency
   runtime enforcement in the logging/dispatch layers is a follow-up; the aspects
   are recorded + surfaced.)*
8. 📋 **Voice timing in the graph** — per-stage filler/reprompt/interrupt/
   endpointing/context-delivery as declarative policy lowering to Live settings.
9. ✅ **NL→flow codegen as a skill** — `.claude/skills/conversation-from-script/`
   drafts a `ConversationSpec` + `Scenario` tests from a script (authoring
   assistant; model drafts, control plane governs). Example JSON validated by an
   integration test.

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
  `FlowExplanation` ("why did the assistant ask that?").
- ✅ **Richer compile checks + L2 compiled-flow surface.** `compile()` also rejects
  unsatisfiable `never…until` guards (`done(step)` on an unknown step) and
  ordering cycles closed by `before(a, b)` edges; `Flow::compile_with_tools(&[..])`
  reports tool names missing from a known registry. L2:
  `Live::govern_compiled`/`observe_compiled` attach a `CompiledFlow` without
  re-validating at connect, and `handle.why_blocked()`/`handle.explain()` snapshot
  the governed monitor's `FlowExplanation` from the live handle.

## Milestone 3 — Reactive substrate `0.9.0`

The pieces for a single deterministic reaction loop already exist; they're
half-wired.

- ✅ **Honest reactor vocabulary.** The dead effect nouns took the "remove" path:
  `EffectPolicy::dedupe_key`/`cancel_scope` and `LiveEffect::TransitionPhase` were
  deleted rather than implemented speculatively, and concurrent effect failures
  are now supervised (surfaced as `LiveEvent::Error`). *(The full unified reaction
  loop — phases/watchers/temporal/repair on one scheduler — remains the 0.9.0
  arc; new scheduler nouns get added when a rule actually needs them.)*
- 💭 **Promote the mutation journal into the substrate.** `State` already records
  `StateMutation` (seq, old/new, origin, ts) with cursors/drain. Have
  watchers/computed/extractors consume cursors instead of re-snapshotting; add an
  optional durable sink. → **Value:** time-travel debugging, deterministic session
  replay, prefix subscriptions, lower CPU, a clean devtools-timeline bridge.
- ✅ **Explicit lane backpressure.** Per-event-class delivery policy
  (`Delivery`/`DeliveryConfig`): `Lossless` default preserves the old byte-for-byte
  behavior; `LossyDropNewest` opt-in for audio/VAD/progress with drop counters.
  Exposed at L2 as `.delivery()`/`.lossy_audio()`.

## Milestone 4 — Make misuse impossible to ship `0.8.x`

Mechanical guardrails so Milestones 1–2 can't regress and the crate stays honest.

- ✅ **Feature diet.** L0 defaults are now `["live", "tls-native"]` — ML VAD and
  the tracing subscriber are opt-in, TLS is selectable (`tls-native`/`tls-rustls`,
  `reqwest` follows), `reqwest` is optional behind the REST features, all
  published crates use targeted tokio features instead of `tokio/full`, and the
  tracing facade (unconditional, tiny) is split from the subscriber machinery
  (`tracing-subscriber` feature).
- ✅ **CI/release ratchet.** `cargo hack check --each-feature` (per-feature
  isolation of the published crates), `cargo deny` (advisories/licenses/sources,
  `deny.toml`), `cargo semver-checks` in the release validate job, feature
  extremes (`--no-default-features`/`--all-features`), publish-with-verification
  (no more `--no-verify`), and `await_holding_lock` enforced. All four style allows are
  burned down: callback shapes have named type aliases, and the deliberate
  exceptions carry targeted `#[allow(lint, reason = "…")]`.
- ✅ **Golden-wire protocol tests.** Checked-in fixtures for setup/server
  messages/tool calls/audio/thinking/transcription parts and the
  Vertex-vs-Google-AI deltas (`tests/golden_wire.rs`; `GOLDEN_BLESS=1` to
  re-bless); `GeminiModel::Custom`/`Voice::Custom` escape hatches covered.
- ✅ **Metadata truth.** Crate READMEs + README license section corrected to MIT
  (matching `LICENSE`); install snippets bumped to `0.7`; MSRV made explicit
  (`rust-version = "1.93"`, badge `1.93+`). *(Remaining: generate crate READMEs
  from one source + test snippets as doctests/trycmd.)*
- ✅ **Macro hardening.** `trybuild` compile-fail UI tests (10 fixtures: bad
  signatures, missing descriptions, invalid derive attributes, plus pass
  anchors), path-aware `Option` detection (std/core paths accepted, lookalikes
  rejected), and a real expansion bug fixed (trailing comma after `Option`
  params). *(Deferred: `insta` schema snapshots
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

## Milestone 6 — The correctness floor `0.8.0` 🚧

The five production concurrency bugs found by audit (2026-06-11), plus API
evolvability. Nothing above this matters if barge-in hangs or snapshots tear.

- 🚧 **Background tools cancelled on disconnect.** `BackgroundToolTracker` is
  never held by `LiveHandle`; orphaned tool tasks can post stale results to a
  dead (or new) control lane.
- 🚧 **Lanes aborted on disconnect.** Fast/control `JoinHandle`s are detached,
  never aborted/awaited — a lane blocked in a slow tool runs forever.
- 🚧 **Atomic persistence.** `FsPersistence::save` writes directly (no
  tmp+rename); a crash mid-write corrupts the snapshot unrecoverably.
- 🚧 **Barge-in beats slow tools.** Inline tool dispatch blocks the control
  lane; an interruption waits for the tool to finish. Cancellation must win.
- 🚧 **Graceful drain + GoAway resume.** Deferred context is dropped on
  disconnect; no `resume`-after-GoAway path exists despite tracked handles.
- 🚧 **`#[non_exhaustive]`** on `SessionEvent`/`LiveEvent`/`GeminiModel`/`Voice`
  — every new Gemini model or server event is a semver break until this lands.
- 🚧 **Hot-path elegance.** Kill the double-parse (string-contains + full serde
  per message), fix the 64-deep control channel that can stall audio under slow
  tools, wire the orphaned `TokenBucket` send backpressure.
- 🚧 **`LiveHandle::stream()`** — `impl Stream<Item = LiveEvent>` so events
  compose with `tokio-stream`; callbacks become sugar.

## Milestone 7 — The determinism spine `0.9.0` 🚧

The keystone: **any session can be replayed deterministically through the real
control plane.** (Verified: Sim already runs real FlowStack/extractor code.)

- 🚧 **`RecordingCodec`** wrapping the `Codec` trait — every wire byte recorded.
- 🚧 **Durable `JournalSink`** — the mutation journal is capped at 1024 entries
  (a 2-hour call loses 98% of history); add a sink trait + file backend.
- 🚧 **Replay harness** — feed a recorded wire log through the real processor;
  diff the mutation journal. `adk record` / `adk replay <session.log>`.
- 📋 **Injectable clock** — `Instant::now()`/`SystemTime::now()`/timeouts leak
  nondeterminism into control flow (sites catalogued in audit).
- 📋 **Recorded LLM/resolver outputs** — tape async resolver results so replay
  never re-executes a model call.
- 📋 Promote the mutation journal: watchers/computed/extractors consume cursors
  (carried over from old Milestone 3).

## Milestone 8 — Conversation CI 📋

The most evidenced bet: every commercial voice-agent tester is LLM-vs-LLM
(τ²-bench: 90% pass@1 → 57% pass^8). Ours is deterministic and free.

- 📋 GitHub-Action conformance suite: `adk flow simulate` over a scenario
  corpus on every PR, `why_blocked()` diffs as review artifacts.
- 📋 Scenario extraction from recorded sessions (incident → regression test).
- 📋 Strict canned-response mode (per-phase enforced template-only output) —
  the zero-hallucination guarantee for regulated deployments.

## Milestone 9 — The funnel 📋

- 📋 **Python bindings** (PyO3) over the Rust core — the Pydantic/Polars play;
  the adoption funnel for the entire Python voice-AI population.
- 📋 Proof artifacts: published reproducible p99 mic-to-model jitter benchmark
  vs LiveKit/Pipecat; time-travel debugger UI (journal × wire log) in the web
  devtools.
- ❌ **OpenAI Realtime L0: deliberately not pursued** (decision 2026-06-11) —
  Gemini-native is the identity; the control plane stays provider-agnostic so
  the option remains open.

## Milestone 10 — Rust-only endgames 💭

- 💭 Single-binary telephony via `rustpbx`/`rsipstack` integration (the
  governed agent brain in the media path).
- 💭 WASM edge governance: compiler + Sim in the browser (authoring/validation)
  and Workers/on-device.
- 💭 On-device turn detection (smart-turn-v3 is BSD-2/8M params/12ms CPU;
  Kyutai STT has semantic VAD; sherpa-onnx has official Rust bindings).

---

## Recently shipped (0.7.0)

- **Governed Agents**: Flow (governed DAG + `FlowMonitor`), Extract (recognizers +
  `#[derive(Extract)]` + async resolvers), Orchestration (`Mode` + `Resolver` +
  provenance), wired into `Live` via `.govern()`/`.observe()`.

See [`CHANGELOG.md`](CHANGELOG.md) for full history.
