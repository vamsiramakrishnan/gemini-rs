# Documentation Quality & Auto-Publishing Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Bring all three crates to publishable documentation quality with `#![warn(missing_docs)]`, complete public API doc comments, per-crate READMEs, and a GitHub Actions workflow that builds docs on PR and deploys to GitHub Pages on merge to main.

**Architecture:** Three independent crates (gemini-live L0, gemini-adk L1, gemini-adk-fluent L2) form a layered stack. Documentation work is parallelizable across crates. The CI workflow is a single file covering the whole workspace.

**Tech Stack:** Rust (rustdoc), GitHub Actions, GitHub Pages (`actions/deploy-pages@v4`)

**Repo:** https://github.com/vamsiramakrishnan/gemini-rs

---

## Task 1: Workspace & Per-Crate Cargo.toml Metadata

**Files:**
- Modify: `Cargo.toml` (workspace root, line 16-18)
- Modify: `crates/gemini-live/Cargo.toml` (lines 1-8)
- Modify: `crates/gemini-adk/Cargo.toml` (lines 1-7)
- Modify: `crates/gemini-adk-fluent/Cargo.toml` (lines 1-7)

**Step 1: Add repository to workspace Cargo.toml**

In `Cargo.toml`, add to `[workspace.package]`:

```toml
[workspace.package]
edition = "2021"
license = "Apache-2.0"
repository = "https://github.com/vamsiramakrishnan/gemini-rs"
```

**Step 2: Add metadata to gemini-live/Cargo.toml**

Add after the `categories` line:

```toml
repository.workspace = true
documentation = "https://docs.rs/gemini-live"
readme = "README.md"
```

**Step 3: Add metadata to gemini-adk/Cargo.toml**

Add after `description`:

```toml
keywords = ["gemini", "agents", "adk", "tools", "streaming"]
categories = ["api-bindings", "asynchronous", "network-programming"]
repository.workspace = true
documentation = "https://docs.rs/gemini-adk"
readme = "README.md"
```

**Step 4: Add metadata to gemini-adk-fluent/Cargo.toml**

Add after `description`:

```toml
keywords = ["gemini", "agents", "fluent", "builder", "composition"]
categories = ["api-bindings", "asynchronous", "development-tools"]
repository.workspace = true
documentation = "https://docs.rs/gemini-adk-fluent"
readme = "README.md"
```

**Step 5: Verify**

Run: `cargo metadata --no-deps --format-version 1 | python3 -c "import sys,json; pkgs=json.load(sys.stdin)['packages']; [print(f'{p[\"name\"]}: repo={p.get(\"repository\",\"MISSING\")} docs={p.get(\"documentation\",\"MISSING\")} readme={p.get(\"readme\",\"MISSING\")}') for p in pkgs if p['name'] in ['gemini-live','gemini-adk','gemini-adk-fluent']]"`

Expected: all three show repository, documentation, and readme fields.

**Step 6: Commit**

```bash
git add Cargo.toml crates/gemini-live/Cargo.toml crates/gemini-adk/Cargo.toml crates/gemini-adk-fluent/Cargo.toml
git commit -m "docs: add repository, documentation, and readme metadata to all crates"
```

---

## Task 2: Add `#![warn(missing_docs)]` to All Crates

**Files:**
- Modify: `crates/gemini-live/src/lib.rs` (line 1)
- Modify: `crates/gemini-adk/src/lib.rs` (line 1)
- Modify: `crates/gemini-adk-fluent/src/lib.rs` (line 1)

**Step 1: Add warn attribute to gemini-live**

Insert at the very top of `crates/gemini-live/src/lib.rs` (before `//! # gemini-live`):

```rust
#![warn(missing_docs)]
```

**Step 2: Add warn attribute to gemini-adk**

Insert at the very top of `crates/gemini-adk/src/lib.rs`:

```rust
#![warn(missing_docs)]
```

**Step 3: Add warn attribute to gemini-adk-fluent**

Insert at the very top of `crates/gemini-adk-fluent/src/lib.rs`:

```rust
#![warn(missing_docs)]
```

**Step 4: Build and count warnings**

Run: `cargo doc --no-deps --workspace 2>&1 | grep "warning: missing documentation" | wc -l`

Record the count — this is the baseline. Each subsequent task should reduce it to zero.

**Step 5: Commit**

```bash
git add crates/gemini-live/src/lib.rs crates/gemini-adk/src/lib.rs crates/gemini-adk-fluent/src/lib.rs
git commit -m "docs: add #![warn(missing_docs)] to all three crates"
```

