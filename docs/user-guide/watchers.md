# State Watchers & Temporal Patterns

Watchers and temporal patterns are reactive primitives that fire callbacks when
state conditions are met. Watchers respond to value changes (numeric thresholds,
boolean flips, value transitions). Temporal patterns respond to time-based
conditions (sustained state, event rates, consecutive turns). Together they let
you build escalation logic, compliance monitoring, and adaptive behavior without
polling.

## What Are Watchers?

A watcher observes a single state key and fires an async action when a
predicate matches the state diff. The SDK evaluates all watchers after each
mutation cycle (extractors + computed variables), comparing a snapshot of watched
keys taken before mutations to the values after.

```rust,ignore
use adk_rs_fluent::prelude::*;

Live::builder()
    .watch("app:score")
        .crossed_above(0.9)
        .then(|old, new, state| async move {
            state.set("high_score_alert", true);
            tracing::info!("Score crossed 0.9: {old} -> {new}");
        })
```

The `.watch(key)` call starts a `WatchBuilder`. You chain a predicate, then
`.then(action)` to complete it and return to the `Live` builder.

## Numeric Watchers

Fire when a numeric value crosses a threshold in a specific direction.

### crossed_above

Fires when the old value was below the threshold and the new value is at or above
it. Does not fire again if the value stays above the threshold.

```rust,ignore
// Fire when willingness_to_pay crosses above 0.7
.watch("willingness_to_pay")
    .crossed_above(0.7)
    .then(|_old, new, _state| async move {
        tracing::info!("Willingness crossed above 0.7: {new}");
    })
```

### crossed_below

Fires when the old value was at or above the threshold and the new value drops
below it.

```rust,ignore
// Fire when sentiment drops below 0.3
.watch("derived:sentiment_score")
    .crossed_below(0.3)
    .then(|_old, new, state| async move {
        state.set("low_sentiment_alert", true);
        tracing::warn!("Sentiment dropped below 0.3: {new}");
    })
```

Both predicates require numeric JSON values. Non-numeric values (strings, bools)
will not trigger the watcher.

## Boolean Watchers

Fire on boolean state transitions.

### became_true

Fires when the value changes from any non-`true` value to `true`. This includes
the transition from not-set (null) to `true`.

```rust,ignore
// Fire when cease_desist_requested flips to true
.watch("cease_desist_requested")
    .became_true()
    .blocking()  // await this action before continuing
    .then(|_old, _new, state| async move {
        state.set("cease_desist_active", true);
        tracing::warn!("Cease-and-desist requested");
    })
```

### became_false

Fires when the value changes from `true` to any non-`true` value.

```rust,ignore
// Fire when identity_verified reverts to false
.watch("identity_verified")
    .became_false()
    .then(|_old, _new, _state| async move {
        tracing::warn!("Identity verification revoked");
    })
```

## Value Watchers

### changed

Fires on any change to the watched key, regardless of old or new value. This is
the default predicate if you call `.then()` without setting one.

```rust,ignore
.watch("negotiation_intent")
    .changed()
    .then(|old, new, _state| async move {
        tracing::info!("Intent changed: {old} -> {new}");
    })
```

### changed_to

Fires only when the new value equals a specific `serde_json::Value`.

```rust,ignore
use serde_json::json;

// Fire when negotiation_intent becomes "dispute"
.watch("negotiation_intent")
    .changed_to(json!("dispute"))
    .then(|_old, _new, _state| async move {
        tracing::warn!("Debtor is disputing the debt");
    })
```

### Blocking vs Concurrent

By default, watcher actions are spawned concurrently. Use `.blocking()` to make
the processor await the action before continuing. Use blocking for actions that
set state other watchers or phases depend on. Use concurrent (the default) for
fire-and-forget side effects like logging and notifications.

## Temporal Patterns

Temporal patterns detect conditions that unfold over time. Unlike watchers (which
react to single state diffs), temporal patterns track duration, rates, and
consecutive counts.

### when_sustained -- State Held for Duration

Fires when a state-based condition remains true for at least the specified
duration. Resets if the condition becomes false. Requires periodic timer checks,
which the SDK handles automatically.

```rust,ignore
// Fire when sentiment stays below 0.4 for 30 seconds
.when_sustained(
    "sustained_frustration",
    |s| {
        let sentiment: f64 = s.get("derived:sentiment_score").unwrap_or(0.5);
        sentiment < 0.4
    },
    Duration::from_secs(30),
    |state, writer| async move {
        // Inject a de-escalation prompt
        writer.send_client_content(
            vec![Content::user("[System: User appears frustrated. Use empathetic tone.]")],
            false,
        ).await.ok();
        state.set("de_escalation_triggered", true);
    },
)
```

How it works internally:
1. First check where condition is true: records the start time. Does not fire.
2. Subsequent checks while condition holds: compares elapsed time to duration.
3. When elapsed >= duration: fires the action.
4. If condition becomes false at any point: resets the start time.

### when_rate -- Event Rate Threshold

Fires when at least `count` matching events occur within a sliding time window.
The filter function selects which `SessionEvent` types count.

