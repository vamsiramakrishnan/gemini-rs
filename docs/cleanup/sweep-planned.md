# Planned / partial items sweep

Read-only inventory. Sources read in full: ROADMAP.md, plan.md, CHANGELOG.md, README.md, docs/user-guide/telephony.md, CLAUDE.md, AGENTS.md, GEMINI.md, .cursorrules, .clinerules, .windsurfrules, .cursor/rules/gemini-rs.mdc. No `.claude/` directory exists.

## A. Planned or partial items

Counts: ROADMAP.md 15 active (5 status-tagged + 10 "follow-up/remaining/deferred" notes) plus 2 closed as declined; plan.md 17 unchecked subsections under 6 phases; CHANGELOG.md 19 Unreleased entries plus 5 limitation statements; README.md 5 (1 explicit SIP/SRTP + 4 Boundaries); docs/user-guide/telephony.md 1.

### A1. ROADMAP.md (legend: ✅ shipped, 🚧 in progress, 📋 planned, 💭 exploratory)

Status-tagged:
1. `## Milestone 9 — The funnel` — status **🚧** — heading still in progress although all but one bullet is ✅. Crate: `crates/gemini-adk-py` (shipped part), `session-bench`.
2. Milestone 9, bullet "The same measurement against LiveKit and Pipecat; time-travel debugger UI (journal × wire log) in the web devtools" — status **📋** — two things in one bullet: competitor jitter comparison, and a debugger UI in web devtools (`apps/gemini-adk-web-rs`).
3. `## Milestone 10 — Rust-only endgames` — status **💭** (heading) — heading is exploratory although its first bullet is ✅ shipped.
4. Milestone 10, "WASM edge governance: compiler + Sim in the browser ... and Workers/on-device" — status **💭**. Points at the compiler and `Sim` (crate `gemini-adk-fluent-rs`: `conversation.rs`, `simulation.rs`).
5. Milestone 10, "On-device turn detection (smart-turn-v3 ...; Kyutai STT; sherpa-onnx)" — status **💭**. No crate named.

Partial / follow-up / deferred notes (status words as the source writes them):
6. "Remaining sequence" #1 Hierarchical statecharts — "MVP: depth-1; nested overlays, slot-reopen-on-correction, and `Live`-drives-`FlowStack` are follow-ups". Crate: `gemini-adk-fluent-rs` (`OverlaySpec`, `Live::converse`, `FlowStack`).
7. "Remaining sequence" #4 Repair flows — "MVP: turn-based stalling; barge-in/tool-timeout variants are follow-ups". Crate: `RepairPolicy`, `Conversation::repair`.
8. "Remaining sequence" #5 Typed graph macros — "Full declarative DSL body ... is a follow-up". Crate: `voice_flow!` (`flow_macros.rs`). CHANGELOG line ~1261 repeats it.
9. "Remaining sequence" #6 Visual devtools — "`replay`/`why` over recorded traces are follow-ups". Stale: Milestone 8 says `adk flow replay` and `adk flow why` shipped in 3.0.0. Crate: `tools/gemini-adk-cli-rs`.
10. Milestone 3 "Honest reactor vocabulary" — "The full unified reaction loop ... remains the 0.9.0 arc". Crate: `gemini-adk-rs` reactor.
11. Milestone 4 "Metadata truth" — "Remaining: generate crate READMEs from one source + test snippets as doctests/trycmd".
12. Milestone 4 "Macro hardening" — "Deferred: `insta` schema snapshots for `#[tool]`/`#[derive(Extract)]`". Stale: CHANGELOG 3.0.0 says schema snapshots shipped (`crates/gemini-adk-macros-rs/tests/snapshots`). Crate: `gemini-adk-macros-rs`.
13. Milestone 6 "Graceful drain + GoAway resume" — "manual resume after GoAway possible (no auto-reconnect)". Crate: `gemini-adk-rs` `LiveHandle::resume_handle`.
14. Milestone 6 "Hot-path elegance" — "Control-channel depth lands with the concurrency fixes". Crate: `gemini-adk-rs` live processor.
15. Milestone 5 (inside the ❌ bullet) — "MCP/A2A/OpenAPI/Search tool sources currently error at connect — implement or document". Status words "implement or document". Crate: `gemini-adk-rs` tool sources.