---

## Task 3: Document All Public Items in gemini-live

**Scope:** ~25 module files. The crate-level docs (lib.rs) are already excellent. Focus on undocumented public structs, enums, traits, functions, variants, and fields.

**Files:** All `.rs` files under `crates/gemini-live/src/`

**Step 1: Get exact list of missing docs**

Run: `cargo doc --no-deps -p gemini-live 2>&1 | grep "warning: missing documentation"`

This gives you the exact file:line for every undocumented item.

**Step 2: Add doc comments to every item listed**

For each warning, add a `///` doc comment. Guidelines:
- **Structs/Enums**: One sentence describing what it represents. If it maps to a Gemini API type, say so.
- **Variants**: Brief description of what the variant means (e.g., `/// 16-bit signed PCM at 16kHz.`)
- **Trait methods**: What the method does, what it returns.
- **Functions**: What it does, parameters if non-obvious, return value.
- **Struct fields**: Only if non-obvious from the field name + type.
- **Re-exports in prelude**: Skip (re-exports inherit docs from the original).

Style: Match existing docs in the crate — concise, technical, no fluff. One-liners preferred unless the item genuinely needs more explanation.

**Step 3: Verify zero warnings**

Run: `cargo doc --no-deps -p gemini-live 2>&1 | grep "warning: missing documentation" | wc -l`

Expected: `0`

**Step 4: Commit**

```bash
git add crates/gemini-live/src/
git commit -m "docs: add missing doc comments to all public items in gemini-live"
```

---

## Task 4: Document All Public Items in gemini-adk

**Scope:** ~92 module files. Largest crate. The crate-level docs are good. Focus on undocumented public items.

**Files:** All `.rs` files under `crates/gemini-adk/src/`

**Step 1: Get exact list of missing docs**

Run: `cargo doc --no-deps -p gemini-adk 2>&1 | grep "warning: missing documentation"`

**Step 2: Add doc comments to every item listed**

Same guidelines as Task 3. Additional notes for gemini-adk:
- **Agent trait methods**: Describe the lifecycle stage each method handles.
- **Tool traits** (ToolFunction, StreamingTool, etc.): Describe when to use each variant.
- **State methods**: Describe prefix scoping behavior if relevant.
- **Live module types**: Describe the role in the three-lane processor architecture.
- **Builder methods**: Describe what the method configures and any defaults.

**Step 3: Verify zero warnings**

Run: `cargo doc --no-deps -p gemini-adk 2>&1 | grep "warning: missing documentation" | wc -l`

Expected: `0`

**Step 4: Commit**

```bash
git add crates/gemini-adk/src/
git commit -m "docs: add missing doc comments to all public items in gemini-adk"
```

---

## Task 5: Document All Public Items in gemini-adk-fluent + Enhance lib.rs

**Scope:** ~14 module files. Smallest crate but highest doc gap (~50%). Also needs lib.rs crate-level doc enhancement.

**Files:** All `.rs` files under `crates/gemini-adk-fluent/src/`

**Step 1: Enhance crate-level docs in lib.rs**

Replace the current crate-level docs (lines 1-4) with a full overview:

```rust
//! # gemini-adk-fluent
//!
//! Fluent developer experience layer for the Gemini Live agent stack.
//! This is the highest-level crate in the workspace, providing a builder API,
//! operator algebra, and composition modules that sit on top of
//! [`gemini_adk`] (agent runtime) and [`gemini_live`] (wire protocol).
//!
//! ## Module Organization
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`builder`] | Copy-on-write immutable `AgentBuilder` for declarative agent configuration |
//! | [`compose`] | S·C·T·P·M·A operator algebra for composing agent primitives |
//! | [`live`] | `Live` session handle — callback-driven full-duplex event handling |
//! | [`live_builders`] | Builder types for live session configuration |
//! | [`operators`] | Operator combinators for composing agents |
//! | [`patterns`] | Pre-built composition patterns for common use cases |
//! | [`testing`] | Test utilities and mock helpers |
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use gemini_adk_fluent::prelude::*;
//!
//! let agent = AgentBuilder::new("my-agent")
//!     .model(GeminiModel::Gemini2_0Flash)
//!     .instruction("You are a helpful assistant.")
//!     .build();
//! ```
//!
//! ## Relationship to Other Crates
//!
//! - **`gemini-live`** (L0): Wire protocol, transport, types — re-exported via [`gemini_live`]
//! - **`gemini-adk`** (L1): Agent runtime, tools, sessions — re-exported via [`gemini_adk`]
//! - **`gemini-adk-fluent`** (L2): This crate — ergonomic builder API and composition
```

**Step 2: Get exact list of missing docs**

Run: `cargo doc --no-deps -p gemini-adk-fluent 2>&1 | grep "warning: missing documentation"`

**Step 3: Add doc comments to every item listed**

Same guidelines as Tasks 3-4. Additional notes for gemini-adk-fluent:
- **Builder methods**: Document what each method configures, the default value, and return type.
- **Operators (S, C, T, P, M, A)**: Ensure each operator module has a `//!` header explaining the algebra.
- **Live type**: Document the callback registration methods.
- **Prelude re-exports**: Skip (they inherit from source crate).