```rust,ignore
use rs_genai::session::SessionEvent;

// Fire when 3+ interruptions happen within 60 seconds
.when_rate(
    "rapid_interruptions",
    |evt| matches!(evt, SessionEvent::Interrupted),
    3,
    Duration::from_secs(60),
    |_state, writer| async move {
        writer.update_instruction(
            "The user is interrupting frequently. Speak more concisely.".into()
        ).await.ok();
    },
)
```

Old timestamps outside the window are automatically expired on each check.

### when_turns -- Consecutive Turn Threshold

Fires when a condition is true for N consecutive turns. Resets the counter if
the condition is false on any turn.

```rust,ignore
// Fire when no_progress is true for 5 consecutive turns
.when_turns(
    "stalled_conversation",
    |s| {
        let progress: bool = s.get("making_progress").unwrap_or(true);
        !progress
    },
    5,
    |state, writer| async move {
        state.set("escalation_needed", true);
        writer.send_text(
            "It seems we're having difficulty. Let me connect you with a specialist.".into()
        ).await.ok();
    },
)
```

## Computed Variables

Computed variables are pure functions of other state keys. They auto-recalculate
when dependencies change and write results to the `derived:` prefix.

```rust,ignore
Live::builder()
    // Simple computed var: sentiment from emotion
    .computed("sentiment_score", &["emotional_state"], |state| {
        let emotion: String = state.get("emotional_state")?;
        let score = match emotion.as_str() {
            "cooperative" => 0.9,
            "calm" => 0.7,
            "frustrated" => 0.4,
            "angry" => 0.2,
            _ => 0.5,
        };
        Some(serde_json::json!(score))
    })
    // Computed var that depends on another computed var
    .computed("call_risk_level", &["derived:sentiment_score", "cease_desist_requested"], |state| {
        let sentiment: f64 = state.get("derived:sentiment_score").unwrap_or(0.5);
        let cease_desist: bool = state.get("cease_desist_requested").unwrap_or(false);
        let level = if cease_desist { "critical" }
            else if sentiment < 0.3 { "high" }
            else if sentiment < 0.5 { "medium" }
            else { "low" };
        Some(serde_json::json!(level))
    })
```

Key behaviors:
- **Dependency ordering**: topologically sorted, dependencies evaluated first
- **Change detection**: watchers only fire on keys that actually changed
- **Derived fallback**: written to `derived:{key}`, readable without prefix (see [State Management](state.md))
- **Cycle detection**: panics at registration if you create circular dependencies
- **Returning None**: skips the key (no write, no change detection)

## Watcher + Phase Integration

Watchers and phases work together naturally. A watcher can set state that triggers
a phase transition, or a phase transition can create state that a watcher reacts to.

### Pattern: Watcher triggers phase transition

```rust,ignore
Live::builder()
    // Watcher sets state when cease-and-desist is requested
    .watch("cease_desist_requested")
        .became_true()
        .blocking()
        .then(|_old, _new, state| async move {
            state.set("cease_desist_active", true);
        })
    // Phase transition reacts to that state
    .phase("negotiate")
        .instruction(NEGOTIATE_INSTRUCTION)
        .transition("close", S::is_true("cease_desist_requested"))
        .done()
```

### Pattern: Computed var drives phase transition

```rust,ignore
Live::builder()
    .computed("risk_level", &["derived:sentiment_score"], |state| {
        let sentiment: f64 = state.get("derived:sentiment_score").unwrap_or(0.5);
        if sentiment < 0.3 { Some(json!("high")) }
        else { Some(json!("low")) }
    })
    .phase("normal")
        .instruction("Handle the conversation normally.")
        .transition("de_escalate", S::eq("risk_level", "high"))
        .done()
    .phase("de_escalate")
        .instruction("The user is upset. De-escalate with empathy.")
        .terminal()
        .done()
```

## Real-World Example: Debt Collection Escalation

The debt collection cookbook (`cookbooks/ui/src/apps/debt_collection.rs`) combines
all reactive primitives in a single builder chain:

1. **Computed chain**: `emotional_state` (from extractor) -> `sentiment_score` -> `call_risk_level`
2. **Watchers**: `crossed_below(0.3)` on sentiment triggers alerts; `became_true`
   on `cease_desist_requested` runs a blocking action
3. **Temporal**: `when_sustained` detects 30 seconds of frustration;
   `when_rate` catches 3+ interruptions in 60 seconds; `when_turns` flags 5
   consecutive stalled turns
4. **Phase defaults**: inject computed state into every phase instruction and
   conditionally append empathy warnings
5. **Transitions**: guards use `S::is_true`, `S::eq`, `S::one_of` to move
   between 7 phases with compliance gates

The data flows in one direction: extractors populate raw state -> computed
variables derive higher-level signals -> watchers react to changes -> temporal
patterns detect sustained conditions -> phase transitions evaluate guards. All
within a single turn cycle.

## Evaluation Order

After each turn, the control lane processes mutations in this order:

1. **Extractors** run and write to state (e.g. `emotional_state`, `willingness_to_pay`)
2. **Computed variables** recompute in dependency order
3. **Watchers** evaluate against the diff (snapshot before vs after)
4. **Temporal patterns** check against current state and event
5. **Phase transitions** evaluate guards

This means a computed variable can react to extractor output, a watcher can
react to a computed variable's change, and a phase transition can react to state
set by a watcher -- all within a single turn cycle.
