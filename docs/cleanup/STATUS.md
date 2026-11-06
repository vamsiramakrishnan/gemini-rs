# Cleanup status

Read this first when resuming. Starting commit: `255e914` (main, 2026-11-06).
Branch: `vamsi-/practical-einstein-im3bi9`. Phase plan and gates are in the
cleanup prompt; the classified inventory is `inventory.md`.

## Current phase

**Phase 1 — make the baseline green.** Inventory written (Phase 0 done).

## Gate results on the starting commit

| Command | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | pass |
| `cargo test --workspace --all-features --locked` | first run failed to build: `alsa-sys` needs `libasound2-dev` (environment, not code); re-run in progress |
| `cargo hack check --workspace --feature-powerset --depth 2` | pending |
| `cargo deny check` | pending |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked` | pending |

## Open questions for Vamsi

See `inventory.md` §3 "Questions". Until answered: uncertain items take
outcome 2 (keep, documented unstable), nothing is published to PyPI, and no
4.x API break is made without an entry in `decisions.md`.

## Work log

| Item | Commit | Gate | Next |
|---|---|---|---|
| Phase 0 inventory + sweeps | (this commit) | n/a | Phase 1 gates |

## Fast-lane diffs needing Fable/Opus review

(none yet)
