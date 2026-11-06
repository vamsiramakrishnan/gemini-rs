# Sweep: features, examples, directories, CI

Read-only inventory. Source: root Cargo.toml, member manifests,
`cargo metadata --no-deps --locked`, `.github/workflows/*.yml`, `justfile`.
Toolchain pinned to 1.99.0 in all jobs. `RUSTFLAGS=-D warnings` is set
workflow-wide in ci.yml and release.yml.

## A. Crate graph

`publish`: "yes" = key absent (publishable); "false" = `publish = false`.
Deps column lists path (workspace) deps only, normal + dev + build.

| Package | Path | publish | Version | Workspace deps |
|---|---|---|---|---|
| gemini-genai-rs | crates/gemini-genai-rs | yes | 4.0.0 | none |
| gemini-adk-macros-rs | crates/gemini-adk-macros-rs | yes | 4.0.0 | gemini-adk-rs (dev) |
| gemini-adk-rs | crates/gemini-adk-rs | yes | 4.0.0 | gemini-adk-macros-rs, gemini-genai-rs |
| gemini-adk-fluent-rs | crates/gemini-adk-fluent-rs | yes | 4.0.0 | gemini-adk-rs, gemini-genai-rs |
| gemini-memory-rs | crates/gemini-memory-rs | yes | 4.0.0 | gemini-adk-fluent-rs, gemini-adk-rs, gemini-genai-rs |
| gemini-adk (facade) | crates/gemini-adk | yes | 4.0.0 | gemini-adk-fluent-rs, gemini-memory-rs |
| gemini-adk-server-rs | apps/gemini-adk-server-rs | yes | 4.0.0 | gemini-adk-fluent-rs, gemini-adk-rs, gemini-genai-rs |
| gemini-adk-web-rs | apps/gemini-adk-web-rs | false | 4.0.0 | gemini-adk-fluent-rs, gemini-adk-rs, gemini-adk-server-rs, gemini-genai-rs, gemini-memory-rs |
| gemini-adk-api-rs | apps/gemini-adk-api-rs | false | 4.0.0 | gemini-adk-server-rs |
| gemini-adk-cli-rs | tools/gemini-adk-cli-rs | yes | 4.0.0 | gemini-adk-fluent-rs, gemini-adk-rs, gemini-adk-server-rs, gemini-genai-rs |
| gemini-adk-transpiler-rs | tools/gemini-adk-transpiler-rs | false | 0.1.0 | none |
| example-agents | examples/agents | false | 0.1.0 | gemini-adk-fluent-rs |
| example-quickstart | examples/quickstart | false | 4.0.0 | gemini-adk-fluent-rs |
| example-text-chat | examples/text-chat | false | 0.1.0 | gemini-adk-fluent-rs |
| example-voice-chat | examples/voice-chat | false | 0.1.0 | gemini-adk-fluent-rs |
| example-tool-calling | examples/tool-calling | false | 0.1.0 | gemini-adk-fluent-rs |
| example-transcription | examples/transcription | false | 0.1.0 | gemini-adk-fluent-rs |
| example-cookbook | examples/cookbook | false | 0.1.0 | gemini-adk-fluent-rs, gemini-adk-rs, gemini-genai-rs |
| example-telephony | examples/telephony | false | 0.1.0 | gemini-adk-fluent-rs |
| example-sip-agent | examples/sip-agent | false | 0.1.0 | gemini-adk-fluent-rs (+`sip`) |
| example-audiohook | examples/audiohook | false | 0.1.0 | gemini-adk-fluent-rs |
| example-voice-spec-demo | examples/voice-spec-demo | false | 0.1.0 | gemini-adk-fluent-rs |
| example-redteam-call | examples/redteam-call | false | 4.0.0 | gemini-adk-fluent-rs, gemini-adk-rs, gemini-genai-rs |
| example-session-bench | examples/session-bench | false | 4.0.0 | gemini-adk-rs, gemini-genai-rs |
| example-context-update-spike | examples/context-update-spike | false | 4.0.0 | gemini-adk-fluent-rs, gemini-genai-rs |

25 members. Version is `4.0.0` via `version.workspace` for crates/apps/cli;
examples are a mix of `0.1.0` (hard-coded) and `4.0.0`.

Notes:
- gemini-adk-server-rs, gemini-adk-web-rs, gemini-adk-api-rs: server is
  published, web and api are not.
