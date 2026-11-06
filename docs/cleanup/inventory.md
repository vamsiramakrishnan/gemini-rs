# Cleanup inventory

Built 2026-11-06 from commit `255e914` on branch `vamsi-/practical-einstein-im3bi9`.
The raw sweeps that feed this document are beside it:
`sweep-markers.md`, `sweep-unwrap.md`, `sweep-features-ci.md`,
`sweep-planned.md`, `sweep-duplicates.md`. This file is the classified
summary; the sweeps hold every file:line.

## 1. Context files, in reading order

### 1.1 CLAUDE.md and AGENTS.md

`CLAUDE.md` is a five-line pointer that imports `AGENTS.md`. `AGENTS.md` is the
operating guidance: layer selection (fluent → adk → genai; memory separate),
behaviour rules (atomic `State::modify`, strict state access, short sync
fast-lane callbacks, separate tests for admission/confirmation/cancellation/
retry, no allow-all confirmation provider, record/replay substitutes only the
transport, redaction is not retention), the model-free validation commands,
docs editing rules and the release recipes.

Conflicts between AGENTS.md and the cleanup prompt, and how they are resolved:

| AGENTS.md says | Prompt says | Resolution |
|---|---|---|
| Validate with `cargo check --workspace --locked`, `cargo test --workspace --lib --locked`, `cargo fmt --check` | Gate is fmt + clippy `--all-features` + test `--all-features` + hack powerset + deny + rustdoc | Prompt wins; the gate is a superset. Every cargo call keeps `--locked` because CI passes it. |
| Use `just ci` for the full matrix | Same list, different composition | `just ci` is extended so the two stay one recipe set (Phase 1). |
| Nothing on `experimental-*` features | Half-done work goes behind `experimental-*` | Prompt wins. |
| Design docs in `docs/plans/` (CONTRIBUTING) | Decisions in `docs/cleanup/decisions.md` | Both: cleanup decisions in `decisions.md`; a design that outlives the cleanup is also linked from `docs/plans/`. |

### 1.2 README.md: the public contract

Each claim below needs a test that proves it. Column 3 names the test that
exists today or `gap`; gaps are closed in Phase 3 (bug bash) or Phase 6.

| # | Claim | Proof today |
|---|---|---|
| R1 | Three library crates: genai (transport), adk (runtime), fluent (authoring) | crate graph (`cargo metadata`); `gemini-memory-rs`, the facade `gemini-adk`, macros, server and CLI are also published — README table is incomplete (see 4.3) |
| R2 | A tool call is rejected when the flow has not admitted it | `crates/gemini-adk-rs/src/flow/*` tests; property test is a Phase 3 gap |
| R3 | The same flow can be serialized, validated, simulated offline, edited in Flow Studio and generated back into Rust | `spec/` tests, `conversation-ci` job, `generated-projects` job; a round-trip *property* test is a Phase 3 gap |
| R4 | Text agents need Rust 1.99+ | `rust-version = "1.99"`, CI toolchain pin |
| R5 | Text path does not pull in the system audio stack | `features` job builds `--no-default-features`; `voice-io` is off by default |
| R6 | `GeminiLlm::from_env()` fails before any request, naming the variable | `protocol/types/config.rs` tests (env-var set/unset) |
| R7 | Quickstart programs compile in CI | `examples/quickstart` is a workspace member; `check --all-targets` |
| R8 | `MockLlm` replies from a script and records every request | `examples/quickstart/tests/agent_test.rs` |
| R9 | `talk()` uses local devices; `voice::pump()` is device-free | `voice::dsp` tests; `pump` unit tests |
| R10 | Flow Studio reads and writes the same session document the runtime consumes; preview runs the offline simulator | `studio` job (typecheck, vitest, build drift); Rust-side load-what-studio-saved test is a Phase 3 gap |
| R11 | Fast lane: sync audio/text/transcript/VAD; control lane: async tools/extraction/phases/turns; telemetry lane: counters off the hot path | `live/processor.rs` tests; no benchmark asserts the budget — Phase 3 gap |
| R12 | Telephony: Twilio, SIP/RTP with G.711 (`sip` feature), AudioHook; audio and DTMF feed the same state | `telephony::sip::tests` (CI step), `telephony/*` unit tests; G.711 reference vectors and DTMF-into-state are Phase 3 gaps |
| R13 | "SIP registration and SRTP are not implemented in the current example path" | **False for the library**: `telephony/sip.rs` has `register`, `SipAccount`, `RegistrationState`; `telephony/srtp.rs` has `SrtpSession::protect/unprotect`; CI runs their tests. True only of `examples/sip-agent`, which binds and answers without registering. README to be corrected (item F1). |
| R14 | `adk create` output compiles in CI | `generated-projects` job compiles the *spec codegen* output; the `adk create` templates are covered only through `tools/gemini-adk-cli-rs` unit tests — Phase 3 verifies every template |
| R15 | Boundaries: flow gating is not idempotency; offline sim is not a live test; generated Rust and documents share one model but need matching versions; fast lane assumes callbacks obey the contract | Documentation statements; R11 benchmark and R3 round trip cover the testable parts |

