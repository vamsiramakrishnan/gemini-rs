# Documentation Quality & Auto-Publishing Design

**Date**: 2026-03-03
**Status**: Approved
**Repo**: https://github.com/vamsiramakrishnan/gemini-rs

## Goal

Bring all three crates (gemini-live, gemini-adk, gemini-adk-fluent) to publishable documentation quality with enforced standards and automatic doc generation/deployment.

## Current State

| Aspect | gemini-live | gemini-adk | gemini-adk-fluent |
|--------|----------|--------|---------------|
| Crate-level docs | Excellent | Good | Minimal |
| Module //! coverage | ~80% | ~61% | ~50% |
| Public item /// docs | ~70% | ~60% | ~50% |
| Cargo.toml metadata | Partial | Minimal | Minimal |
| README.md | None | None | None |
| warn(missing_docs) | No | No | No |
| CI doc generation | None | None | None |

## Design

### 1. Cargo.toml Metadata

**Workspace Cargo.toml** — add to `[workspace.package]`:
```toml
repository = "https://github.com/vamsiramakrishnan/gemini-rs"
```

**Per-crate Cargo.toml** — add:
```toml
documentation = "https://docs.rs/{crate-name}"
readme = "README.md"
```

**gemini-adk and gemini-adk-fluent** — add missing:
```toml
keywords = [...]
categories = [...]
```

### 2. Doc Enforcement

Add `#![warn(missing_docs)]` to all three `lib.rs` files. This produces compiler warnings (not errors) for undocumented public items, enabling gradual adoption.

### 3. Doc Comment Coverage

Write `///` doc comments on every undocumented public item across all crates:
- Structs, enums, traits, functions, type aliases, constants
- Enum variants and struct fields where non-obvious
- Trait methods (required and provided)
- Builder methods in gemini-adk-fluent

Estimated scope:
- gemini-live: ~30% gap across ~25 modules
- gemini-adk: ~40% gap across ~92 modules (largest effort)
- gemini-adk-fluent: ~50% gap across ~14 modules

### 4. Per-Crate READMEs

Each crate gets `README.md` with:
- One-paragraph purpose description (which layer, what it does)
- Feature highlights (3-5 bullets)
- Quick-start code example
- Link to API docs

### 5. Crate-Level Doc Enhancement

Expand `gemini-adk-fluent/src/lib.rs` crate docs from single-line to full overview:
- Architecture overview of the fluent DX layer
- Module organization
- Key types to start with
- Relationship to gemini-adk and gemini-live

gemini-live and gemini-adk lib.rs docs are already good — no changes needed.

### 6. GitHub Actions CI

**File**: `.github/workflows/docs.yml`

**On PR** (any branch):
```yaml
- name: Check docs
  run: RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```
Fails the PR if any documentation warnings exist.

**On push to main**:
```yaml
- name: Build docs
  run: cargo doc --no-deps --workspace
- name: Deploy to GitHub Pages
  uses: actions/deploy-pages@v4
```
Publishes to `https://vamsiramakrishnan.github.io/gemini-rs/`.

## Out of Scope

- mdBook / tutorial-style docs (can layer on later)
- crates.io publishing (separate concern)
- Changelog generation
- Doc tests / example validation