- release.yml publishes 8 crates: genai, macros, adk, fluent, memory, facade,
  server, cli. Matches the publishable set.

### Excluded from the workspace

| Path | Stated reason |
|---|---|
| evals/turnbench/predictor | none given (comment above it belongs to code-mode) |
| evals/code-mode | needs newer toolchain than workspace (own rust-toolchain.toml) |
| evals/python-vs-rust | load harness, own lockfile, never runs in CI |
| crates/gemini-adk-py | links libpython, built by maturin; kept out of workspace graph/CI gates |

Not listed in `exclude` but outside the workspace: `evals/dspbench` (own
`[workspace]` table, own Cargo.lock). `evals/code-mode` and
`evals/python-vs-rust` also carry their own `[workspace]` table.

## B. Feature flags per crate

Legend: D = in `default`. Descriptions come from the comment above the entry;
"-" means no comment.

### gemini-genai-rs
| Feature | D | Description |
|---|---|---|
| default | - | live + tls-native |
| live | D | Live WebSocket protocol (slim default) |
| tls-native | D | TLS backend; enable exactly one |
| tls-rustls | | TLS backend (rustls) |
| vad | | - |
| vad-wavekat | | ML VAD (wavekat-vad) |
| http | | - (reqwest) |
| generate, embed, files, models, tokens, caches, tunings, batches | | - (REST surfaces, imply http) |
| chats | | - (implies generate) |
| all-apis | | all REST surfaces |
| tracing-support | | DEPRECATED no-op, kept for manifest compat |
| tracing-subscriber | | fmt/EnvFilter subscriber behind `TelemetryConfig::init` |
| metrics | | - (metrics + prometheus exporter) |
| otel-base | | shared OTel deps |
| otel-otlp | | generic OTLP exporter |
| otel-gcp | | Cloud Trace + Monitoring exporters |
| otel | | alias for otel-otlp |

### gemini-adk-rs
| Feature | D | Description |
|---|---|---|
| default | - | tls-native, gemini-llm, code-mode |
| tls-native | D | TLS backend passthrough |
| gemini-llm | D | `GeminiLlm` generates instead of erroring |
| code-mode | D | in-process Monty sandbox |
| code-pool | | Monty worker processes; needs `monty` binary |
| tls-rustls | | alt TLS backend (exclusive with native by design) |
| http | | optional reqwest client |
| ai-gateway | | Jev decision models via Vercel AI Gateway |
| gcs-artifacts | | - |
| tracing-support | | no-op since 2.0 |
| vad-wavekat | | passthrough |
| templates | | Jinja2-syntax TemplateInstruction |
| metrics | | - |
| database-sessions | | sqlx sqlite sessions |
| postgres-sessions | | sqlx postgres |
| vertex-ai-sessions | | - |
| vertex-ai-code-executor | | Vertex managed code execution |
| vertex-ai-rag | | Vertex RAG tool + memory service |
| mcp-http | | MCP over HTTP/SSE |
| otel, otel-otlp, otel-gcp | | passthroughs to genai |

### gemini-adk-fluent-rs
| Feature | D | Description |
|---|---|---|
| default | - | tls-native, gemini-llm, code-mode |
| tls-native, gemini-llm, code-mode | D | as in L1 |
| voice | | bundle: voice-io, denoise, dsp, vad-wavekat |
| full | | bundle: every optional capability (not default) |
| templates | | passthrough |
| http-tools | | HTTP tool bindings in `spec` |
| ai-gateway | | Jev via AI Gateway |
| gcs-store | | `gs://` bundle store |
| voice-io | | mic/speaker duplex; needs libasound2-dev |
| denoise | | RNNoise mic-chain stage |
| dsp | | resampler / FFT (realfft, rubato) |
| sip | | in-process SIP agent |
| tracing-support | | no-op passthrough |
| code-pool | | Monty worker processes |
| vad-wavekat, tls-rustls | | passthroughs |
| otel, otel-otlp, otel-gcp | | passthroughs |

### gemini-memory-rs
| Feature | D | Description |
|---|---|---|
| default | - | fluent |
| fluent | D | one-call install onto L2 `Live` builder |
| gemini-llm | | model-backed extractors over HTTP |