### 1.3 ROADMAP.md, plan.md, CHANGELOG.md

Full extraction in `sweep-planned.md` §A. Summary:

- `ROADMAP.md`: 15 open items. 5 carry markers (🚧 Milestone 9; 📋 LiveKit/
  Pipecat comparison, time-travel debugger UI; 💭 Milestone 10, WASM edge
  governance, on-device turn detection). 10 are follow-up notes inside shipped
  items. Its header still says "Current release 3.0.0" and MSRV 1.93; both are
  stale (4.0.0, 1.99). Two of its "remaining" notes (`replay`/`why` over
  traces, `insta` snapshots) shipped per CHANGELOG.
- `plan.md`: the "adk-fluent naming parity plan", 6 phases. No overlap with
  ROADMAP. One proposal (`pub type Agent = AgentBuilder`) contradicts
  AGENTS.md, where `Agent` is the runtime trait. Several files it says to add
  already exist (`compose/eval.rs`, `compose/guards.rs`, `a2a.rs`,
  `patterns.rs`).
- `CHANGELOG.md`: Keep a Changelog 1.1.0; `## [Unreleased]` with 12 Added,
  1 Changed, 6 Fixed (shipped, untagged); latest tag `[4.0.0] - 2026-10-02`.
  Five limitation statements (sandbox memory, Gemini 2.5 `contextUpdate`,
  3.8/2.5 `codeExecution`, 3.8 `thinkingConfig`, `voice_flow!` body).

### 1.4 Workspace manifests

Crate graph, feature flags and existing commands: `sweep-features-ci.md` §A–B.

The prompt's crate table is corrected as follows:

| Prompt | Actual |
|---|---|
| 5 crates under `crates/` | 7 under `crates/` (`gemini-genai-rs`, `gemini-adk-macros-rs`, `gemini-adk-rs`, `gemini-adk-fluent-rs`, `gemini-memory-rs`, `gemini-adk` facade, `gemini-adk-py` excluded from the workspace), 3 apps (`gemini-adk-server-rs` published, `gemini-adk-web-rs`, `gemini-adk-api-rs`), 2 tools (`gemini-adk-cli-rs` published, `gemini-adk-transpiler-rs`), 14 example packages |
| `gemini-adk-cli-rs` in `crates/` | lives in `tools/gemini-adk-cli-rs` |
| `gemini-adk-web-rs` in `crates/` | lives in `apps/gemini-adk-web-rs`; the Studio front end is `apps/studio` (Vite/TS) and its build is committed into the web app's `static/` and copied into the CLI's `assets/web/` |
| No Python crate | `crates/gemini-adk-py` exists: pyo3 0.29, abi3-py310, JSON-only surface (`spec_schema`, `validate_spec`, `Conversation`, `Sim`), smoke-tested in the `python` CI job, `publish = false`, package name `gemini-adk`, module `gemini_adk._gemini_adk`. No async, no Live, no tools, no `MockLlm`, no stubs. |

Toolchain: `1.99.0` pinned; edition 2024 (the Python crate is 2021).
`deny.toml`: permissive licences only, advisories deny, wildcards deny.
Workspace lints: `unsafe_code = "warn"` plus `#![forbid(unsafe_code)]` in all
six library crates (adk-rs and genai-rs use `cfg_attr(not(test), …)` because
tests set env vars). `missing_docs` and `unreachable_pub` are warned in the
four main library crates and CI runs with `RUSTFLAGS=-D warnings`, so public
items without docs already fail the build there. The facade and the macro
crate do not warn `missing_docs`.

### 1.5 CI (`.github/workflows/`)