Closed as declined (❌, listed for completeness, not counted above):
- Milestone 5: "Subsume `extract_turns` into the unified `Extract` API" — decided against.
- Milestone 9: "OpenAI Realtime L0: deliberately not pursued" (decision 2026-06-11).

ROADMAP staleness seen while reading (relevant to cleanup):
- Header says "Current release: **3.0.0** (staged for tagging, 2026-09-25)"; CHANGELOG has `[4.0.0] - 2026-10-02` and README installs `4.0.0`.
- `## Milestone 1 ... ✅ shipped (Unreleased)`; the `0.8.0` milestones are long released.
- Milestone 10 first bullet says "SIP registration (digest auth, refresh) and SRTP (SDES, RFC 3711) shipped in 3.0.0", which contradicts the README sentence quoted in A3.
- Milestone 4 "Metadata truth" mentions `rust-version = "1.93"` / badge `1.93+`; `Cargo.toml` and `rust-toolchain.toml` now say 1.99.
- "Recently shipped (0.7.0)" is the last footer section.
- Several milestone headings carry crate versions (`0.8.0`, `0.9.0`, `0.8.x`) that predate 1.0.0.

### A2. plan.md

Title: "gemini-adk-fluent-rs <-> adk-fluent Naming Parity Plan". It has no checkboxes, no TODO/pending markers, and no dates. The whole file is a to-do list: every "Action" cell says "Add". Status words in the source are "Action: Add", "N/A", "Already matches", and "Priority" in section 9. Crate for all items: `crates/gemini-adk-fluent-rs` (L2).

Section 9 "Implementation Order (Priority)" lists 6 phases, none marked done:
- Phase 1 Core naming: `Agent` alias, `.instruct()`, `.describe()`, `Pipeline`/`FanOut`/`Loop` builders, prelude.
- Phase 2 Composition namespaces: `E` (eval), `G` (guards), extend `C`/`P`/`S`/`M`/`T`.
- Phase 3 A2A and patterns: `RemoteAgent`, `A2AServer`, `chain`, `conditional`, `map_reduce`, `a2a_*`.
- Phase 4 Builder enhancements: `.tool()`, `.tools()`, `.guard()`, `.context()`, `.planner()`, `.code_executor()`, `.ask()`, `.eval()`, `.eval_suite()`.
- Phase 5 Testing and runtime: test harness, `MockBackend`, `diagnose`, runner abstractions.
- Phase 6 Examples: update existing, add new.

The 17 subsections that still read as pending:
- 1a `Agent` alias for `AgentBuilder`
- 1b `Pipeline`/`FanOut`/`Loop` structs
- 1c builder methods (`.tool`, `.guard`, `.memory`, `.publish`, `.ask`, `.eval` and so on)
- 2a `compose/eval.rs` (`E::`)
- 2b `compose/guards.rs` (`G::`)
- 2c `C::` extras
- 2d `P::` extras
- 2e `S::` extras (`StateDelta`, `StateReplacement`)
- 2f `M::` extras (scope, logs, A2A middleware)
- 2g `T::` extras (`confirm`, `timeout`, `cached`, `transform`)
- 3b new patterns
- 4 A2A module
- 5 testing additions
- 6 runtime/execution (`App`, `Runner`, `StreamRunner`)
- 7 prelude updates
- 8a update existing examples
- 8b new examples