### gemini-adk (facade)
| Feature | D | Description |
|---|---|---|
| default | - | tls-native, gemini-llm, code-mode |
| tls-native, gemini-llm, code-mode | D | forwarded to fluent |
| tls-rustls | | TLS backend, exactly one |
| voice-io | | mic/speaker; needs ALSA |
| denoise, dsp, vad-wavekat | | forwarded |
| voice | | bundle |
| sip | | telephony over SIP |
| code-pool | | worker processes |
| http-tools | | spec-driven HTTP tools |
| templates | | instruction templates |
| otel, otel-otlp, otel-gcp | | OTel export |
| memory | | contextual memory (`gemini_adk::memory`) |
| full | | fluent/full + memory |

Cargo.toml ends with a dangling comment ("Everything except `voice-io` ...
docs.rs") with no entry below it; likely a docs.rs metadata remnant.

### apps/gemini-adk-server-rs
| Feature | D | Description |
|---|---|---|
| otel-otlp | | OTLP trace/metric export from adk-runtime |
| otel-gcp | | Cloud Trace + Monitoring (`ADK_TELEMETRY=gcp`) |

### tools/gemini-adk-cli-rs
| Feature | D | Description |
|---|---|---|
| voice | | mic/speakers for `adk spec run` (needs audio headers) |
| code-pool | | Monty worker processes via `MONTY_BIN` |

### apps/gemini-adk-web-rs
| Feature | D | Description |
|---|---|---|
| default | - | empty |
| otel, otel-otlp, otel-gcp | | passthroughs to genai + adk-rs; no comments |

### apps/gemini-adk-api-rs
No `[features]`.

### CI coverage per feature

Commands considered:
- C1 `check` job: `cargo check --workspace --all-targets --locked` (default)
- C2 `features` job: `cargo check --workspace --no-default-features --locked`
- C3 `features` job: `cargo check --workspace --all-features --all-targets --locked`
- H `hack` job (4 partitions): `cargo hack check --each-feature --no-dev-deps
  -p genai -p adk-rs -p fluent -p memory` (no `--locked`, no `--all-targets`,
  so lib targets only)
- D `deny` job: `--all-features` (metadata/graph only, no compile)
- R docs.yml `build`: `cargo doc --no-deps --workspace --all-features --locked`
- T `test`/`doctests`: default features, plus three targeted step runs below
- Targeted: `dsp` (fluent lib), `sip` (fluent lib), `otel-base` (genai lib),
  `code-pool` (adk-rs lib, `monty-worker` job)
- justfile `features` recipe differs from CI: all-features check there has no
  `--all-targets`; `just ci` includes `test-all-features` to cover that.

| Crate / features | Enabled by |
|---|---|
| genai, adk-rs, fluent, memory: every feature | H (each) and C3, R, D |
| genai default (live, tls-native) | C1, T (default build) |
| fluent `dsp`, `sip` | also doctests job (lib tests) |
| genai `otel-base` | also doctests job (lib test) |
| adk-rs `code-pool` | also `monty-worker` job (lib tests) |
| fluent `sip` | also forced on in C1 and T via example-sip-agent (unification) |
| fluent `http-tools`, `denoise`, `gcs-store` | also forced on in C1/T via gemini-adk-server-rs deps |
| genai `metrics` | also forced on in C1/T via gemini-adk-server-rs deps |
| gemini-adk (facade): all features | C3, R, D only (not in H `-p` list) |
| gemini-adk-server-rs `otel-otlp`, `otel-gcp` | C3, R, D only |
| gemini-adk-web-rs `otel`, `otel-otlp`, `otel-gcp` | C3, R, D only |
| gemini-adk-cli-rs `voice`, `code-pool` | C3, R, D only |
| example-quickstart `voice` | C3, R, D only |
| gemini-adk-api-rs | no features |

Observations:
- Under `--all-features` every feature of every member is turned on, so
  strictly no feature is "never enabled". The weaker facts follow.
- `--all-features` turns on both `tls-native` and `tls-rustls` together;
  docs say they are exclusive. It compiles today, but proves nothing about
  either backend alone beyond what H does for the four hack crates.
- Features of the facade, server, web, cli, quickstart are compiled only
  once: C3 (`check`, not clippy, not test, not isolated).

### Features never enabled by any CI job

No feature is wholly absent: C3 enables all. Features not enabled by any job
other than the single all-features compile (no each-feature, no clippy, no
test run, not isolated):

- gemini-adk: `tls-rustls`, `voice-io`,
  `denoise`, `dsp`, `vad-wavekat`, `voice`, `sip`, `code-pool`, `http-tools`,
  `templates`, `otel`, `otel-otlp`, `otel-gcp`, `memory`, `full`
  (facade is not in the hack `-p` list)
- gemini-adk-server-rs: `otel-otlp`, `otel-gcp`
- gemini-adk-web-rs: `otel`, `otel-otlp`, `otel-gcp`
- gemini-adk-cli-rs: `voice`, `code-pool`
- example-quickstart: `voice` (gates bin `hello-voice`)

Compile-only: no CI job runs tests with non-default features of the four
hack crates except the targeted runs above
(`dsp`, `sip`, `otel-base`, `code-pool`). Untested at runtime (compile only):
`tls-rustls`, `http`, `generate/embed/files/models/tokens/caches/tunings/
batches/chats`, `vad`, `vad-wavekat`, `metrics`(genai/adk), `otel*`,
`database-sessions`, `postgres-sessions`, `vertex-ai-*`, `mcp-http`,
`gcs-artifacts`, `ai-gateway`, `templates`, `voice-io`, `denoise`, `full`.

## C. Examples vs CI

| Package | Binaries | required-features |
|---|---|---|
| example-agents | research-pipeline, weather-agent | none |
| example-quickstart | chat, hello-text, hello-voice, tool, typed (src/bin/*.rs) | hello-voice: `voice` |
| example-text-chat | example-text-chat | none |
| example-voice-chat | example-voice-chat | none |
| example-tool-calling | example-tool-calling | none |
| example-transcription | example-transcription | none |
| example-cookbook | 30 bins: 01-foundations .. 03-composition, 11-27, 28-a2a-remote, 29-live-voice, 30-production-pipeline, 34-40 (gaps at 04-10, 31-33) | none |
| example-telephony | example-telephony | none |
| example-sip-agent | example-sip-agent | none (dep enables fluent `sip`) |
| example-audiohook | example-audiohook | none |
| example-voice-spec-demo | example-voice-spec-demo | none |
| example-redteam-call | example-redteam-call | none |
| example-session-bench | session-bench (+ test `smoke`) | none |
| example-context-update-spike | context-update-spike, context-update-steering | none |

Also in-crate examples (not packages): `gemini-genai-rs` wire_raw_session,
`gemini-adk-rs` runtime_agent, `gemini-adk-fluent-rs` fluent_pipeline,
`gemini-memory-rs` flow_memory_companion / latency_budget / memory_pipeline,
`gemini-adk-cli-rs` scaffold-agent. All are compiled by `--all-targets`.

Compiled by CI:
- All 14 example packages are workspace members, so C1 (`check` job,
  `--all-targets`) compiles every bin with default features.
- Only `example-quickstart --bin hello-voice` needs a non-default feature
  (`voice` -> fluent `voice-io`); it is compiled only by C3 (`features`
  job). C1 skips it (required-features not met). Needs libasound2-dev; the
  job installs it.
- Clippy (`--all-targets`, default features) also skips `hello-voice`.
- Tests: `test` job (nextest) runs example-quickstart tests
  (`agent_test`, `readme_snippets`) and example-session-bench `smoke`.
  No example bin is ever run in CI.
- `cargo check --workspace --no-default-features` (C2) builds examples too.
  Examples set `default-features = true` on fluent, so this does not narrow
  them.

Uncovered / gaps:
- Not workspace members at all: `examples/ai-gateway-jev` (TypeScript, has
  package.json; no CI job, no `npm` step). It is listed in INDEX.md.
- Required feature enabled only by compile-only C3: `hello-voice`.
- Workspace-member examples NOT mentioned in INDEX.md:
  `example-cookbook` (30 bins), `example-voice-spec-demo`,
  quickstart bins `chat`, `tool`, `typed`, and bin `context-update-steering`.
- INDEX.md mentions all others: quickstart (hello-text, hello-voice),
  text-chat, voice-chat, tool-calling, transcription, telephony, sip-agent,
  audiohook, redteam-call, session-bench, context-update-spike, agents
  (weather-agent, research-pipeline), ai-gateway-jev.
- `evals/dspbench` has its own `examples/` dir (out of workspace); not CI.
- INDEX.md also documents a "Multi-App UI Index" (Crawl/Walk/Run); those
  apps live in `gemini-adk-web-rs` (workspace member, built in C1).

## D. Directory purposes and CI coverage

### crates/
| Dir | Purpose | CI |
|---|---|---|
| gemini-genai-rs | L0: Live protocol, auth, transport | all Rust jobs; hack; release publish |
| gemini-adk-macros-rs | proc macros (`#[tool]`, derives) | all Rust jobs; release publish + dry-run |
| gemini-adk-rs | L1: session state, tools, phases, callbacks | all Rust jobs; hack; monty-worker; release |
| gemini-adk-fluent-rs | L2: fluent authoring API | all Rust jobs; hack; doctests (dsp, sip); release |
| gemini-memory-rs | independent contextual memory | all Rust jobs; hack; release |
| gemini-adk | facade re-exporting fluent + memory | check/test/features; release publish (not in hack) |
| gemini-adk-py | Python binding (pyo3, maturin); excluded | `python` job (maturin develop + smoke.py); `deny` matrix `python` |

### apps/
| Dir | Purpose | CI |
|---|---|---|
| docs | Astro/Starlight site, synced from docs/ | docs.yml `build` (npm ci, build, lychee on docs/) |
| gemini-adk-api-rs | headless REST server (`adk api_server`) | check/test/clippy/features |
| gemini-adk-server-rs | shared server core + `adk-runtime` bin | same; release publish |
| gemini-adk-web-rs | web UI (`adk web`); static/ embeds Studio build | same; `studio` job checks static/studio is fresh |
| studio | Flow Studio front end (Vite/TS) | ci.yml `studio` (typecheck, test, build, drift check) |

### examples/
Summary: 14 Rust example packages (all members, all compiled by `check`) plus
`ai-gateway-jev` (TS). Only `session-bench` and `quickstart` have tests.
No job runs any example binary. See section C.

### evals/
| Dir | Purpose | CI |
|---|---|---|
| code-mode | code mode vs function calls on live Gemini | not in CI (excluded; live calls) |
| dspbench | does the mic DSP chain improve VAD decisions | not in CI (own workspace) |
| python-vs-rust | Python vs Rust Live client load bench | not in CI (excluded) |
| turnbench | turn-taking benchmark (py + predictor crate) | not in CI (predictor excluded) |

### skills/
`skills/gemini-voice-workflow` plus README: coding-agent skills (SKILL.md +
references). Not in CI. (docs.yml lychee does not cover skills/.)

### tools/
| Dir | Purpose | CI |
|---|---|---|
| gemini-adk-cli-rs | `adk` CLI (scaffold, run, serve, eval, flow, spec) | all Rust jobs; `conversation-ci` and `generated-projects` run it; release publish |
| gemini-adk-transpiler-rs | transpiles ADK-JS / js-genai / adk-fluent sources to Rust; no README | check/test/clippy only (no tests of its behavior seen in CI) |

### scripts/
| Script | Purpose | CI |
|---|---|---|
| check-generated-projects.sh | codegen Rust/Python/Go projects per gallery spec; build+test | ci.yml `generated-projects` |
| release.sh | release branch, bump, tag, PR (`just release`) | not in CI (run locally; release.yml reacts to tag) |
| run-studio-local.sh | start web apps + Studio from .env | not in CI |
| verify-use-cases.py | verify gallery examples and packaged copies | not in CI |

### deploy/
| Item | Purpose | CI |
|---|---|---|
| Dockerfile | container image | not in CI |
| cloudbuild.yaml | Cloud Build pipeline | not in CI |
| cloudrun/ | service.yaml for Cloud Run | not in CI |
| gke/ | deployment, hpa, ingress, pdb, service, serviceaccount | not in CI |
| terraform/ | main.tf, variables.tf | not in CI (no `terraform validate`) |

### conversations/
Governed conversation specs (booking spec + 2 scenario files). Run by
ci.yml `conversation-ci` (`adk flow ci conversations`); also `just conversations`.

## E. CI summary

### ci.yml (triggers: push/PR to main; permissions contents: read)
- `check`: `cargo check --workspace --all-targets --locked`
- `test`: `cargo nextest run --workspace --locked` (default features; no doctests)
- `doctests`: `cargo test --workspace --doc --locked`; plus lib tests:
  fluent `--no-default-features --features dsp` (`voice::dsp`), fluent
  `sip` (`telephony::sip::tests`), genai `otel-base` (one test)
- `clippy`: `cargo clippy --workspace --all-targets --locked -- -D warnings`
  (default features)
- `fmt`: `cargo fmt --all -- --check`
- `features`: `cargo check --workspace --no-default-features --locked`;
  `cargo check --workspace --all-features --all-targets --locked`
- `hack` (4-way matrix): `cargo hack check --each-feature --no-dev-deps
  --partition N/4` on genai, adk-rs, fluent, memory (no `--locked`)
- `deny` (matrix workspace, python): cargo-deny-action, `--all-features
  --locked --config deny.toml`; python uses crates/gemini-adk-py/Cargo.toml
- `monty-worker`: installs monty-runtime matching lock; `cargo test -p
  gemini-adk-rs --lib --features code-pool code_mode::pool`
- `conversation-ci`: `cargo run -p gemini-adk-cli-rs -- flow ci conversations`
- `generated-projects`: scripts/check-generated-projects.sh (Rust, Python, Go)
- `studio`: npm ci, typecheck, test, build, drift check of committed build
- `python`: maturin develop + tests/smoke.py (excluded crate)
- `ci-ok`: aggregate gate over all 13 jobs above

### docs.yml (push/PR to main + dispatch)
- `build`: lychee offline link check (docs/src, docs/user-guide, README.md);
  `npm ci` + `npm run build` in apps/docs; then
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
  --locked` (read in full: confirmed); merges rustdoc into site; uploads on
  main push.
- `deploy`: GitHub Pages deploy (main push only).
- Not gated by `ci-ok`; separate workflow.

### release.yml (tag push `vX.Y.Z[-pre]`; permissions contents: write)
- `validate`: `cargo fmt --check`; `cargo test --workspace --locked`;
  `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  `cargo publish --dry-run` for genai and macros; cargo-semver-checks on
  genai, adk-rs, fluent.
- `publish`: crates.io in order: genai, macros, adk-rs, fluent, memory,
  adk (facade), server, cli.
- `github-release`: builds body, creates GitHub Release.
- Release `validate` does not run doctests, feature checks, deny, or docs.

### codeql.yml (push/PR to main + weekly cron `43 2 * * 5`)
- `analyze` matrix, build-mode none: actions, javascript-typescript, python,
  rust; Initialize, Analyze, then prints findings.

### Gaps vs target gate set

| Target gate | Today | Gap |
|---|---|---|
| `cargo fmt --all --check` | `fmt` job; release validate | none |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | `clippy` job without `--all-features`; release same | Feature-gated code (sip, voice-io, otel, sqlx, vertex, ai-gateway, mcp-http, templates, hello-voice, cli voice/code-pool) is never linted. Compile only (`RUSTFLAGS=-D warnings`) in C3. |
| `cargo test --workspace --all-features` | `test` (nextest) and `doctests` use default features; all-features is compile-only (`check --all-targets`) | Tests under `#![cfg(feature)]` never run; only dsp, sip, otel-base, code-pool have targeted runs. Needs live/model-free split since `--all-features` pulls `voice-io`, `code-pool` (needs MONTY_BIN) and tls-native+tls-rustls together. |
| `cargo hack check --workspace --feature-powerset --depth 2` | `hack` runs `--each-feature` with `-p` on 4 crates, `--no-dev-deps`, no `--locked`, partitioned 4 ways | Not powerset/depth 2; not `--workspace` (misses gemini-adk facade, server, web, cli, quickstart); pairs of features (e.g. tls-rustls with http, two TLS backends) untested. |
| `cargo deny check` | `deny` job runs with `--all-features --locked --config deny.toml` on workspace and python graphs | none (equivalent or stronger; confirm `deny.toml` checks advisories, bans, licenses, sources by default) |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features` | docs.yml `build` runs exactly this plus `--locked` | none for the command; but it lives in docs.yml, not in `ci-ok` aggregate, so branch protection on "CI" alone does not require it. Not run in release validate. |

Other observations:
- `hack` omits `--locked` by design (`--no-dev-deps` rewrites manifests).
- `just ci` (fmt-check, lint, doc-check, test, test-all-features, features,
  feature-isolation, deny, conversations) differs from CI: its `test` uses
  `cargo test --workspace` (not nextest), its `features` recipe omits
  `--all-targets` on all-features, and it omits studio / python /
  generated-projects / monty-worker / doctests-specific steps.
- `.githooks/pre-push` runs fmt and clippy only.
- Examples, evals, deploy/, skills/, terraform, Dockerfile are never built
  or validated in CI (except examples via `check`/`clippy`).
- `ai-gateway-jev` TypeScript example has no CI step.