**Step 4: Verify zero warnings**

Run: `cargo doc --no-deps -p gemini-adk-fluent 2>&1 | grep "warning: missing documentation" | wc -l`

Expected: `0`

**Step 5: Commit**

```bash
git add crates/gemini-adk-fluent/src/
git commit -m "docs: enhance crate-level docs and add missing doc comments in gemini-adk-fluent"
```

---

## Task 6: Create Per-Crate README.md Files

**Files:**
- Create: `crates/gemini-live/README.md`
- Create: `crates/gemini-adk/README.md`
- Create: `crates/gemini-adk-fluent/README.md`

**Step 1: Create gemini-live/README.md**

```markdown
# gemini-live

Raw wire protocol and transport for the Gemini Multimodal Live API. This is the L0 (foundation) crate in the gemini-rs workspace — it handles WebSocket connections, authentication, wire-format types, and audio buffering with no agent abstractions.

## Features

- **Protocol types** mapping 1:1 to the Gemini Live API wire format
- **WebSocket transport** with Vertex AI and Google AI authentication
- **Lock-free audio buffers** (SPSC ring buffer, adaptive jitter buffer)
- **Voice activity detection** with adaptive noise floor
- **Feature-gated REST APIs** (generate, embed, files, models, tokens, caches, tunings, batches)
- **Pluggable architecture** via `Transport`, `Codec`, and `AuthProvider` traits

## Quick Start

```rust,ignore
use gemini_live::prelude::*;

let config = TransportConfig::google_ai("YOUR_API_KEY", GeminiModel::Gemini2_0Flash);
let (handle, events) = connect(config).await?;

handle.send_text("Hello!").await?;
while let Some(event) = events.recv().await {
    // Handle server events
}
```

## Documentation

[API Reference (docs.rs)](https://docs.rs/gemini-live)

## License

Apache-2.0
```

**Step 2: Create gemini-adk/README.md**

```markdown
# gemini-adk

Agent runtime for Gemini Live — tools, streaming, agent transfer, middleware. This is the L1 (runtime) crate that builds on `gemini-live` to provide agent lifecycle, tool dispatch, state management, and the three-lane processor architecture.

## Features

- **Agent trait** with lifecycle hooks for text and live (voice) sessions
- **Tool system** — `ToolFunction`, `StreamingTool`, `TypedTool` with JSON Schema generation
- **State management** — prefixed key-value store with atomic `modify()`, delta tracking
- **Three-lane processor** — fast (audio), control (tools/phases), telemetry (signals)
- **LLM extractors** — structured data extraction from conversation transcripts
- **Phase system** — instruction-scoped conversation phases with tool filtering
- **Middleware chain** — composable request/response processing pipeline
- **Text agents** — 15+ combinators (sequential, parallel, race, route, loop, etc.)

## Quick Start

```rust,ignore
use gemini_adk::*;

let tool = SimpleTool::new("get_weather", "Get current weather", |args| async {
    Ok(serde_json::json!({"temp": 72, "unit": "F"}))
});

let session = LiveSessionBuilder::new()
    .model(gemini_live::prelude::GeminiModel::Gemini2_0Flash)
    .instruction("You are a weather assistant.")
    .tool(tool)
    .build()
    .await?;
```

## Documentation

[API Reference (docs.rs)](https://docs.rs/gemini-adk)

## License

Apache-2.0
```

**Step 3: Create gemini-adk-fluent/README.md**