Observation, not a conclusion: `crates/gemini-adk-fluent-rs/src/compose/eval.rs`, `compose/guards.rs`, `a2a.rs`, `patterns.rs`, `testing.rs` and `operators.rs` exist today, and GEMINI.md already lists `Pipeline`/`FanOut`/`Loop` and `M::` as existing. Much of plan.md may already be done; this was not verified symbol by symbol. `git log` shows plan.md was last touched only by an unrelated dependabot commit.

### A3. README.md

- `## Telephony`: "SIP registration and SRTP are not implemented in the current example path. See the telephony chapter for the current support matrix." Status words: "not implemented". Crate: `crates/gemini-adk-fluent-rs/src/telephony` (feature `sip`), `examples/sip-agent`. See the code findings below.
- `## Boundaries`, four limitation statements:
  - A flow cannot make an external tool idempotent.
  - Offline simulation is not a substitute for a live model/transport test.
  - Generated Rust and serialized session documents depend on matching schema and crate versions.
  - The fast callback path assumes callbacks obey the latency contract.

README has no "planned" or "roadmap" wording.

### A4. CHANGELOG.md

`## [Unreleased]` (lines 8-119) has 19 entries, none planned (all are shipped-but-untagged work):
- Added (12):
  - code mode
  - two sandboxes
  - code mode in skills
  - `evals/code-mode`
  - mid-session `contextUpdate`
  - `examples/context-update-spike`
  - Breaking: `SessionCommand::UpdateContext`
  - skills declare only needed tools
  - typed skill entry points
  - flow offers decision frontier
  - Breaking: `FlowStack::gate_scope`
  - `Client::base_url`
- Changed (1): Breaking, minimum Rust 1.99.
- Fixed (6):
  - phase-refused call opens digression
  - failed `contextUpdate` resend
  - skill name length 58
  - stub guard panic
  - CI lockfile / `deny.toml`
  - code execution at setup

Limitation statements:
- Unreleased: `InProcess` sandbox "cannot bound memory, and `'a' * 10**10` can abort the process" (`MontyPool` mitigates).
- Unreleased: "Gemini 2.5 closes the session over [contextUpdate] (1007), so the codec refuses to send it there".
- Unreleased: Gemini 3.8 Live and 2.5 native audio close the session over `codeExecution`; setup leaves it off.
- 3.0.0 region: Gemini 3.8 setup leaves off `thinkingConfig` (unsupported), line ~626.
- Older: voice_flow "Full declarative DSL body is a follow-up", line ~1261.

### A5. docs/user-guide/telephony.md

- SRTP section: "SRTCP, MKIs and DTLS-SRTP are not supported." SDES-only, so media privacy depends on SIP over TLS.
- The same page documents registration and SRTP as working features, so it disagrees with the README sentence.

### SIP registration and SRTP: what exists in code today

Registration exists, in `crates/gemini-adk-fluent-rs/src/telephony/sip.rs` (module gated by `#[cfg(feature = "sip")]` at `telephony/mod.rs:29-33`):
- `sip.rs:269` doc comment: first REGISTER answers the digest challenge before `register` returns.
- `sip.rs:284` `pub async fn register(&self, account: SipAccount) -> Result<SipRegistration, SipError>`.
- `sip.rs:363` `pub struct SipAccount`; `sip.rs:432` `pub enum RegistrationState`.
- `sip.rs:472` `unregister` (REGISTER with zero lifetime); `sip.rs:338`/`359` `UNREGISTER_TIMEOUT` (5 s).
- `sip.rs:482` `register_once`; `sip.rs:498` parses the granted lifetime from the 2xx.
- Tests in `sip.rs` (`mod tests` at line 942):
  - `registration_refreshes_and_shutdown_stops_waiting_for_calls` (1017; asserts a refresh REGISTER at 1038)
  - `registers_with_digest_auth_and_unregisters` (1068)
  - `a_refused_registration_fails_at_once` (1133)
  - the fake registrar helpers at 964-968