Full job list in `sweep-features-ci.md` §E. What CI runs today:
check (`--all-targets`, default features), test (nextest, default features),
doctests + four targeted feature tests (`dsp`, `sip`, `otel-base`,
`code-pool`), clippy (default features), fmt, feature extremes (`check
--no-default-features` and `check --all-features --all-targets`), cargo-hack
`--each-feature --no-dev-deps` on four crates in 4 partitions, cargo-deny on
the workspace and the Python graph, Monty worker, conversation CI,
generated projects (Rust/Python/Go), Studio (typecheck, test, build drift),
Python binding (maturin develop + smoke), CodeQL, docs (lychee on sources,
Astro build, rustdoc `-D warnings --all-features`), release (fmt, test, clippy,
publish dry-runs, `cargo semver-checks` against the published baseline).

Gaps against the Phase 1 gate:

| Gate | Gap |
|---|---|
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clippy job runs default features only; feature-gated code is never linted in CI (it passes locally today, see Phase 1 log) |
| `cargo test --workspace --all-features` | all-features tests are compiled, not run; only four targeted steps run feature tests |
| `cargo hack check --workspace --feature-powerset --depth 2` | runs `--each-feature` on four crates, not the workspace, no powerset |
| `cargo deny check` | no gap |
| rustdoc `-D warnings --all-features` | runs in `docs.yml`, which is not a `ci-ok` dependency and not in the release validate job |
| examples | all 14 example packages are workspace members, so `check --all-targets` builds them; `hello-voice` needs `voice` and is built only by the all-features check; no job *runs* an example; `examples/ai-gateway-jev` (TypeScript) is outside CI |

### 1.6 Directories

Purpose and CI coverage per directory: `sweep-features-ci.md` §D. Not in CI:
`evals/*` (four evaluation harnesses, three with their own workspaces),
`skills/`, `scripts/release.sh`, `scripts/run-studio-local.sh`,
`scripts/verify-use-cases.py`, `deploy/*`, `examples/ai-gateway-jev`,
`tools/gemini-adk-transpiler-rs` behaviour (compiled and unit-tested, not
exercised end to end).

## 2. Unfinished-work sweep

Counts, with the file:line lists in `sweep-markers.md` and `sweep-unwrap.md`.

