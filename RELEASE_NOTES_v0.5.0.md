# Release v0.5.0

> **gemini-rs** — A Rust SDK for the Gemini Multimodal Live API

An additive release that achieves full composition namespace parity with the upstream ADK, introduces 30 progressive cookbook examples as a learning path, and ships a completely redesigned web UI with a modern design system.

---

## Highlights

### Namespace Parity (~70 new methods)

All eight composition namespaces now match upstream ADK capabilities:

| Namespace | Operator | New Methods |
|-----------|----------|-------------|
| `G::` Guards | — | `rate_limit`, `toxicity`, `grounded`, `hallucination`, `llm_judge` |
| `T::` Tools | `\|` | `agent`, `mcp`, `a2a`, `mock`, `openapi`, `search`, `schema`, `transform` |
| `M::` Middleware | `\|` | `fallback_model`, `cache`, `dedup`, `metrics`, agent/model hooks |
| `P::` Prompt | `+` | `reorder`, `only`, `without`, `compress`, `adapt`, `scaffolded`, `versioned` |
| `C::` Context | `+` | `summarize`, `relevant`, `extract`, `distill`, `priority`, `fit`, `project` |
| `S::` State | `>>` | `log`, `unflatten`, `zip`, `group_by`, `history`, `validate`, `branch` |
| `E::` Eval | — | `from_file`, `persona` |
| `A::` Artifacts | `+` | `publish`, `save`, `load`, `list`, `delete`, `version`, JSON/text ops |

#### Example: Full namespace composition

```rust
use gemini_adk_fluent::prelude::*;

let agent = AgentBuilder::new("analyst")
    .model(GeminiModel::Gemini2_0Flash)
    .instruction(
        P::role("senior analyst")
            + P::task("Analyze the dataset and produce a report")
            + P::constraint("Use only verified sources")
            + P::format("JSON")
    )
    .state_transform(S::pick(&["dataset", "config"]) >> S::defaults(&[("format", "json")]))
    .context(C::window(10) + C::user_only() + C::exclude_tools())
    .with_tools(
        T::simple("query_db", "Query the database", |args| async move {
            Ok(json!({"rows": 42}))
        })
        | T::google_search()
        | T::code_execution()
    )
    .build(llm);
```

---

### 30 Cookbook Examples

A progressive **Crawl / Walk / Run** learning path covering the full SDK surface area.

#### Crawl (01–10) — Foundations

| # | Example | What You Learn |
|---|---------|----------------|
| 01 | `simple_agent` | Minimal agent with a system instruction |
| 02 | `agent_with_tools` | `SimpleTool` and `TypedTool` registration |
| 03 | `callbacks` | Event callbacks (`on_text`, `on_audio`, lifecycle) |
| 04 | `sequential_pipeline` | `>>` operator for multi-step pipelines |
| 05 | `parallel_fanout` | `\|` operator for concurrent execution |
| 06 | `loop_agent` | `* N` and `* until(predicate)` loops |
| 07 | `state_transforms` | `S::pick`, `S::rename`, `S::merge`, `S::map` |
| 08 | `prompt_composition` | `P::role`, `P::task`, `P::constraint`, `P::format` |
| 09 | `tool_composition` | `T::simple \| T::google_search \| T::code_execution` |
| 10 | `guards` | `G::rate_limit`, `G::toxicity`, input/output validation |

#### Walk (11–20) — Multi-Agent Patterns

| # | Example | What You Learn |
|---|---------|----------------|
| 11 | `route_branching` | Conditional routing with `RouteTextAgent` |
| 12 | `fallback_chain` | `/` operator for graceful degradation |
| 13 | `review_loop` | Reviewer agent with revision cycles |
| 14 | `map_over` | `MapOverTextAgent` for batch processing |
| 15 | `middleware_stack` | `M::cache`, `M::dedup`, `M::metrics` composition |
| 16 | `context_engineering` | `C::window`, `C::summarize`, `C::priority` |
| 17 | `evaluation_suite` | `E::from_file` with rubric and trajectory scoring |
| 18 | `artifacts` | `A::json_output`, `A::publish`, `A::version` |
| 19 | `agent_tool` | Nested agents as callable tools |
| 20 | `supervised` | Human-in-the-loop approval workflows |

#### Run (21–30) — Production Patterns

| # | Example | What You Learn |
|---|---------|----------------|
| 21 | `full_algebra` | All six namespaces composed together |
| 22 | `contract_testing` | Schema validation and contract tests |
| 23 | `deep_research` | Multi-source research with synthesis |
| 24 | `customer_support` | Routing, escalation, and state machines |
| 25 | `code_review` | Automated code analysis pipelines |
| 26 | `dispatch_join` | Async dispatch with join synchronization |
| 27 | `race_timeout` | `RaceTextAgent` and `TimeoutTextAgent` |
| 28 | `a2a_remote` | Agent-to-agent protocol communication |
| 29 | `live_voice` | Live voice session with phases and extraction |
| 30 | `production_pipeline` | Full production pipeline with telemetry |

```bash
# Run any example
cargo run -p example-cookbook --example 01_simple_agent
cargo run -p example-cookbook --example 17_evaluation_suite
cargo run -p example-cookbook --example 30_production_pipeline
```

---

### Web UI Redesign

The `gemini-adk-web` landing page and application shell have been rebuilt from scratch:

- **Design system** — 80+ CSS custom properties for colors, spacing, typography, shadows, and radii. Fonts: `Inter` for UI, `JetBrains Mono` for code.
- **Dark / light mode** — Full theme support with a toggle persisted to `localStorage`. All components respect `[data-theme]` attribute.
- **Landing page** — Animated gradient orbs, three-layer architecture diagram, operator algebra showcase, live stats counters, and a cookbook browser.
- **Cookbook browser** — Filterable example gallery with Crawl/Walk/Run difficulty tiers, syntax-highlighted code previews, and direct links to source files.
- **Glassmorphism navigation** — Frosted-glass nav bar with scroll-aware opacity, backdrop blur, and smooth transitions.
- **DevTools** — New cookbook panel alongside existing state, transcript, metrics, and phase panels.

---

## Crates

| Crate | Version | crates.io |
|-------|---------|-----------|
| `gemini-live` | 0.5.0 | [crates.io/crates/gemini-live](https://crates.io/crates/gemini-live) |
| `gemini-adk` | 0.5.0 | [crates.io/crates/gemini-adk](https://crates.io/crates/gemini-adk) |
| `gemini-adk-fluent` | 0.5.0 | [crates.io/crates/gemini-adk-fluent](https://crates.io/crates/gemini-adk-fluent) |
| `gemini-adk-cli` | 0.5.0 | [crates.io/crates/gemini-adk-cli](https://crates.io/crates/gemini-adk-cli) |

## Install

```bash
# Library (add to Cargo.toml)
cargo add gemini-adk-fluent    # Full fluent DX (recommended)
cargo add gemini-adk            # Runtime only
cargo add gemini-live           # Wire protocol only

# CLI
cargo install gemini-adk-cli
```

## Upgrade Guide

Update your `Cargo.toml` dependencies from `0.4.0` to `0.5.0`. No breaking API changes — this is a purely additive release.

```toml
# Before
gemini-adk-fluent = "0.4.0"

# After
gemini-adk-fluent = "0.5.0"
```

## CI Improvements

- Release workflow now checks crates.io before each publish step and skips crates whose version is already live. This makes tag re-runs and partial failure recovery safe — no more "already exists" errors.

---

## Full Changelog

See [CHANGELOG.md](./CHANGELOG.md) for the complete changelog.
