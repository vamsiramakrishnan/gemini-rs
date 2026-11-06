# Cleanup status

Read this first when resuming. Starting commit: `255e914` (main, 2026-11-06).
Branch: `vamsi-/practical-einstein-im3bi9`. Phase plan and gates are in the
cleanup prompt; the classified inventory is `inventory.md`.

## Current phase

**Phase 3 — bug bash**, with the Phase 2 doc items landing alongside.
Phase 1 gates pass locally (table below); the CI change is committed
(`74ead81`) and runs on PR #131.

## Gate results on the starting commit (local, debuginfo off, provider keys unset)

| Command | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | pass |
| `cargo test --workspace --all-features --locked` | pass: 3 743 passed, 0 failed, 50 ignored (131 binaries) |
| `cargo hack check --workspace --feature-powerset --depth 2 --locked` | 954 checks; the local run must be repeated once Phase 3's dev-dependencies settle the lockfile (the first run failed on a lockfile change those caused) |
| `cargo deny check` | pass (duplicate-version warnings only) |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked` | pass |

Environment notes that every later run must respect:

- This machine carries a live `GEMINI_API_KEY`. The all-features suite ran
  live memory probes against the provider until it was stopped; every gate
  run since unsets `GEMINI_API_KEY`, `GOOGLE_GENAI_API_KEY`, `GOOGLE_API_KEY`
  and the Vertex variables (`scratchpad/env.sh`). Live tests are reported
  separately, never as part of a gate.
- The disk allowance is about 40 GB; an all-features test build with
  debuginfo is 25 GB. Local builds set `CARGO_PROFILE_DEV_DEBUG=0` and
  `CARGO_PROFILE_TEST_DEBUG=0`; the new `test-all-features` CI job does the
  same.
- `libasound2-dev` is needed for `voice-io` (installed here; CI installs it).

## Open questions for Vamsi

See `inventory.md` §3 "Questions". Until answered: uncertain items take
outcome 2 (keep, documented unstable), nothing is published to PyPI, and no
4.x API break is made without an entry in `decisions.md`.

## Work log

| Item | Commit | Gate | Next |
|---|---|---|---|
| Phase 0 inventory + sweeps | `f22cdd4` | n/a | Phase 1 gates |
| Phase 1: CI runs clippy/tests/hack with all features, rustdoc required | `74ead81` | local gates pass; PR CI pending | Phase 2 |
| F1/F2: README telephony claim corrected; sip-agent registers | `ec3263c` | `cargo test -p example-sip-agent` pass | F5/F7 |
| F5/F7: plan.md → ROADMAP Milestone 11; header fixed | `9fc92f0` | docs | Phase 3 |
| Python binding design doc | `db76a53` | docs | Phase 5 |
| F13/F14: evals and transpiler READMEs, dspbench excluded | `472d8f5` | docs | Phase 3 |
| Bug 2: CLI read errors name the path; cli_help tests | `81e6293` | `cli_help` pass | |
| Bug 1: G.711 encoders bit-exact with the reference; reference tests | `64481c8` | `telephony::g711` pass (21 tests) | |
| DTMF delivery into state (Twilio, RTP, in-band) | `8c23dcf` | `telephony::dtmf_delivery` pass (9 tests) | |
| Bug 3: spec temperature serialization; Studio round trip (Rust + vitest) | `9a917a9` | `studio_round_trip` pass (11), vitest 53 pass | |
| Flow-guard property tests | `c45bcae` | `flow_guard_props` pass (3 × 512 cases) | |
| Bugs 5, 6, F3: codegen keeps conversation docs, multi-line descriptions, custom guard → compile_error | `8043728` | `spec::codegen` pass (14) | |
| Spec property tests + fast-lane benchmark (+ dev-deps, Cargo.lock) | `cb7dec3` | `spec_round_trip_props` pass (4 × 256 cases); bench compiles | |
| Bug 4: on_tool_cancelled on barge-in; lifecycle tests; decision D1 (GoAway) | `249b51a` | `session_lifecycle` pass (7) | |
| Fast-lane budget tests, loom model, fuzz targets + corpus, CI jobs (fuzz, loom, bench-build), docs | pending | clippy/loom/fuzz runs in progress | commit once green |

## Fast-lane diffs needing Fable/Opus review

(none yet)