| Pattern | Hits | Real items |
|---|---|---|
| `todo!()`, `unimplemented!()`, `panic!("not implemented` | 8 | **0 in code.** 3 are comments; 5 are inside string literals the transpiler and the spec codegen emit (generated `run_live` stubs and `Guard::custom(|_| todo!(…))` for a custom guard). The codegen `todo!` is a Phase 2 item (F3). |
| `TODO`/`FIXME`/`XXX`/`HACK` | 8 | **0 in code.** 1 is an SSN pattern comment; 7 are strings in the transpiler's generated output and its tests. |
| `#[ignore]` | 9 | All carry a reason; 7 need live credentials (kept, per the prompt); 2 in `gemini-memory-rs` need an `Embedder` fixture (Phase 2 item F4). |
| Feature-gated tests CI never runs | 44 sites | 16 `gemini-memory-rs` integration files behind `gemini-llm` (live calls, 7 of them `#[ignore]`-equivalent probes); 28 inline test attributes behind `code-mode`, `gemini-llm`, `vertex-ai-code-executor`, `templates`, `vad-wavekat`, `http-tools`, `gcs-store`, `dsp`, `database-sessions`, `otel-base`, `tls-*`, `fluent`. Phase 1 runs `cargo test --workspace --all-features` so the model-free ones run; live ones must stay behind an env check (item C1). |
| `#[allow(...)]` without reason | 78 of 116 | 17 `apps/gemini-adk-web-rs` (`clinic.rs` dead-code fields, `mod.rs` unused modules), 13 `clippy::type_complexity` in fluent `compose/*` and adk `tool/*`, 12 `tests/common` crate-level `dead_code`, 11 macro `ui/` fixtures, 10 adk `flow/decided.rs`, `tool/schema.rs`, `tool/typed.rs`, `text/mod.rs`, 5 CLI, 5 evals/examples, 1 genai `telemetry`. Phase 4 item D1. |
| `.unwrap()`/`.expect(` in library code | 92 (corrected) | genai 6, adk-rs 43 (the sweep's 217 counted `tasks/tests.rs` and two `cfg(all(test, feature))` modules), fluent 34, memory 4, macros 2, server 3, CLI 0, facade 0. Phase 4 item D2 with the per-site list in `sweep-unwrap.md`. |
| Feature flags no CI job enables | 0 strictly | `--all-features` compiles all; never *linted* or *tested*: facade features, server/web `otel-*`, CLI `voice`/`code-pool`, quickstart `voice`. Phase 1 closes this. |
| Examples CI does not build | 1 | `hello-voice` (built by the all-features check only); `examples/INDEX.md` omits `example-cookbook` (30 bins), `voice-spec-demo`, quickstart `chat`/`tool`/`typed`, `context-update-steering`. Phase 4 item D7. |
| Public items without doc comment | 0 in the four main crates | `missing_docs` is warned and CI denies warnings. The facade and macro crate lack the lint (Phase 4 item D6). |
| Duplicated types/helpers | 14 helper groups, 4 duplicate types + 1 partial, 5 web handlers duplicated between web app and CLI, test fixtures (`MockWriter` ×4, `MockLlm` ×6, `EchoLlm`/`NoopJudge`/`NoopAgent` ×2) | Identical bodies: `now_iso8601` (adk-rs `session/types.rs:115`, server `types.rs:144`; both emit `"{secs}Z"`, which is not RFC 3339), `resolve_voice`, `default_agent_type`, `now_secs`. Same job, different bodies: Levenshtein, code-fence stripping ×3, `truncate` ×3 (CLI copy slices bytes and can panic on non-ASCII), `interpolate`, `build_extraction_llm`, `init_telemetry`, env credential resolution ×8 (vertex truthiness differs: `"true"\|"1"` in genai/adk-rs vs `"true"` only in web/server; `GOOGLE_API_KEY` accepted everywhere but web), PCM16→i16 ×4. Types: `TestConfig` (adk-rs vs CLI, schemas differ), `EvalCase` ×3, `CodeExecutionResult` (genai vs adk-rs), agent definition (`AgentConfig`/`AgentEntry`+`TomlManifest`/`AgentManifest`), `AgentEvent` partial. Phase 4 item D3. |
| `unsafe` | 0 outside tests | 6 crates `forbid(unsafe_code)`; the only `unsafe` blocks set env vars in tests. **`miri` has nothing to check; recorded as not applicable.** |
| Agent-instruction files | 7 files | `CLAUDE.md` → `AGENTS.md` (current); `GEMINI.md` (18 KB cookbook, three-crate story, wrong defaults, `|` vs `+` for tools); `.cursorrules` = `.clinerules` (789 B, crate names `rs-genai`/`rs-adk`/`adk-rs-fluent` that do not exist); `.cursor/rules/gemini-rs.mdc` (same wrong names); `.windsurfrules` (22 KB stale copy of GEMINI.md with a removed Live API). 16 disagreements listed in `sweep-planned.md` §C. Phase 4 item D0. |

## 3. Half-done features: classification

Outcomes: **1** finish (tests + docs), **2** keep behind `experimental-*`
(documented unstable, off by default), **3** delete (CHANGELOG entry).

| Id | Item | Where | Outcome | Reason |
|---|---|---|---|---|
| F1 | README says SIP registration and SRTP are not implemented | README §Telephony; `telephony/sip.rs`, `telephony/srtp.rs` | **1** (docs only) | Both are implemented and CI-tested. Correct the README and the telephony chapter's support matrix to state what the example path does and does not exercise. No code change; the "ask before option 1" condition applies to *building* them, which is not needed. |
| F2 | `examples/sip-agent` neither registers nor uses SRTP | `examples/sip-agent/src/main.rs` | **1** (small) | Add a `--register` path using the existing `register()` API so the example path matches the library. Offline-tested through `SipAccount` config parsing; the live path stays manual. If Vamsi prefers, this can be dropped to a README note. |
| F3 | Spec codegen emits `Guard::custom(\|_\| todo!("custom guard"))` for `Guard::Custom` | `fluent/src/spec/codegen.rs:511` | **1** | Generated code must compile and not panic: emit a named, documented stub function with a `compile_error!`-free body that returns `false` plus a `// implement` comment, or reject `Guard::Custom` at validation with a clear diagnostic. Decision in `decisions.md`. |
| F4 | Two `gemini-memory-rs` tests ignored "needs an Embedder in the fixture" | `tests/memory_paraphrase.rs:253`, `tests/memory_retrieval.rs:304` | **2** | Semantic-layer acceptance tests for work that is not started. Keep ignored with the reason, document in the memory chapter as experimental; not a feature flag because they are tests, not API. |
| F5 | ROADMAP 🚧 Milestone 9 heading and its open notes | ROADMAP.md | **1** (docs) | Audit each note against CHANGELOG, mark shipped ones ✅, keep the rest 📋 with no code. ROADMAP header → 4.0.0, MSRV 1.99. |
| F6 | ROADMAP 📋/💭: LiveKit/Pipecat comparison, time-travel debugger UI, WASM edge governance, on-device turn detection | ROADMAP.md | **3** from the "half-done" list (no code exists) | Nothing to finish or flag; they stay as roadmap entries. Not a deletion of code, so no CHANGELOG entry. |
| F7 | `plan.md` naming-parity plan | plan.md | **3** (move) | Content merges into ROADMAP.md as one 📋 milestone ("adk-fluent naming parity"), minus the `Agent = AgentBuilder` alias that contradicts AGENTS.md; `plan.md` deleted. CHANGELOG entry under Changed (docs). |
| F8 | `GEMINI.md`, `.cursorrules`, `.clinerules`, `.cursor/rules/gemini-rs.mdc`, `.windsurfrules` | repo root | **3** | Stale and wrong crate names. `GEMINI.md` becomes a pointer to AGENTS.md (prompt: keep as pointer). The others are deleted unless a tool requires them; none does (they are read by editors only when present). CHANGELOG entry. |
| F9 | `apps/gemini-adk-web-rs/src/apps/clinic.rs` fields marked `dead_code` ×11, `apps/mod.rs` unused modules ×4 | web app | **1** or **3** per site | Each field is either used (remove the allow) or removed. Phase 4 D1. |
| F10 | `Delivery` drop-oldest variant "left as future work" | `live/processor.rs:70` | **2** (documented) | The doc already says why it is absent. No flag needed; it is a documented non-feature. Phase 5's Python fast-lane channel needs latest-only semantics and will add a bounded ring on the Python side, not here. |
| F11 | Python binding `crates/gemini-adk-py` | crates/gemini-adk-py | **1** (Phase 5) | The JSON-only surface stays as the base; Phase 5 adds the text agent, tools, Live, `MockLlm`, stubs, wheels and the benchmark harness. Name confirmation needed (see questions). |
| F12 | `examples/ai-gateway-jev` (TypeScript) outside CI | examples/ | **2** | Keep; document in `examples/INDEX.md` as not built by CI. Decision for Vamsi whether to delete. |
| F13 | `evals/*` four harnesses outside the workspace | evals/ | **2** | Measurement harnesses with their own toolchains; document as not CI-built in `evals/README.md`. `evals/dspbench` is added to `[workspace].exclude` so the intent is explicit. |
| F14 | `tools/gemini-adk-transpiler-rs` emits `todo!("…::run_live")` and `// TODO: Implement Rust callback type` into generated code | transpiler | **2** | The transpiler generates `generated.rs` for `gemini-adk-rs` and is a maintainer tool; its generated stubs are intentional (the ADK-JS agents it transpiles have no Live path). Documented as such in a `tools/gemini-adk-transpiler-rs/README.md`. |

Items that are **not** half-done and are listed here so they are not re-raised:
no `todo!()` in library code; all `#[ignore]`s have reasons; `missing_docs`
already enforced on the four main crates; `unsafe` absent.

### Questions for Vamsi (answers go into STATUS.md)

1. Confirm the classification above. Without an answer, uncertain items take
   outcome 2 and work continues (prompt §2).
2. F2: add a registration path to `examples/sip-agent`, or only document that
   the example does not register?
3. F12: keep or delete `examples/ai-gateway-jev`?
4. Phase 5: PyPI package name and import name. Today: PyPI `gemini-adk`,
   import `gemini_adk`. Nothing is published until confirmed.
5. Phase 4: any 4.x API break needs sign-off; proposals accumulate in
   `decisions.md`.

## 4. Other findings recorded during the inventory

### 4.1 Fast lane (Fable scope)

`crates/gemini-adk-rs/src/live/processor.rs::run_fast_lane` is a single task
draining a bounded `mpsc` (capacity 512). Per frame it does one
`Acquire` load of the interrupt flag, one callback call, and one
`broadcast::Sender::send` of a `LiveEvent`. The broadcast send takes
tokio's internal tail mutex and clones the payload (`Bytes` refcount for audio,
`String` clone for text). The documented contract is "no locks, no
allocations on the hot path" (CONTRIBUTING); the broadcast send is the one
lock today. This is pre-existing and unchanged; the Phase 3 benchmark
measures it, and a decision on whether to make the broadcast opt-in goes in
`decisions.md`.