- `.github/workflows/ci.yml:87-88`: step "SIP signaling and registration regressions" runs `cargo test -p gemini-adk-fluent-rs --no-default-features --features sip --lib telephony::sip::tests --locked`.
- CHANGELOG 3.0.0 lines ~371 and ~600 ("SIP registration", `SipAgent::register`) and ROADMAP Milestone 10 both say it shipped in 3.0.0.

SRTP exists:
- `telephony/mod.rs:33` `pub mod srtp;` (feature `sip`).
- `telephony/srtp.rs` is a whole module:
  - `SrtpSuite` (38)
  - `MasterKey` (74)
  - `CryptoAttribute` (135)
  - `SrtpError` (181)
  - `SrtpSession::new`/`protect`/`unprotect` (258/290/312)
- `telephony/sdp.rs:8`, `31`, `33-35`, `72-74` collect the `a=crypto` lines and the secure (`RTP/SAVP`) offer flag; `secure_audio_answer` builds the answer.
- `sip.rs:60` imports the SRTP types; `sip.rs:75` and `89` `SipError::NoCommonCrypto`.
- `sip.rs:578-609` `answer` handles a secure offer: rejects with 488 when no suite is usable, else builds the SRTP pair.
- `sip.rs:725` `SrtpPair`; `sip.rs:817-820` inbound `unprotect`; `sip.rs:902-903` outbound `protect`.
- Test `an_srtp_offer_gets_encrypted_media_both_ways` at `sip.rs:1194`.

`examples/sip-agent` does not use either:
- `examples/sip-agent/src/main.rs:32` `SipAgent::bind(...)`, `:48` `incoming.answer(&session)`.
- No `register`, `REGISTER` or `srtp` hit in `main.rs`, `Cargo.toml` or the README.
- `examples/sip-agent/README.md:22` says "no registration needed".

The README's "not implemented in the current example path" is therefore literally true of `examples/sip-agent`, but the library API ships both. Whether to reword the README is left to the caller.

## B. plan.md vs ROADMAP.md overlap

ROADMAP.md never mentions plan.md, `adk-fluent`, or naming parity. There is no overlap. Distinct topics that exist only in plan.md:
- Naming-parity goal with the upstream Python `adk-fluent` library (`Agent("name")`, `Pipeline`, `FanOut`, `Loop`).
- Alias plan: `Agent = AgentBuilder`, `.instruct()`, `.describe()`, `C::recent`, `C::none`, `T::fn_tool`. This conflicts with AGENTS.md, which says `Agent` is the runtime trait (see C).
- Eval composition namespace `E::` (`suite`, `trajectory`, `response_match`, `safety`, `semantic_match`, `compare`, `EvalSuite`) and `.eval()`/`.eval_suite()`.
- Guard composition namespace `G::` (`pii`, `length`, `budget`, `regex`, `output`, `custom`, `|` composition) and `.guard()`.
- Large method-gap tables for `C::`, `P::`, `S::`, `M::`, `T::`.
- New pattern functions: `chain`, `conditional`, `map_reduce`, `a2a_cascade`/`a2a_fanout`/`a2a_delegate`.
- A2A L2 module: `RemoteAgent`, `A2AServer`, `AgentRegistry`, `SkillDeclaration`, `.skill()`, `.publish(port)`, A2A middleware.
- Testing additions: `infer_data_flow`, `DataFlowSuggestion`, `MockBackend`, `AgentHarness`, `diagnose`.
- Runtime abstractions: `App`, `Runner`/`InMemoryRunner`, `Source`/`Inbox`, `StreamRunner`.
- Builder additions: `.tool/.tools/.context/.planner/.code_executor/.agent_tool/.show/.hide/.memory/.ask/.ask_async`.
- Prelude re-export list (section 7).
- Example renames and new examples aligned to upstream (section 8: operator-composition, context-engineering, state-transforms, guards, a2a, dispatch-join, testing, middleware, map-over, loop-patterns).
- 6-phase implementation order (section 9).

