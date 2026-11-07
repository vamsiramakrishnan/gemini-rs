# Build speed

What was slow, what changed, and what it measured. Timings are clean builds
(`rm -rf target` first) on the cleanup session's 4-vCPU container, Rust
1.99.0, with the provider keys unset; each row is one run, so treat
differences under about 10 % as noise.

## What changed

| # | Change | Where |
|---|---|---|
| 1 | A push to a PR cancels the previous head's CI and CodeQL runs | `.github/workflows/ci.yml`, `codeql.yml` (`concurrency`) |
| 2 | Dev/test profile: `line-tables-only` debuginfo for workspace code, none for dependencies | root `Cargo.toml` `[profile.dev]` |
| 3 | rustls is the default TLS backend; OpenSSL, `native-tls`, `hyper-tls`, `tokio-native-tls` leave the default graph | `tls-rustls` default on L0, L1, L2 and the facade (D20) |
| 4 | One generation of each dependency family the workspace controls: `sha2`/`sha1` 0.11, `hmac` 0.13, `toml` 0.9 | fluent, server, CLI manifests |
| 5 | `default-members`: a root `cargo check`/`test` builds the six library crates | root `Cargo.toml` |
| 6 | One integration-test binary per crate (`tests/main.rs`) | genai, adk-rs, fluent, memory |
| 7 | The `check` job folded into clippy (sccache was tried and removed; see the notes) | `.github/workflows/ci.yml` |

## Measurements

All rows below were taken on the same container after the same restart, so
the two columns are comparable; an earlier baseline taken before the
restart was 15–30 % faster on every shape and is not used. "After" is two
runs; a third run with only the test consolidation reverted is the control
for change 6.

| Measurement | Before | After (two runs) | Control: after, without change 6 |
|---|---|---|---|
| Clean `cargo check` at the root | 88 s, 866 MB (all 25 members) | 61 s / 60 s, 422 MB (six library crates) | 60 s |
| Clean `cargo check --workspace --all-targets` | 109 s, 1.1 GB | 111 s / 108 s, 678 MB | 107 s |
| Clean `cargo test -p gemini-adk-fluent-rs --no-run` | 130 s, 7.3 GB, 21 binaries | 101 s / 92 s, 1.5 GB, 4 binaries | 96 s, 2.3 GB, 21 binaries |
| Test binaries in `cargo test --workspace` | 131 | 82 | 131 |
| Full `cargo test --workspace` target directory | 25 GB (all features, debuginfo; from STATUS) | 9.8 GB (default features) | not measured |
| Lockfile packages | 700 | 697 | 697 |
| Distinct crates in the normal dependency graph | 367 | 360 | 360 |
| TLS/crypto stacks in a default build | 2 (OpenSSL + native-tls, and rustls + ring + aws-lc-rs) | 1 (rustls) | 1 |
| Duplicate-version families from workspace-controlled dependencies | `sha2`, `sha1`, `digest`, `block-buffer`, `toml_edit`, `winnow`, `toml_datetime` | none | none |
| CI jobs per push | 21 (31 check runs) | 20; a superseded head's run is cancelled | same |

What the numbers say:

- **The root `cargo check` is a third faster** (88 s to 60 s) and its target
  half the size, from `default-members` alone: the examples, servers and
  CLI no longer build on every save.
- **`cargo check --workspace --all-targets` is unchanged in time** (109 s
  to about 109 s, within run-to-run noise) with a 38 % smaller target. This
  shape has no codegen and no link step, so neither the profile nor the
  test consolidation can move it; the smaller target is the dependency
  removals. The control run confirms the consolidation costs it nothing.
- **The fluent test build is a quarter faster** (130 s to about 96 s) and
  its target a fifth of the size (7.3 GB to 1.5 GB). The profile change is
  the time win (the control, which has the profile but not the
  consolidation, is 96 s); the consolidation is time-neutral here and is
  what takes the disk from 2.3 GB to 1.5 GB and the binaries from 21 to 4.
  On a machine with a slower linker than this one's lld, fewer links also
  save time; here they did not.
- **The full suite's target** went from 25 GB to 9.8 GB; the two figures
  are not the same feature set, so read it as an order of magnitude, not a
  ratio.
- Duplicates that remain are pulled by third-party crates (`axum` and
  `rsipstack` on `tokio-tungstenite` 0.29 next to our 0.30;
  `metrics-exporter-prometheus` on `thiserror` 1; `hickory` on `rand`
  0.10; derive crates on `syn` 3). They go when those crates move.
- CI wall-clock is not in the table: it depends on GitHub's runner queue,
  which is what the cancellation fixes.
- sccache with the Actions-cache backend was tried on one run and removed:
  with twenty jobs writing a few hundred entries each at once, the backend
  returned errors on most writes (the Test job: 347 cache errors, 7 % hits,
  on a cold cache) and every job paid the write attempts. rust-cache's
  per-job target directories stay. A shared cache needs a backend without
  that rate limit (S3 or GCS) and is a follow-up.

## Reproduce

```bash
scripts/bench-build.sh after     # the three shapes above, from empty target dirs
cargo tree --workspace -e normal --prefix none | sed 's/ (.*//' | sort -u | wc -l
cargo tree --workspace -e normal -d
```