The router (`route_event`) redacts transcripts before either lane sees them
(allocation on the router, not the fast consumer) and clones transcript text
once for the control lane. Also pre-existing.

### 4.2 Telemetry lane

`live/telemetry.rs` and `live/session_signals.rs` use atomics with `Relaxed`
loads for counters and a debounced flush. `loom` modelling (Phase 3) targets
the counter/flush pair and `DroppedFrames`.

### 4.3 README crate table

The README's "What it contains" table lists three crates. Published crates
are eight. Phase 6 updates the table.

### 4.4 Error conventions today

- genai: 19 `thiserror` types; `Box<dyn Error>` in four public telemetry fns (`telemetry/mod.rs:150,180,229,282`), one without `Send + Sync`.
- adk-rs: 20 `thiserror` types plus a manual-`Display` `FlowError` (`flow/mod.rs:2137`); `Box<dyn Error>` in `telemetry/setup.rs:117`; about ten public fns return `Result<_, String>`.
- fluent: seven hand-written `Display` + empty `impl Error` (Conversation, Store, Patch, Answer, Twilio, Srtp, Sip) and one `thiserror` (`VoiceIoError`); public `Result<_, String>` fns.
- memory: `MemoryError`, `MemorySlotError` `thiserror`; `YamlError` manual; hand-written `From<io::Error>`/`From<serde_json::Error>`.
- server: `AppError` `thiserror`; `StreamTokenError` has no `Display`/`Error`; `Result<_, String>` and `Box<dyn Error>` in public fns.
- web, api: no error types, `Result<_, String>`.
- CLI: every command returns `Result<_, Box<dyn Error>>`.
- `AgentError` is defined in adk-rs `error.rs:25` and only re-exported by fluent. It has `#[from]` for `SessionError`, `ToolError`, `LlmError`, `StateError` and a manual `From<ConfigError>`. Every other error (all genai errors, `WorkflowError`, `FlowError`, `AgentConfigError`, `McpError`, `ArtifactError`, both memory errors, the seven fluent errors) is stringified into `AgentError::Config(String)` or `Other(String)`. No crate uses `anyhow`.