Where ROADMAP.md touches the same area only indirectly: Milestone 5 (eval REST, `extraction.md`), Milestone 9 (Python bindings), and the `Governed Agents` footer. The 0.7.0 footer covers Flow/Extract/Orchestration, not fluent naming.

Merge note: because nothing overlaps, merging means adding a new "Fluent API parity with adk-fluent" milestone to ROADMAP.md. First audit which plan.md items already shipped; `compose/eval.rs`, `guards.rs`, `a2a.rs` and `patterns.rs` exist.

## C. Agent-instruction files

Existing:
- CLAUDE.md
- AGENTS.md
- GEMINI.md
- .cursorrules
- .clinerules
- .windsurfrules
- .cursor/rules/gemini-rs.mdc
- `skills/gemini-voice-workflow/`: not an instruction file; a skill with SKILL.md and references.

Missing: `.claude/` (no directory).

Per-file summaries:
- CLAUDE.md (5 lines):
  - Imports AGENTS.md with `@AGENTS.md`.
  - States that shared operating and API guidance lives in AGENTS.md.
  - Carries nothing else.
- AGENTS.md (58 lines):
  - Current guidance.
  - Layer choice across four crates (`gemini-adk-fluent-rs`, `gemini-adk-rs`, `gemini-genai-rs`, `gemini-memory-rs`).
  - Behavior rules: atomic `State::modify`, strict state access, fast-lane sync, tool admission/confirmation tests, no allow-all confirmation, redaction caveats.
  - Gates: `cargo check/test --lib/fmt --locked`, `just ci`, `just hooks`, docs build via `npm --prefix apps/docs`, and a release flow gated on `just release`.
  - Points to CONTRIBUTING.md and docs/agent-reference.md.
- GEMINI.md (433 lines):
  - Long API cookbook in current naming (`gemini_adk_fluent_rs`, `ModelId::FLASH_LATEST`, `.transcription()`).
  - Covers builders, State, phases, extraction, watchers, S.C.T.P.M.A table, key types per layer, three-lane processor, commands, Best Practices, Common Mistakes, workspace structure.
  - Largely duplicates docs/agent-reference.md.
- .cursorrules and .clinerules (789 bytes each, byte-identical; `diff` reports SAME):
  - Describe a three-crate stack with the old names `rs-genai`/`rs-adk`/`adk-rs-fluent`.
  - Import rules `use adk_rs_fluent::prelude::*`.
  - Patterns: three lanes, state prefixes, phase system, `#![warn(missing_docs)]`, commands `just check/test/docs`.
- .cursor/rules/gemini-rs.mdc (2355 bytes, `alwaysApply: true`, `globs: **/*.rs`):
  - Same old crate names (`rs-genai`/`rs-adk`/`adk-rs-fluent`).
  - Rules: `State::modify`, `StateKey<T>`, prefer `TypedTool`, three-lane limits, phase builder `.done()`/`.initial_phase`.
  - Code-style rules: no dead code, minimize `Arc<Mutex>`.
- .windsurfrules (22494 bytes):
  - An older, stale copy of GEMINI.md (header "gemini-rs", `adk_rs_fluent`/`rs_adk`/`rs_genai`, `apps/adk-web`, `tools/adk-transpiler`).
  - Has an ASCII architecture diagram, `GeminiModel::Gemini2_0Flash`/`Gemini2_0FlashLive`, `.transcription(true, true)`, `.with_tools(... | ...)`, `.build(llm)` without `?`.
  - Adds "Async Function Calling & Scheduling" and "Control Plane Features" sections that GEMINI.md lacks.

