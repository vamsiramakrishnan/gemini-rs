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

---

## Milestone 1 — State correctness `0.8.0` 🔴 highest value

`State` is the spine everything else reads and writes. Its advertised transactional
guarantees do not currently hold.

- 📋 **Atomic `modify`.** Today `modify()` is `get()` → `f()` → `set()`
  (`state.rs:251`), which races under concurrent writers despite the README calling
  it "atomic read-modify-write." → **Value:** the one method users reach for to
  avoid races currently has one. Build it on `DashMap::entry` (per-key lock).
- 📋 **Delta tracking that can actually roll back.** `delta` is a
  `DashMap<String, Value>` (`state.rs:90`) with no tombstones, so `remove()`
  deletes from the committed `inner` directly (`state.rs:268`) and `clear_prefix()`
  mutates `inner` too (`state.rs:497`) — but `rollback()` is just `delta.clear()`
  (`state.rs:381`). **A rollback after a remove/clear silently cannot restore the
  base state.** → **Value:** transactions that don't roll back are worse than no
  transactions. Fix: `delta: DashMap<String, DeltaOp>` where
  `DeltaOp = Put(Value) | Delete`; reads honor tombstones, `commit()` applies
  puts+deletes, `rollback()` drops the delta.
- 📋 **Fallible setters.** `set()`/`set_committed()` `expect("value must be
  serializable")` (`state.rs:221,235`) — a public SDK path that panics on user
  data. → **Value:** no library call should be able to abort the host process. Add
  `try_set`/`try_modify` returning `Result`; keep `set` ergonomic.
- 📋 **Property tests** for the transaction invariants: concurrent-increment,
  rollback-after-remove, rollback-after-clear-prefix, snapshot/merge under delta.

## Milestone 2 — Flow: compile, don't just build `0.8.0`

Flow is the most valuable product surface — a serializable governed DAG. Two
declared semantics are not actually enforced, and one silently weakens policy.

- 📋 **Enforce `Constraint::Before`.** `before(a,b)` is exposed and validated for
  references (`flow/mod.rs:388`) but never consulted by `eligible()`
  (`flow/mod.rs:650`) or `admits_tool()` (`flow/mod.rs:775`). → **Value:** a
  documented ordering guarantee that does nothing is a trap. Lower it into explicit
  dependency/gate semantics at compile time, or reject it as ambiguous.
- 📋 **Never silently erase a custom guard.** `collect_specs` lowers any
  `Guard::Custom` nested in `all`/`any`/`not` to `Pred::Always`
  (`flow/mod.rs:248`). A composed safety guard *disappears*. → **Value:** this is a
  security-relevant footgun — "I added a guard" becomes "the guard vanished." Make
  it a compile/`compile()` error instead.
- 📋 **`CompiledFlow`.** `Flow::build()` and deserialization feed
  `compile() -> Result<CompiledFlow, FlowErrors>` that rejects the above, surfaces
  unreachable steps, unsatisfiable guards, dangling tool names, unused
  `confirm_tools`, and unguarded commit tools, and precomputes the active-tool
  policy. `FlowMonitor::new` takes only `CompiledFlow`. → **Value:** turns a class
  of runtime surprises into load-time errors.
- 📋 **Rename `flow::Mode`.** It collides with `orchestration::Mode`
  (Call/Dispatch/Background) — both public, used side-by-side, disambiguated only by
  prelude aliases. Rename → `Enforcement` (Enforce/Observe) and reserve `Mode` for
  resolver execution discipline, per the synthesis glossary. → **Value:** removes a
  standing autocomplete/refactor hazard.

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
- 📋 **Metadata truth.** Workspace is MIT but crate READMEs say Apache-2.0;
  install snippets show `0.1`/`0.6` vs a `0.7.0` workspace; README says Rust 1.75+
  while CI pins 1.93.1. Generate crate READMEs from one source, test snippets as
  doctests/trycmd, set an explicit MSRV. → **Value:** stale install snippets and a
  wrong license badge are trust/conversion leaks.
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