Phase 4 item D4: one `thiserror` enum per crate, typed `From` conversions into `AgentError` at the fluent layer. Each conversion that changes a public signature is a semver item for `decisions.md`.

### 4.5 Naming across SDK, CLI and Flow Studio

Nineteen mismatches in `sweep-duplicates.md` §4. The ones that change user-facing words (Phase 4 item D5):

| Concept | SDK | CLI | Studio |
|---|---|---|---|
| the document | `SessionSpec` | "spec" (`agent.json`), `flow` commands say "ConversationSpec JSON file" | "agent" ("New agent", "Open JSON") |
| stored versions | `bundle` | `bundle` | "Versions", "Save a version" |
| flow nodes | `Flow` → `steps`; `ConversationSpec` → `stages` | — | both words |
| `overlays` | overlay | — | "Digression" in the panel, "overlay" on the canvas badge |
| `extract` / `watch` / `mcp` / `tests` | those words | — | "Extraction", "Watchers", "MCP servers", "Flow tests"/"Tests" |
| validation | `validate()` | `spec check`, `flow validate`, `spec test` | "Problems" |
| simulation | `simulate` | `flow simulate`, `spec test` | "Step through", "Test a conversation turn" |
| replay | `replay` | `replay`, `run --replay`, `session replay`, `flow replay` | none |
| cloud location | `GOOGLE_CLOUD_LOCATION` / `location` | `deploy --region` | — |
| `confirm_tools` / `commit` | those words | — | chip "confirm before", class `chip-commit` |

Studio hints also mention `spec.inputs`/`spec.outputs` at the top level; the SDK defines them on `SkillSpec`, not `SessionSpec`.

### 4.6 Tooling available in this environment

Installed from prebuilt releases: `cargo-hack`, `cargo-nextest`,
`cargo-semver-checks`. Installing from crates.io: `cargo-deny`,
`cargo-machete`. `cargo-fuzz` needs nightly (not pinned here); the fuzz
targets are written so `cargo +nightly fuzz run` works and CI installs
nightly for the short fuzz job only. `miri` is not applicable (no `unsafe`).
