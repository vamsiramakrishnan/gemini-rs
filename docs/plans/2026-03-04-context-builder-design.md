# Phase-Aware Context System Design

**Date**: 2026-03-04
**Status**: Approved

## Problem

Every demo hand-writes 40+ line `fn app_context(s: &State) -> String` closures
that all follow the same pattern: check key, format label, skip if missing, join.
These closures are phase-blind — the same context renders in every phase regardless
of what matters right now.

## Solution: Four Primitives

### 1. `ContextBuilder` (L1: gemini-adk)

Declarative state-to-narrative renderer. Sections group related keys. Skips missing
values. Implements `Fn(&State) -> String` — drops into existing `with_context()`.

When the current phase has `needs` metadata, appends a "Gathering: X, Y" line for
missing keys so the model knows what to focus on.

```rust
// File: crates/gemini-adk/src/live/context_builder.rs
pub struct ContextBuilder { sections: Vec<Section>, needs_keys: Vec<String> }
pub struct Section { label: String, fields: Vec<Field> }
pub struct Field { key: String, label: String, kind: FieldKind }
enum FieldKind { Value, Flag, Sentiment, Format(Arc<dyn Fn(&Value) -> String>) }
```

### 2. `Ctx::` namespace (L2: gemini-adk-fluent)

Fluent factory methods mirroring `S::`, `P::`, `T::`:

```rust
Ctx::builder()
    .section("Caller")
        .field("caller_name", "Name")
        .flag("is_known_contact", "Known contact")
    .section("Call")
        .sentiment("caller_sentiment")
    .build()
```

Composable with `+` operator.

### 3. Phase `.needs()` (L2 builder + L1 metadata)

```rust
.phase("identify_caller")
    .needs(&["caller_name", "caller_organization"])
```

Stored on `Phase` struct. ContextBuilder reads current phase needs from
`session:phase` and appends guidance for missing keys.

### 4. `S::is_set(key)` (L2)

Predicate: key exists with any non-null value. Replaces
`|s| s.get::<String>("key").is_some()`.

## File Layout

```
crates/gemini-adk/src/live/
  context_builder.rs    NEW
  phase.rs              ADD needs field
  mod.rs                ADD re-exports

crates/gemini-adk-fluent/src/
  compose/ctx.rs        NEW
  compose/mod.rs        ADD Ctx re-export
  compose/state.rs      ADD S::is_set()
  live_builders.rs      ADD .needs() on PhaseBuilder
```

## Integration

Zero breaking changes. `ContextBuilder` implements `Fn(&State) -> String`,
works with existing `InstructionModifier::CustomAppend` pipeline.