```markdown
# gemini-adk-fluent

Fluent developer experience for Gemini Live — builder API, operator algebra, and composition modules. This is the L2 (DX) crate, the highest-level entry point in the gemini-rs workspace.

## Features

- **`AgentBuilder`** — copy-on-write immutable builder for declarative agent configuration
- **S·C·T·P·M·A operators** — composable algebra for state, context, tools, phases, middleware, and agents
- **`Live` session** — callback-driven full-duplex voice/text event handling
- **Pre-built patterns** — common agent compositions ready to use
- **Full re-exports** — `gemini_adk` and `gemini_live` available through the prelude

## Quick Start

```rust,ignore
use gemini_adk_fluent::prelude::*;

let agent = AgentBuilder::new("assistant")
    .model(GeminiModel::Gemini2_0Flash)
    .instruction("You are a helpful assistant.")
    .build();
```

## Documentation

[API Reference (docs.rs)](https://docs.rs/gemini-adk-fluent)

## License

Apache-2.0
```

**Step 4: Commit**

```bash
git add crates/gemini-live/README.md crates/gemini-adk/README.md crates/gemini-adk-fluent/README.md
git commit -m "docs: add per-crate README.md files"
```

---

## Task 7: GitHub Actions Docs CI + GitHub Pages Deployment

**Files:**
- Create: `.github/workflows/docs.yml`

**Step 1: Create the workflow file**

```yaml
name: Documentation

on:
  pull_request:
    branches: [main]
  push:
    branches: [main]

permissions:
  contents: read
  pages: write
  id-token: write

concurrency:
  group: "pages"
  cancel-in-progress: false

jobs:
  check-docs:
    name: Check documentation
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Check docs build with no warnings
        env:
          RUSTDOCFLAGS: "-D warnings"
        run: cargo doc --no-deps --workspace

  deploy:
    if: github.event_name == 'push' && github.ref == 'refs/heads/main'
    needs: check-docs
    runs-on: ubuntu-latest
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build documentation
        run: cargo doc --no-deps --workspace
      - name: Add redirect index
        run: echo '<meta http-equiv="refresh" content="0; url=gemini_live/index.html">' > target/doc/index.html
      - uses: actions/upload-pages-artifact@v3
        with:
          path: target/doc
      - id: deployment
        uses: actions/deploy-pages@v4
```

**Step 2: Verify YAML syntax**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/docs.yml'))"`

Expected: No error.

**Step 3: Commit**

```bash
git add .github/workflows/docs.yml
git commit -m "ci: add GitHub Actions workflow for docs check and GitHub Pages deployment"
```

---

## Task 8: Final Verification

**Step 1: Full workspace doc build with strict warnings**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace 2>&1`

Expected: Clean build, zero warnings, zero errors.

**Step 2: Verify generated docs locally**

Run: `cargo doc --no-deps --workspace --open`

Manually check:
- gemini-live landing page has full crate overview
- gemini-adk landing page lists all modules with descriptions
- gemini-adk-fluent landing page shows the enhanced overview with module table
- Click through 3-4 types per crate — confirm doc comments render

**Step 3: Verify Cargo.toml metadata**

Run: `cargo metadata --no-deps --format-version 1 | python3 -c "import sys,json; pkgs=json.load(sys.stdin)['packages']; [print(f'{p[\"name\"]}: repo={p.get(\"repository\",\"MISSING\")} docs={p.get(\"documentation\",\"MISSING\")} readme={p.get(\"readme\",\"MISSING\")} kw={p.get(\"keywords\",[])}') for p in pkgs if p['name'] in ['gemini-live','gemini-adk','gemini-adk-fluent']]"`

Expected: All fields populated for all three crates.

---

## Execution Notes

- **Tasks 3, 4, 5 are parallelizable** — each crate's doc comments are independent.
- **Task 7 depends on Tasks 3-5** — the CI workflow uses `-D warnings` so it will fail if docs are incomplete.
- **Task 1 and 2 are quick setup** — do first to establish the baseline warning count.
- **Task 6 is independent** — can run in parallel with doc comment tasks.
- **Task 8 is the gate** — nothing ships until this passes.

## Dependency Graph

```
Task 1 (Cargo.toml metadata)  ──┐
Task 2 (warn attribute)  ───────┤
                                 ├──→ Task 3 (gemini-live docs)    ──┐
                                 ├──→ Task 4 (gemini-adk docs)      ──┼──→ Task 7 (CI workflow) ──→ Task 8 (verification)
                                 ├──→ Task 5 (gemini-adk-fluent)    ──┘
                                 └──→ Task 6 (READMEs)          ──┘
```
