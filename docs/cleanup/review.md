# Phase 6 review of the cleanup diff (255e914..HEAD)

Three independent reviews of the full diff, by area, each verified against
the code at HEAD before reporting. Every finding is listed with its
disposition; the fix commits are on the branch. Severity is the reviewer's.

## Rust core: gemini-genai-rs, gemini-adk-rs

| # | File:line | Severity | Finding | Disposition |
|---|---|---|---|---|
| 1 | `crates/gemini-adk-rs/src/live/processor.rs:2317` | major | The wall-clock budget tests (p99 < 1 ms over 2 000 frames, 10× ratio) ran in the ordinary test job alongside every other test process; twenty slow wake-ups fail a correct lane. | Fixed (`d8f301a`): both tests are `#[ignore]`d and run in the `bench-build` job in a release build on their own (`just fast-lane-budget`); the structural test stays in the default run. |
| 2 | `crates/gemini-adk-rs/src/llm/gemini.rs:154` | minor | The dedupe changed `GeminiLlm::from_env` key precedence (`GOOGLE_GENAI_API_KEY` first, empty accepted) to the unified rule (`GEMINI_API_KEY` first, empty ignored) without a record. | Recorded (`d8f301a`): CHANGELOG "Changed", decisions D18. |
| 3 | `crates/gemini-adk-rs/src/session/types.rs:63` | minor | `Session::created_at`/`updated_at` moved from `"{unix}Z"` to RFC 3339 without a record. | Recorded: CHANGELOG, decisions D19. |
| 4 | `crates/gemini-adk-rs/src/live/redaction.rs:145` | minor | `redact_digit_runs` allocated per short digit run and always built an owned string. | Fixed: one pass with `find_iter`, no-match returns the input. |
| 5 | `crates/gemini-adk-rs/src/flow/mod.rs:2306`, `genai .../config.rs:825`, `agent_config.rs:115` | minor | `FlowBuilder` panic→error, `non_empty_env` and `default_agent_type` made `pub` without a decisions entry. | Recorded in D16's additive list. |
| 6 | `crates/gemini-genai-rs/src/transport/replay.rs:188` | nit | `unreachable!` after `pending().await` in library code. | Fixed: `Infallible` match. |
| 7 | `crates/gemini-genai-rs/src/transport/ws.rs:247` | nit | The close-frame warning lost its structured `code` field. | Fixed: `FramePayload::Close` carries `(code, reason)`; the log has `code` again. |
| 8 | `crates/gemini-adk-rs/src/evaluation/response_evaluator.rs:236` | nit | Two tests exercised `strsim` only. | Removed. |
| 9 | `crates/gemini-adk-rs/tests/flow_guard_props.proptest-regressions` | nit | Regression seeds from the test's own development. | Removed. |
| 10 | `crates/gemini-genai-rs/fuzz/.gitignore:4` | nit | The fuzz lockfile was ignored while CI runs the targets. | Lockfile committed. |
| 11 | `crates/gemini-adk-rs/src/live/processor.rs:2225` | nit | Budget tests build `SharedState` by struct literal. | Open (test-only; a `for_tests()` constructor is a follow-up). |

Checked and found fine: the fast lane's non-test code is unchanged; the telemetry ring's ordering (Release store with `latency+1`, Acquire load, skip 0); loom cfg and job; `on_tool_cancelled` dispatch; every `expect` removal; `frame_payload`; the dedupe helpers' equivalence (`interpolate_with`, `strip_code_fence`, `truncate_chars`, `levenshtein`, `MockLlm::echo`, `vertex_flag_enabled`).

## Fluent, memory, macros, server, web, Studio, tools, examples

| # | File:line | Severity | Finding | Disposition |
|---|---|---|---|---|
| 1 | `crates/gemini-adk-fluent-rs/src/telephony/g711.rs:16` | major | The µ-law rewrite transcribed Sun `g711.c` (negation for negatives); ITU-T G.191 takes the one's complement, so the "bit-exact with G.191" claim was false (508 inputs differed) and the reference tests were a second transcription of the same source. | Fixed (`777962b`): encoder follows G.191; the reference module transcribes the four G.191 functions from the ITU source (fetched, not recalled), compares every input and byte, round-trips decoded values, and anchors with literal table pairs; bugs.md #1 and the CHANGELOG corrected. |
| 2 | `crates/gemini-adk-fluent-rs/src/conversation.rs:861` | minor | `spec()`/`into_spec()` silently dropped recorded builder misuse that only `compile()` reported. | Fixed: `try_into_spec()` reports it; both lossy forms documented; test added. |
| 3 | `apps/gemini-adk-server-rs/src/types.rs:146` | minor | Server wire timestamps changed format without a record. | Recorded: D19, CHANGELOG. |
| 4 | `spec/mod.rs:2945`, server `lib.rs:24`, `execution.rs:43` | minor | New public items without a decisions entry. | Recorded in D16. |
| 5 | `apps/gemini-adk-web-rs/src/apps/mod.rs:129`, `tools/.../web.rs:140`, `doctor.rs:35` | minor | Env-rule changes hidden in the dedupe (key precedence, `vertex_requested` accepts `"1"`, `GOOGLE_PROJECT_ID` fallback dropped). | Recorded: D18, CHANGELOG. |
| 6 | `apps/studio/src/panels/Bundles.tsx:106` | minor | D2 rename incomplete ("Save version"). | Fixed (`48d562a`): "Push bundle" / "Pushed …"; assets rebuilt and synced. |
| 7 | `apps/gemini-adk-web-rs/tests/studio_round_trip.rs:6` | nit | `#![allow]` without a reason. | Fixed. |
| 8 | `tools/gemini-adk-cli-rs/tests/cli_help.rs:219` | nit | Stale "known defect" comment. | Fixed. |
| 9 | `crates/gemini-adk-fluent-rs/tests/session_lifecycle.rs:180` | nit | `event || callback` assertion pinned neither path. | Fixed: both asserted. |
| 10 | `crates/gemini-adk-fluent-rs/src/telephony/dtmf_delivery_tests.rs:91` | nit | The RTP leg re-implements the `sip.rs` loop; header overstated "end-to-end". | Header corrected; the SIP loop needs a socket pair, so the leg stays a unit test of its blocks. |