Disagreements and one-sided instructions:
1. Crate names and import paths.
   - AGENTS.md and GEMINI.md: crates `gemini-adk-fluent-rs`/`gemini-adk-rs`/`gemini-genai-rs`; "Start application code with the fluent prelude", `use gemini_adk_fluent_rs::prelude::*;`.
   - .cursorrules, .clinerules, .cursor/rules/gemini-rs.mdc, .windsurfrules: `rs-genai (L0)`, `rs-adk (L1)`, `adk-rs-fluent (L2)`, `use adk_rs_fluent::prelude::*;`, `use rs_adk::*;`, `use rs_genai::prelude::*;`. These names do not match the workspace.
2. Number of crates and memory crate.
   - .cursorrules/.clinerules/mdc/.windsurfrules: "Three-crate layered stack".
   - GEMINI.md also says "Three-crate layered stack" and lists three crates.
   - AGENTS.md lists four, adding `crates/gemini-memory-rs` ("Independent contextual memory"). GEMINI.md's workspace tree omits it, and also omits `gemini-adk-macros-rs`, `gemini-adk-py` and others.
3. Validation commands.
   - AGENTS.md: `cargo check --workspace --locked`, `cargo test --workspace --lib --locked`, `cargo fmt --check`, `just hooks`, `just ci`.
   - .cursorrules/.clinerules: "Commands: just check, just test, just docs".
   - GEMINI.md: `cargo build --workspace`, `cargo test --workspace`, `cargo check --workspace` (no `--locked`, no fmt, no clippy).
   - The mdc has no commands.
4. `Agent` naming.
   - AGENTS.md: "`Agent` is the runtime trait; `AgentBuilder` is the fluent builder."
   - GEMINI.md table: `Agent` | "Core trait: `name()` + `run_live()`" (consistent), but its L1 text `TextAgent`/`LlmTextAgent` is not mentioned in AGENTS.md.
   - plan.md (not an instruction file) proposes `pub type Agent = AgentBuilder`, the opposite of AGENTS.md.
5. Tool composition operator.
   - GEMINI.md: `T::` operator is `|` in the namespace table, yet its own examples use `+` (`T::simple(...) + T::google_search()`); the table row reads "`T::` | `\|` | Tool composition".
   - .windsurfrules: `.with_tools(T::simple(...) | T::google_search() | ...)`.
   - GEMINI.md's Live builder uses `.tools(get_weather() + T::google_search())`.
6. Live builder API shape in the stale copy vs GEMINI.md.
   - .windsurfrules: `.model(GeminiModel::Gemini2_0FlashLive)`, `.transcription(true, true)`, `.tools(dispatcher)`, `.build(llm)`.
   - GEMINI.md: no `.model(...)` on Live, `.transcription()` (no args), `.tools(... T:: composite)`, `.build(llm)?`, `ModelId::FLASH_LATEST`.
   - GEMINI.md's "Boolean-setter rule" says a no-argument verb enables a capability; this contradicts the .windsurfrules `.transcription(true, true)`.
7. Default features.
   - GEMINI.md Common Mistakes: `default = ["tls-native", "gemini-llm"]`.
   - ROADMAP Milestone 4: "L0 defaults are now `["live", "tls-native"]`".
   - AGENTS.md does not state defaults.
8. State access semantics.
   - AGENTS.md: "Choose strict state access when a type mismatch must fail."
   - GEMINI.md shows `state.get` (lenient) vs `state.try_get` (strict).
   - .cursor mdc and .cursorrules: only `State::modify`, no strict/lenient distinction.
   - .windsurfrules: `state.get` only.
9. Prefix list.
   - .cursorrules/.clinerules: `session:, derived:, turn:, app:, user:` (no `bg:` or `temp:`).
   - .cursor mdc, GEMINI.md, .windsurfrules: also `bg:` and `temp:`.
10. Instructions only in AGENTS.md (absent elsewhere):
    - Tool admission, confirmation, cancellation and retries need separate tests.
    - "Do not use an allow-all confirmation provider to make a production path pass."
    - "Record/replay only substitutes the transport."
    - "Do not infer a data-retention guarantee from transcript redaction."
    - Docs: edit `docs/src/` and `docs/user-guide/`; don't edit generated website pages or generated agent code; run `npm --prefix apps/docs run build`.
    - Release: run `just release` only when authorized.
