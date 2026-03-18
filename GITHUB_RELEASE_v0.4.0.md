## v0.4.0 — ADK Capabilities, Workspace Restructure & CLI

A major additive release that brings the Rust SDK in line with the upstream ADK project structure, adds a full evaluation framework, production session backends, and a CLI tool.

### Highlights

**Workspace Restructure** — `examples/` and `apps/adk-web/` layout matching upstream ADK conventions.

**`adk-cli`** — New `adk` binary with `create`, `run`, `web`, `eval`, `deploy`, and `api_server` subcommands for the full agent development lifecycle.

**Evaluation Framework** (`rs-adk`)
- `HallucinationEvaluator` — detect hallucinated content against ground truth
- `RubricEvaluator` — score responses against grading rubrics
- `SafetyEvaluator` — check output for safety policy violations
- `UserSimulatorEvaluator` — simulate multi-turn user interactions
- `TrajectoryMatchType` — exact, in-order, and any-order tool call sequence matching
- `EvalsetParser` — TOML-based eval set configuration

**Server Apps**
- `adk-api-server` — standalone REST API server for headless deployments
- `adk-server-core` — shared library for agent loading, handlers, and session management

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
| [`rs-genai`](https://crates.io/crates/rs-genai) | 0.4.0 | `cargo add rs-genai` |
| [`rs-adk`](https://crates.io/crates/rs-adk) | 0.4.0 | `cargo add rs-adk` |
| [`adk-rs-fluent`](https://crates.io/crates/adk-rs-fluent) | 0.4.0 | `cargo add adk-rs-fluent` |
| [`adk-cli`](https://crates.io/crates/adk-cli) | 0.4.0 | `cargo install adk-cli` |

### Upgrade

Update dependencies from `0.1.0` → `0.4.0`. No breaking API changes. Example paths now live under `examples/` and `apps/adk-web/`.

**Full Changelog**: See [CHANGELOG.md](./CHANGELOG.md)