Checked and found fine: SRTP `auth_mac` and the server's `keyed_hmac!` (RFC 2104 key preparation, byte-identical, tested on both sides of the block size); codegen for every gallery document; `serialize_f32_exact`; CLI `--location`/`--region` alias and secret flags; Twilio frames; telemetry filter order; no new `unwrap`/`expect`/`panic` in library code.

## Python binding, CI, bench, docs

| # | File:line | Severity | Finding | Disposition |
|---|---|---|---|---|
| B1 | `crates/gemini-adk-py/src/spec.rs:231` | blocker | `Sim.step` held a std mutex across `block_on` (GIL released) while `snapshot` took the same lock with the GIL held: two threads deadlocked the interpreter (reproduced). | Fixed (`ad1b039`): async mutex locked inside the runtime future; `test_sim_is_safe_to_share_between_threads`. |
| M1 | `src/tools.rs:276`, `src/runtime.rs:56` | major | An async tool plus any blocking entry point on the loop thread hung silently. | Fixed: `block_on` refuses on a running asyncio loop (`asyncio.run`'s rule); `connect_async`/`scripted_async` added; README states it; `test_blocking_entry_point_from_a_running_loop_is_an_error`. |
| M2 | `src/live.rs:82` | major | A dropped `LiveSession` was never disconnected; the design's `dropped_session_disconnects` test was vacuous. | Fixed: `Drop` spawns the disconnect; the test asserts the dropped session's stream ends with `disconnected`. |
| M3 | `src/live.rs:235` | major | `events()` after the session ended waited forever for a `disconnected` event sent before it subscribed. | Fixed: a closed flag (set by close, drop, or a bridge seeing the end) and the handle's phase make `events()` return an ended stream. |
| M4 | module init | major | No `atexit` hook; a pending `__anext__` at interpreter exit panicked a Tokio worker after finalisation (reproduced). | Fixed: `_shutdown` closes every open session, registered with `atexit`; `test_atexit_closes_sessions` runs a subprocess and asserts no panic. |
| M5 | `docs/cleanup/python-binding-design.md` | major | Promised attributes (`.kind`, `.raw`), `async with`, and several named tests were missing with no record. | Fixed: `.kind`/`.reason`/`.raw` and `async with` implemented and tested; design §10 lists what shipped, what differs and what is deferred (`__cause__` chaining and four tests). |
| M6 | `tests/test_agent.py:180`, `:285` | major | Two tautological assertions, one hiding a schema-key mismatch. | Fixed: real assertions on `response_json_schema` and the tool result. |
| m1 | `src/agent.rs:281` | minor | Typed answers accepted any JSON scalar for an object schema. | Fixed: top-level `type` and `required` checked; deeper validation is Pydantic's (`ask_as`). |
| m2 | `python/gemini_adk/__init__.py:113` | minor | `Literal` without `type`, fixed tuples, `Field` descriptions, nested models (`$defs`), `self` methods, partials and lambdas. | Fixed with tests for each shape. |
| m3 | `src/tools.rs:104` | minor | Callable objects with `async def __call__` ran as sync. | Fixed; a sync tool returning an awaitable is reported with a hint. |
| m4 | `src/live.rs:363` | minor | `next_timeout` returned `None` for both timeout and end. | Fixed: raises `TimeoutError` on timeout. |
| m5 | `bench/python/bench.py:134` | minor | The validation row silently measured a fallback document. | Fixed: the gallery restaurant document; RESULTS says so. |
| m6 | `pyproject.toml:64`, README | minor | The `voice` extra was a no-op and the README claim false. | Removed; README reworded. |
| m7 | `src/live.rs:237` | minor | Abandoned bridge tasks were reclaimed only on the next event. | Fixed: `select!` on `tx.closed()`. |
| m8 | `src/tools.rs:294` | minor | The cancel guard took the GIL after every successful async tool call. | Fixed: disarmed on success. |
| m9 | `@tool` docstring | minor | `bytes` results failed obscurely. | Documented: JSON-compatible return values. |
| m10 | `justfile:192` | minor | `test-all-features` recipe ran a check. | Renamed `check-all-features`. |
| n1–n4 | stubs, `typing_extensions`, closed-loop hint, fuzz on nightly | nit | Stub return types; `Self` import; hint on a closed loop; a nightly regression blocks every PR. | Stubs and hint fixed; `Self` imported from `typing` on 3.11+; the fuzz job stays required (the toolchain is nightly by cargo-fuzz's design; pin if it flakes). |

Checked and found fine: the one-runtime design (`OnceLock`, shared with pyo3-async-runtimes, fork guard), the sync tool path, barge-in cancellation, lag accounting, the exception mapping's completeness, stubtest/mypy, Cargo.lock sync, the maturin-action inputs, and the bench rows' descriptions.

## Open after the review

- D1 (GoAway drain vs reconnect), D3/D4–D16 (API shape), D18/D19 (behaviour
  changes made) await Vamsi.
- The PyPI distribution and import name.
- CodeQL on the PR reports alerts whose details the session cannot read
  (no code-scanning access); one high (`web_handlers.rs` log injection)
  was found by inspection and fixed in `3df47e0`; the earlier two are the
  pre-existing #58/#67 explained on the PR.
