## v0.5.0 — Namespace Parity, Cookbook Examples & Web UI Redesign

A feature-rich release that achieves full namespace parity with the upstream ADK, adds 30 progressive cookbook examples, and ships a redesigned web UI.

### Highlights

**Namespace Parity** — ~70 new methods across all composition namespaces:
- `G::` (Guards): `rate_limit`, `toxicity`, `grounded`, `hallucination`, `llm_judge`
- `T::` (Tools): `agent`, `mcp`, `a2a`, `mock`, `openapi`, `search`, `schema`, `transform`
- `M::` (Middleware): `fallback_model`, `cache`, `dedup`, `metrics`, `agent`/`model` hooks
- `P::` (Prompt): `reorder`, `only`, `without`, `compress`, `adapt`, `scaffolded`, `versioned`
- `C::` (Context): `summarize`, `relevant`, `extract`, `distill`, `priority`, `fit`, `project`
- `S::` (State): `log`, `unflatten`, `zip`, `group_by`, `history`, `validate`, `branch`
- `E::` (Eval): `from_file`, `persona`
- `A::` (Artifacts): `publish`, `save`, `load`, `list`, `delete`, `version`

**30 Cookbook Examples** — Progressive Crawl/Walk/Run learning path:
- Crawl (01–10): Agent basics, tools, callbacks, pipelines, state, prompts
- Walk (11–20): Routing, fallbacks, review loops, middleware, evaluation, artifacts
- Run (21–30): Full algebra, deep research, customer support, voice, production pipelines

**Web UI Redesign** — Modern design system with 80+ CSS tokens, dark/light mode, animated hero, architecture diagrams, cookbook browser, and glassmorphism navigation.

### Crates

| Crate | Version | Install |
|-------|---------|---------|
| [`rs-genai`](https://crates.io/crates/rs-genai) | 0.5.0 | `cargo add rs-genai` |
| [`rs-adk`](https://crates.io/crates/rs-adk) | 0.5.0 | `cargo add rs-adk` |
| [`adk-rs-fluent`](https://crates.io/crates/adk-rs-fluent) | 0.5.0 | `cargo add adk-rs-fluent` |
| [`adk-cli`](https://crates.io/crates/adk-cli) | 0.5.0 | `cargo install adk-cli` |

### Upgrade

Update dependencies from `0.4.0` → `0.5.0`. No breaking API changes.

**Full Changelog**: See [CHANGELOG.md](./CHANGELOG.md)