11. Instructions only in .cursor/rules/gemini-rs.mdc and .cursorrules/.clinerules (absent from AGENTS.md):
    - Code style: "No speculative dead code", "No `#[allow(dead_code)]` unless there's a documented reason", "Minimize `Arc<Mutex<>>`".
    - `#![warn(missing_docs)]` enforced on all crates (.cursorrules/.clinerules only).
    - Phase rules: "Always end phase builder chains with `.done()`", "Always call `.initial_phase("name")`", "Don't use `instruction_template` with phases" (also in GEMINI.md).
    - "`GeminiLive2_5FlashNativeAudio` only supports `Modality::Audio` output"; Vertex endpoint `aiplatform.googleapis.com` (also in GEMINI.md).
12. Instructions only in .windsurfrules:
    - "Async Function Calling & Scheduling" and "Control Plane Features" sections, with no counterpart in GEMINI.md or AGENTS.md.
13. Tool preference.
    - GEMINI.md and the mdc: prefer `TypedTool` over `SimpleTool`.
    - README and AGENTS.md push `#[tool]` async fns; README's "Tools" section never mentions `TypedTool`.
14. Pointer structure.
    - CLAUDE.md delegates to AGENTS.md; AGENTS.md says "Use the API working reference (docs/agent-reference.md)".
    - GEMINI.md and the other files point at nothing and duplicate content, so they will drift.
15. Rust version/MSRV.
    - README/Cargo.toml/rust-toolchain.toml: 1.99.
    - Instruction files do not state it.
    - ROADMAP Milestone 4 still says 1.93.
16. Directories.
    - `.claude/` is absent, but ROADMAP.md item 9 says "`.claude/skills/conversation-from-script/`", which does not exist. The only skills directory is `skills/gemini-voice-workflow`.

Net: AGENTS.md (via CLAUDE.md) is the only current, maintained file. GEMINI.md is current in API but duplicates agent-reference. .cursorrules, .clinerules, the .cursor mdc and .windsurfrules use the pre-rename crate names. .cursorrules and .clinerules are byte-identical.

## D. CHANGELOG conventions

1. File opens with `# Changelog`, the sentence "All notable changes...", and links to Keep a Changelog 1.1.0 and SemVer 2.0.0. docs/agent-reference.md calls it "Keep a Changelog format".
2. Version headings are `## [X.Y.Z] - YYYY-MM-DD`, newest first (`[4.0.0] - 2026-10-02`, then `[3.0.0] - 2026-09-25`, `[2.0.0]`, `[1.0.0]`, `[0.9.0]`, `[0.8.0]`, `[0.7.0]` ... `[0.1.0] - 2026-03-03`). There are no compare-link footers.
3. An `## [Unreleased]` section exists at the top (line 8), with `### Added`, `### Changed`, `### Fixed`. New entries go there; the latest released heading is `## [4.0.0] - 2026-10-02` (line 120).
4. Subsection names are Keep a Changelog style (`Added`, `Changed`, `Fixed`, `Removed`, `Deprecated`, `Security`), plus ad hoc ones in releases (`Highlights`, `Multi-capability agents`, `Changed (breaking)`) and conventional-commit style sections in old releases (`Features`, `Bug Fixes`, `Documentation`, `Other`, `Chores`).
5. Entry style is a hyphen bullet, hard-wrapped to about 80 columns, with a bold lead phrase and a prose explanation: `- **Bold summary.** Detail with \`code\` names ... See [guide](docs/user-guide/x.md).` Breaking changes are marked `- **Breaking:** ...`, or `- **Breaking: ...**` in Changed; user-guide links are relative paths. Release bodies are stored in the annotated tag, not in a separate file (docs/agent-reference.md).
