## v0.4.0 — ADK Capabilities, Workspace Restructure & CLI

A major additive release that brings the Rust SDK in line with the upstream ADK project structure, adds a full evaluation framework, production session backends, and a CLI tool.

### Highlights

**Workspace Restructure** — `examples/` and `apps/gemini-adk-web/` layout matching upstream ADK conventions.

**`gemini-adk-cli`** — New `adk` binary with `create`, `run`, `web`, `eval`, `deploy`, and `api_server` subcommands for the full agent development lifecycle.

**Evaluation Framework** (`gemini-adk`)
- `HallucinationEvaluator` — detect hallucinated content against ground truth
- `RubricEvaluator` — score responses against grading rubrics
- `SafetyEvaluator` — check output for safety policy violations
- `UserSimulatorEvaluator` — simulate multi-turn user interactions
- `TrajectoryMatchType` — exact, in-order, and any-order tool call sequence matching
- `EvalsetParser` — TOML-based eval set configuration

**Server Apps**
- `gemini-adk-api` — standalone REST API server for headless deployments
- `gemini-adk-server` — shared library for agent loading, handlers, and session management

**Production Features**
- Session persistence: Postgres and Vertex AI backends
- `AgentConfig` with full serde serialization
- Middleware trait and composition pipeline
- Telemetry: structured logging, Prometheus metrics, OpenTelemetry spans
- `AgentBuilder::from_config()` for config-driven construction

**DevTools UI** — New artifact panel, eval panel, event inspector, and trace panel.

### Crates

| Crate | Version | Install |
|-------|---------|---------|
| [`gemini-live`](https://crates.io/crates/gemini-live) | 0.4.0 | `cargo add gemini-live` |
| [`gemini-adk`](https://crates.io/crates/gemini-adk) | 0.4.0 | `cargo add gemini-adk` |
| [`gemini-adk-fluent`](https://crates.io/crates/gemini-adk-fluent) | 0.4.0 | `cargo add gemini-adk-fluent` |
| [`gemini-adk-cli`](https://crates.io/crates/gemini-adk-cli) | 0.4.0 | `cargo install gemini-adk-cli` |

### Upgrade

Update dependencies from `0.1.0` → `0.4.0`. No breaking API changes. Example paths now live under `examples/` and `apps/gemini-adk-web/`.

**Full Changelog**: See [CHANGELOG.md](./CHANGELOG.md)
