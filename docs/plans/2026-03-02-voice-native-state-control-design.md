# Voice-Native State & Control Flow — A Speech-to-Speech Architecture

**Date**: 2026-03-02
**Status**: Design RFC
**Perspective**: How a voice-native architect designs control flow for full-duplex
speech-to-speech systems built on Gemini Live
**Complements**: `callback-mode-design.md` (execution semantics),
`fluent-devex-redesign.md` (composition primitives and fluent surface)

---

## Executive Summary

The companion design documents answer HOW events flow (callback execution modes)
and WHAT verbs exist (fluent API surface). This document answers the question those
documents assume but never state: **what does the developer actually need to BUILD?**

Every voice application is a state machine. A restaurant order bot tracks order
items, conversation phase, user sentiment. A medical intake system tracks symptoms,
severity scores, triage decisions. A customer service agent tracks issue category,
escalation status, resolution attempts.

Today, developers wire all of this by hand: a flat `State` key-value store, manual
extraction calls, hand-coded phase transitions inside `instruction_template`, ad-hoc
counters in callbacks. The result is the same spaghetti code the fluent layer was
meant to prevent — just moved from protocol boilerplate to application logic
boilerplate.

**What's missing is the control flow layer between events and application logic.**

This document defines:

1. **State Variable Taxonomy** — the categories of state every voice app needs,
   with standard naming, types, and lifecycles
2. **Computed State** — derived variables that auto-recalculate when dependencies
   change, eliminating manual bookkeeping
3. **The Phase Machine** — declarative conversation phases with entry/exit actions,
   transition guards, and per-phase instructions — replacing hand-coded
   `match phase { ... }` blocks
4. **State Watchers** — reactive triggers when state values cross thresholds or
   change categories, enabling "when X happens, do Y" without polling
5. **Temporal Patterns** — detecting sequences and durations across events
   (3 interruptions in 30 seconds, 5 seconds of silence, repeated tool failures)
6. **The Computation Taxonomy** — every operation organized by whether it uses an
   LLM, how fast it is, and how it composes — making the permutation space explicit
   so developers pick the right primitive for each job
7. **Higher-Order Patterns** — compositions of these primitives for real-world
   scenarios, showing how LLM and non-LLM computations interleave

The design principle: **a voice application is a reactive state graph, not a
procedural script.** State changes propagate through computed derivations, trigger
phase transitions, fire watchers, and shape model behavior — all declared upfront,
not scattered across callbacks.

---

## 1. The State Architecture

### 1.1 What's Wrong with Flat Key-Value State

The current `State` is a `DashMap<String, Value>` with typed get/set. It works,
but it treats all state as equal. In practice, voice application state has
**structure**:

```
Current State (flat):
  "order_items" → [{"name": "pizza", "qty": 1}]
  "phase" → "ordering"
  "user_name" → "Alice"
  "turn_count" → 5
  "last_error" → "API timeout"
  "sentiment" → "neutral"
  "conversation_summary" → "Customer ordering dinner..."
  "search_results" → [...]
  "interrupted_count" → 2
  "model_is_speaking" → true
```

All keys live in one namespace. Nothing says which keys are set by the runtime vs
the developer vs extractors. Nothing says which keys are ephemeral vs persistent.
Nothing says which keys other keys depend on.

### 1.2 State Variable Categories

Every voice application's state falls into five categories:

```
┌──────────────────────────────────────────────────────────────────┐
│                    Voice Application State                       │
│                                                                  │
│  ┌──────────────────┐  ┌──────────────────┐  ┌───────────────┐  │
│  │ SESSION SIGNALS   │  │  DOMAIN STATE    │  │  DERIVED      │  │
│  │ (runtime-managed) │  │  (app-managed)   │  │  (computed)   │  │
│  │                   │  │                  │  │               │  │
│  │ turn_count        │  │ order_items      │  │ order_total   │  │
│  │ phase             │  │ user_profile     │  │ is_engaged    │  │
│  │ is_model_speaking │  │ search_results   │  │ should_upsell │  │
│  │ is_user_speaking  │  │ appointment_date │  │ context_load  │  │
│  │ silence_ms        │  │ symptoms_list    │  │ readiness     │  │
│  │ interrupt_count   │  │ ticket_id        │  │               │  │
│  │ error_count       │  │                  │  │               │  │
│  └──────────────────┘  └──────────────────┘  └───────────────┘  │
│                                                                  │
│  ┌──────────────────┐  ┌──────────────────────────────────────┐  │
│  │ TURN-SCOPED      │  │  BACKGROUND                          │  │
│  │ (reset each turn)│  │  (written by async agents/tools)     │  │
│  │                  │  │                                      │  │
│  │ current_intent   │  │ conversation_summary (agent)         │  │
│  │ tool_calls_count │  │ enriched_profile (API call)          │  │
│  │ extraction_raw   │  │ knowledge_context (vector search)    │  │
│  │ audio_duration   │  │ sentiment_analysis (LLM classifier)  │  │
│  └──────────────────┘  └──────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

#### Category 1: Session Signals (Runtime-Managed)

These are set by the framework, not the developer. They represent what's happening
in the live session right now:

| Variable | Type | Set By | Description |
|----------|------|--------|-------------|
| `session.turn_count` | `u32` | Processor (TurnComplete) | Completed turns |
| `session.phase` | `SessionPhase` | Processor (PhaseChanged) | Current session phase |
| `session.is_model_speaking` | `bool` | Processor (Phase) | Model producing audio |
| `session.is_user_speaking` | `bool` | Processor (VadStart/End) | User speaking |
| `session.silence_ms` | `u64` | Processor (timer) | Ms since last voice activity |
| `session.interrupt_count` | `u32` | Processor (Interrupted) | Total interruptions |
| `session.error_count` | `u32` | Processor (Error) | Non-fatal errors this session |
| `session.connected_at` | `Instant` | Processor (Connected) | Session start time |
| `session.elapsed_ms` | `u64` | Derived | Time since connection |
| `session.active_tools` | `Vec<String>` | Processor | Currently executing bg tools |
| `session.last_tool_error` | `Option<String>` | Processor | Most recent tool failure |
| `session.context_tokens_est` | `u32` | Processor | Estimated context token count |

Today, developers track these manually in callbacks. This is error-prone and
repetitive. The framework should maintain them automatically and expose them
through `state.session()`:

```rust
// CURRENT: manual tracking
let interrupt_count = Arc::new(AtomicU32::new(0));
let ic = interrupt_count.clone();
.on_interrupted(move || {
    ic.fetch_add(1, Ordering::SeqCst);
    async {}
})
.instruction_template(move |state| {
    let count = interrupt_count.load(Ordering::SeqCst);
    if count > 3 {
        Some("User seems impatient. Be concise.".into())
    } else { None }
})

// PROPOSED: automatic
.instruction_template(|state| {
    if state.session().get::<u32>("interrupt_count")? > 3 {
        Some("User seems impatient. Be concise.".into())
    } else { None }
})
```

**Implementation**: The processor writes session signals to `state.session().*`
(a namespaced prefix) on every relevant event. This is zero-cost for unused
signals — a simple counter increment or bool flip.

#### Category 2: Domain State (App-Managed)

Application-specific state, explicitly written by the developer in callbacks
or extracted by LLMs:

```rust
// Set by extractor
.extract_turns::<OrderState>(llm, "Extract items, quantities, phase")
// → state.set("OrderState", extracted_value)

// Set by developer in callback
.on_connected(|| async {
    let profile = db::load_profile(user_id).await;
    state.set("user_profile", profile);
})

// Set by tool
.tool_with_state("add_item", "Add to order", |args, state| async move {
    let mut items: Vec<Item> = state.get("order_items").unwrap_or_default();
    items.push(Item::from(args));
    state.set("order_items", &items);
    Ok(json!({"added": true, "count": items.len()}))
})
```

No change here — domain state is inherently application-specific.

#### Category 3: Derived State (Computed)

Values that are **functions of other state**. Today, developers compute these
inline wherever they're needed, leading to duplication and inconsistency:

```rust
// TODAY: computed inline in instruction_template
.instruction_template(|state| {
    let items: Vec<Item> = state.get("order_items").unwrap_or_default();
    let total: f64 = items.iter().map(|i| i.price * i.qty as f64).sum();  // computed here
    let has_drinks = items.iter().any(|i| i.category == "drinks");        // and here
    // ... use total, has_drinks in instruction
})

// AND ALSO: computed inline in tool
.tool_with_state("check_total", "Get order total", |_args, state| async move {
    let items: Vec<Item> = state.get("order_items").unwrap_or_default();
    let total: f64 = items.iter().map(|i| i.price * i.qty as f64).sum();  // DUPLICATED
    Ok(json!({"total": total}))
})
```

See Section 2 for the computed state proposal.

#### Category 4: Turn-Scoped State

State that resets every turn. Useful for per-turn accumulators:

```rust
// Audio duration this turn, tool calls this turn, etc.
// Today: developers manually reset in on_turn_complete
// Proposed: state.turn().* auto-resets on TurnComplete
```

#### Category 5: Background State

Written by async background agents or tools, read at the next turn boundary.
Inherently eventually consistent:

```rust
// Written by background summarizer agent (2-3s behind)
state.set("conversation_summary", summary);

// Written by background enrichment API (1-2s behind)
state.set("enriched_profile", enriched);

// Read by instruction_template (uses whatever is latest)
.instruction_template(|state| {
    let summary = state.get::<String>("conversation_summary")?;
    Some(format!("Context: {summary}\n\nBe helpful."))
})
```

Background state is always "one turn behind" at best. The framework should make
this explicit rather than leaving developers to discover it through debugging.

### 1.3 State Namespacing

Proposal: prefix-based namespacing with convenience accessors:

```rust
impl State {
    fn session(&self) -> PrefixedState<'_>  // "session:" — runtime signals
    fn app(&self) -> PrefixedState<'_>      // "app:" — domain state (already exists)
    fn turn(&self) -> PrefixedState<'_>     // "turn:" — reset each turn
    fn bg(&self) -> PrefixedState<'_>       // "bg:" — background agent results
    fn derived(&self) -> PrefixedState<'_>  // "derived:" — computed values (read-only to user)
}
```

The existing `app()`, `user()`, `temp()` prefixes are preserved. New prefixes
for runtime-managed categories.

---

## 2. Computed State — Reactive Derivations

### 2.1 The Problem

Every voice app has values that are pure functions of other state. Developers
compute them ad-hoc, leading to:
- **Duplication**: same formula in instruction_template, tool handlers, watchers
- **Staleness**: value computed in callback A is stale by the time callback B reads it
- **Invisibility**: no way to see what depends on what

### 2.2 The Proposal: `computed()`

A computed variable is a pure function of other state keys. It auto-recalculates
when any dependency changes:

```rust
Live::builder()
    // Computed: order total from items
    .computed("order_total", &["order_items"], |state| {
        let items: Vec<Item> = state.get("order_items")?;
        Some(json!(items.iter().map(|i| i.price * i.qty as f64).sum::<f64>()))
    })

    // Computed: whether to upsell drinks
    .computed("should_upsell", &["order_items", "session.turn_count"], |state| {
        let items: Vec<Item> = state.get("order_items").unwrap_or_default();
        let has_drinks = items.iter().any(|i| i.category == "drinks");
        let turns: u32 = state.session().get("turn_count").unwrap_or(0);
        Some(json!(!has_drinks && turns > 2 && !items.is_empty()))
    })

    // Computed: context load estimate (fraction of context window used)
    .computed("context_load", &["session.turn_count", "session.context_tokens_est"], |state| {
        let tokens: u32 = state.session().get("context_tokens_est").unwrap_or(0);
        let max_tokens: u32 = 128_000; // model context window
        Some(json!((tokens as f64) / (max_tokens as f64)))
    })

    // Computed: engagement score
    .computed("engagement", &["session.interrupt_count", "session.silence_ms", "session.turn_count"], |state| {
        let interrupts: u32 = state.session().get("interrupt_count").unwrap_or(0);
        let silence: u64 = state.session().get("silence_ms").unwrap_or(0);
        let turns: u32 = state.session().get("turn_count").unwrap_or(0);
        // Simple heuristic: engaged users interrupt more, have less silence
        let score = if turns == 0 { 0.5 } else {
            let interrupt_rate = interrupts as f64 / turns as f64;
            let silence_factor = 1.0 - (silence as f64 / 10_000.0).min(1.0);
            (interrupt_rate * 0.3 + silence_factor * 0.7).clamp(0.0, 1.0)
        };
        Some(json!(score))
    })
```

### 2.3 Implementation

```rust
/// A computed state variable.
pub struct ComputedVar {
    /// The state key where the computed value is stored.
    key: String,
    /// State keys this variable depends on.
    dependencies: Vec<String>,
    /// Pure function: state → Option<Value>. Returns None to leave unchanged.
    compute: Arc<dyn Fn(&State) -> Option<Value> + Send + Sync>,
}
```

**When does recomputation happen?**

Not on every state write (too expensive with DashMap notifications). Instead:

1. **On TurnComplete** — after extractors run, before instruction_template.
   All computed vars recompute in dependency order. This is the natural
   "sync point" in the control lane.

2. **On explicit trigger** — `state.recompute()` can be called in any callback
   to force immediate recomputation. Useful in tool handlers that need fresh
   derived values.

**Dependency ordering**: Computed vars can depend on other computed vars.
The framework topologically sorts them at registration time and recomputes
in order:

```
order_items (domain) → order_total (computed) → should_upsell (computed)
                                              → order_summary (computed)
```

If a cycle is detected, the builder panics at registration time with a clear
error message naming the cycle.

### 2.4 Why Not a Full Reactive Framework?

Reactive frameworks (like signals/effects in UI) recompute on every write.
In a voice system with 25+ events/sec, this would mean recomputing derived
state 25+ times per second for audio events that don't change the relevant
dependencies.

The TurnComplete sync point is the right granularity: state changes
accumulate during a turn, then all derived values catch up at once. For the
rare case where you need immediate recomputation (inside a tool handler),
`state.recompute()` is the escape hatch.

### 2.5 Computed vs Extracted

Extracted state uses an LLM (1-5 second async call). Computed state is a
pure function (< 1ms sync). They serve different purposes:

| | Computed | Extracted |
|---|---------|-----------|
| Speed | < 1ms (sync) | 1-5s (async LLM call) |
| Determinism | Always same output for same input | Probabilistic |
| Input | Other state values | Transcript text |
| Cost | Zero (CPU only) | LLM API call per turn |
| When | TurnComplete (after extraction) | TurnComplete (before computed) |

**Evaluation order on TurnComplete:**
```
1. TranscriptBuffer.end_turn()          — finalize transcript
2. Extractors run (LLM calls)           — populate domain state
3. Computed vars recompute              — derive from domain state
4. Phase machine evaluates transitions  — check guards
5. Watchers fire                        — react to changes
6. instruction_template evaluates       — may read all of the above
7. on_turn_boundary fires               — context injection
8. on_turn_complete fires               — user callback
```

---

## 3. The Phase Machine — Declarative Conversation Phases

### 3.1 The Problem

Every voice application has conversation phases. Today, developers encode them
as a string in state and hand-code transitions:

```rust
// TODAY: imperative phase management
.extract_turns::<OrderState>(llm, "Extract items and conversation phase")
.instruction_template(|state| {
    match state.get::<String>("OrderState.phase")?.as_str() {
        "greeting" => Some("Welcome warmly. Ask how you can help.".into()),
        "ordering" => Some("Take orders. Suggest items.".into()),
        "confirming" => Some("Read back order. Confirm total.".into()),
        "complete" => Some("Thank customer. Say goodbye.".into()),
        _ => None,
    }
})
```

Problems:
- Phase transitions are implicit (the LLM extractor decides the phase)
- No entry/exit actions (can't run code when transitioning)
- No transition guards (can't prevent invalid transitions)
- No phase-specific tool activation (all tools available in all phases)
- No phase history (can't check "was the user ever in phase X?")
- Instructions grow unwieldy as phases multiply

### 3.2 The Proposal: Declarative Phase Machine

```rust
Live::builder()
    .phase("greeting")
        .instruction("Welcome the customer warmly. Ask how you can help today.")
        .on_enter(|state, writer| async move {
            // Load user profile on entry
            if let Ok(profile) = db::load_profile(user_id).await {
                state.set("user_profile", profile);
            }
        })
        .transition_to("ordering")
            .when(|state| {
                // Transition when user mentions any food item
                state.get::<Vec<Item>>("order_items")
                    .map(|items| !items.is_empty())
                    .unwrap_or(false)
            })
        .transition_to("help")
            .when(|state| {
                state.get::<String>("current_intent")
                    .map(|i| i == "help" || i == "question")
                    .unwrap_or(false)
            })

    .phase("ordering")
        .instruction("Help with ordering. Suggest popular items. Ask about drinks.")
        .tools_enabled(&["search_menu", "check_allergens", "get_specials"])
        .on_enter(|state, _writer| async move {
            state.session().set("ordering_start_turn",
                state.session().get::<u32>("turn_count").unwrap_or(0));
        })
        .transition_to("confirming")
            .when(|state| {
                state.get::<String>("current_intent")
                    .map(|i| i == "done_ordering" || i == "that_is_all")
                    .unwrap_or(false)
            })
        .transition_to("ordering")  // self-loop: stay in ordering
            .when(|state| {
                state.get::<String>("current_intent")
                    .map(|i| i == "add_item" || i == "remove_item" || i == "modify_item")
                    .unwrap_or(false)
            })

    .phase("confirming")
        .instruction(|state| {
            // Dynamic instruction using current state
            let items: Vec<Item> = state.get("order_items").unwrap_or_default();
            let total: f64 = state.get("order_total").unwrap_or(0.0);
            format!(
                "Read back the order:\n{}\nTotal: ${:.2}\nAsk for confirmation.",
                items.iter().map(|i| format!("- {} x{}", i.name, i.qty)).collect::<Vec<_>>().join("\n"),
                total
            )
        })
        .tools_enabled(&["place_order", "modify_order"])
        .transition_to("complete")
            .when(|state| state.get::<bool>("order_placed").unwrap_or(false))
        .transition_to("ordering")
            .when(|state| {
                state.get::<String>("current_intent")
                    .map(|i| i == "change_order" || i == "add_more")
                    .unwrap_or(false)
            })

    .phase("complete")
        .instruction("Thank the customer. Provide order number. Say goodbye.")
        .on_enter(|state, writer| async move {
            // Inject order confirmation into conversation
            let order_id: String = state.get("order_id").unwrap_or_default();
            writer.send_client_content(
                vec![Content::user().text(format!("[Order #{order_id} confirmed]"))],
                false,
            ).await.ok();
        })
        .terminal()  // No transitions out — session ends here

    .initial_phase("greeting")
```

### 3.3 Phase Machine Semantics

**Evaluation timing**: After computed state recomputes on TurnComplete, the phase
machine evaluates all transitions from the current phase. First matching
transition wins (evaluated in registration order).

**Transition lifecycle**:
```
Current phase: "ordering"
TurnComplete fires
  → Extractors run → state updated
  → Computed vars recompute → order_total, should_upsell updated
  → Phase machine evaluates:
      transition_to("confirming").when(intent == "done_ordering") → TRUE
      1. on_exit("ordering") fires (if registered)
      2. state.session().set("phase", "confirming")
      3. on_enter("confirming") fires (if registered)
      4. instruction updated to confirming phase instruction
  → Watchers fire (including phase watchers)
  → instruction_template runs (phase instruction already set — template can augment)
  → on_turn_boundary fires
  → on_turn_complete fires
```

**Phase-scoped tools**: When `tools_enabled` is set, only those tools are declared
to Gemini. This requires the framework to update the tool declarations in the
session. Since Gemini Live does NOT support changing tools mid-session, this is
implemented as **tool call filtering**: all tools are declared at setup, but the
`on_tool_call` interceptor rejects calls to tools not in the current phase's
enabled set:

```rust
// Framework-generated on_tool_call guard:
if !current_phase.tools_enabled.contains(&call.name) {
    return Some(vec![FunctionResponse {
        name: call.name.clone(),
        response: json!({"error": "This action is not available right now."}),
        id: call.id.clone(),
    }]);
}
```

This is transparent to the developer. The model quickly learns which tools are
available through the rejection responses.

### 3.4 Phase vs Extracted Phase

The LLM extractor can still detect phase from conversation. The phase machine
adds **deterministic guardrails** on top:

```rust
// LLM says we're in "confirming" but no items in order → reject transition
.phase("confirming")
    .guard(|state| {
        // Can only enter confirming if there are items
        state.get::<Vec<Item>>("order_items")
            .map(|items| !items.is_empty())
            .unwrap_or(false)
    })
```

**Hybrid approach**: Use the LLM extractor for **intent detection** (what does the
user want?), and the phase machine for **state transitions** (given this intent
and current state, what phase are we in?). The LLM is good at understanding
natural language intent. The deterministic machine is good at enforcing business
rules.

```
LLM extractor → "current_intent" = "done_ordering"
Phase machine → checks: intent == "done_ordering" AND order_items.len() > 0
             → transition to "confirming" ✓

LLM extractor → "current_intent" = "done_ordering"
Phase machine → checks: intent == "done_ordering" AND order_items.len() > 0
             → order_items is empty → NO TRANSITION
             → model stays in "ordering" phase, instruction says "Take orders"
             → model naturally asks "What would you like to order?"
```

This separation of concerns produces more robust applications than relying on
the LLM extractor alone to determine phase.

### 3.5 Phase History

The framework tracks phase history for pattern detection:

```rust
state.session().get::<Vec<PhaseTransition>>("phase_history")
// → [{from: "greeting", to: "ordering", turn: 2},
//    {from: "ordering", to: "confirming", turn: 5},
//    {from: "confirming", to: "ordering", turn: 6},  // user changed mind
//    {from: "ordering", to: "confirming", turn: 8}]
```

This enables temporal patterns like "user has gone back to ordering twice" →
adjust strategy.

---

## 4. State Watchers — Reactive Triggers

### 4.1 The Problem

Developers need "when X happens, do Y" logic that cuts across callbacks. Today
this requires checking conditions in every callback that might change the
relevant state:

```rust
// TODAY: check sentiment in every place it might matter
.on_extracted(|name, value| async move {
    if name == "sentiment" {
        let sentiment = value.as_str().unwrap_or("neutral");
        if sentiment == "frustrated" {
            // update instruction, log alert, etc.
        }
    }
})
// AND ALSO check in instruction_template
.instruction_template(|state| {
    let sentiment = state.get::<String>("sentiment")?;
    if sentiment == "frustrated" {
        Some("User is frustrated. Be empathetic.".into())
    } else { None }
})
```

### 4.2 The Proposal: `watch()`

A watcher fires when a state key's value changes in a way that matches a
predicate:

```rust
Live::builder()
    // Watch: sentiment changed to frustrated
    .watch("sentiment")
        .changed_to("frustrated")
        .then(|old, new, state| async move {
            tracing::warn!(old = ?old, "User became frustrated");
            metrics::counter!("sentiment_frustrated").increment(1);
        })

    // Watch: order total crossed $50 threshold
    .watch("order_total")
        .crossed_above(50.0)
        .then(|_old, new, state| async move {
            state.set("eligible_for_discount", true);
        })

    // Watch: error count exceeded threshold
    .watch("session.error_count")
        .crossed_above(3)
        .then(|_old, _new, state| async move {
            state.set("should_escalate", true);
        })

    // Watch: any change to order_items
    .watch("order_items")
        .on_change()
        .then(|old, new, state| async move {
            // Log order changes for analytics
            analytics::track("order_modified", json!({
                "old_count": old.as_array().map(|a| a.len()),
                "new_count": new.as_array().map(|a| a.len()),
            })).await;
        })
```

### 4.3 Watcher Predicates

```rust
pub enum WatchPredicate {
    /// Fire on any value change.
    Changed,
    /// Fire when value equals target.
    ChangedTo(Value),
    /// Fire when value was target and is now different.
    ChangedFrom(Value),
    /// Fire when numeric value crosses threshold upward.
    CrossedAbove(f64),
    /// Fire when numeric value crosses threshold downward.
    CrossedBelow(f64),
    /// Fire when boolean becomes true.
    BecameTrue,
    /// Fire when boolean becomes false.
    BecameFalse,
    /// Custom predicate: (old_value, new_value) → bool.
    Custom(Arc<dyn Fn(&Value, &Value) -> bool + Send + Sync>),
}
```

### 4.4 Watcher Timing

Watchers fire **after computed state recomputes** on TurnComplete:

```
TurnComplete → Extractors → Computed vars → Phase machine → WATCHERS → ...
```

This means watchers see fully consistent state: extractors have run, computed
vars are fresh, phase transitions have completed.

**Watcher execution mode**: Watchers default to `Concurrent` (fire-and-forget).
The developer can opt into `Blocking` for watchers that must complete before
the turn:

```rust
.watch("order_items")
    .on_change()
    .blocking()  // Must complete before next turn processes
    .then(|_old, new, state| async move {
        // Validate order with external service — must complete
        let valid = validate_order(new).await;
        state.set("order_valid", valid);
    })
```

### 4.5 Why Watchers Instead of More Callbacks?

Callbacks fire on **events** (things the model/server does). Watchers fire on
**state changes** (things that may result from any combination of events,
extractors, computed vars, or tools). This is a fundamentally different trigger:

| Trigger | Example |
|---------|---------|
| Event callback | "Model completed a turn" → `on_turn_complete` |
| Watcher | "Order total crossed $50" → fires regardless of whether the total changed due to extraction, tool call, or direct state write |

Watchers decouple the "when" (state condition) from the "how" (which callback
caused the state change). This eliminates the cross-cutting concern problem.

---

## 5. Temporal Patterns — Detecting Sequences Over Time

### 5.1 The Problem

Voice applications need to detect patterns that span multiple events or time
windows:

- "User has been silent for 5 seconds" → prompt them
- "Model has been interrupted 3 times in 60 seconds" → user is frustrated
- "Tool `search_menu` has failed 2 consecutive times" → switch to fallback
- "User has asked the same question twice" → escalate
- "No order items after 3 turns in the ordering phase" → suggest popular items

These require tracking history, counting within windows, and detecting sequences.
Today, developers build this from scratch with counters and timestamps in
callbacks.

### 5.2 The Proposal: Temporal Pattern Primitives

#### 5.2.1 Sustained Condition

Fire when a condition has been true for a duration:

```rust
Live::builder()
    // Prompt after 5 seconds of silence
    .when_sustained("silence_prompt",
        |state| !state.session().get::<bool>("is_user_speaking").unwrap_or(false)
            && !state.session().get::<bool>("is_model_speaking").unwrap_or(false),
        Duration::from_secs(5),
    )
    .then(|state, writer| async move {
        writer.send_client_content(
            vec![Content::user().text("[User has been silent. Gently ask if they need anything.]")],
            false,
        ).await.ok();
    })
    .cooldown(Duration::from_secs(15))  // Don't re-trigger for 15s
```

#### 5.2.2 Rate Detection

Fire when an event occurs N times within a time window:

```rust
    // Frustration detection: 3+ interruptions in 60 seconds
    .when_rate("frequent_interrupts",
        |event| matches!(event, SessionEvent::Interrupted),
        3,
        Duration::from_secs(60),
    )
    .then(|state| async move {
        state.set("user_frustrated", true);
    })

    // Tool failure circuit breaker: 2 consecutive failures
    .when_consecutive_failures("menu_search_breaker", "search_menu", 2)
    .then(|state| async move {
        state.set("search_menu_disabled", true);
        tracing::error!("search_menu circuit breaker tripped");
    })
```

#### 5.2.3 Turn Count Condition

Fire when a condition has been true for N consecutive turns:

```rust
    // No items after 3 turns in ordering → suggest
    .when_turns("no_items_suggestion",
        |state| {
            state.session().get::<String>("phase") == Some("ordering".into())
            && state.get::<Vec<Item>>("order_items")
                .map(|items| items.is_empty())
                .unwrap_or(true)
        },
        3,  // 3 consecutive turns
    )
    .then(|state, writer| async move {
        writer.send_client_content(
            vec![Content::user().text(
                "[Customer hasn't ordered anything after 3 turns. \
                 Suggest popular items: Truffle Carbonara, Margherita Pizza, Caesar Salad.]"
            )],
            false,
        ).await.ok();
    })
```

### 5.3 Implementation Sketch

```rust
pub struct TemporalPattern {
    name: String,
    detector: Box<dyn PatternDetector + Send + Sync>,
    action: Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>,
    cooldown: Option<Duration>,
    last_triggered: Mutex<Option<Instant>>,
}

pub trait PatternDetector: Send + Sync {
    /// Called on every relevant event or at regular intervals.
    /// Returns true when the pattern is detected.
    fn check(&self, state: &State, event: Option<&SessionEvent>) -> bool;

    /// Reset internal counters/timestamps.
    fn reset(&self);
}
```

**Evaluation**: Temporal patterns are checked in the control lane on every event
(for event-based patterns) or on a timer (for duration-based patterns). They run
after watchers, before `on_turn_complete`:

```
TurnComplete → Extractors → Computed → Phase → Watchers → Temporal → instruction_template → ...
```

For duration-based patterns (silence detection), the processor spawns a
lightweight timer task that checks conditions at 500ms intervals. This is
cheap — a single atomic read per pattern per tick.

---

## 6. The Computation Taxonomy

### 6.1 Every Operation in a Voice Application

Every computation in a voice application sits at the intersection of five
dimensions. Making these explicit helps developers choose the right primitive.

```
                    ┌─────────────────────────────────────────────────┐
                    │         THE COMPUTATION SPACE                    │
                    │                                                  │
                    │   TRIGGER × INPUT × COMPUTE × TIMING × OUTPUT   │
                    │                                                  │
                    │   Trigger:  Event | StateChange | Timer | Phase  │
                    │   Input:    EventData | State | Transcript       │
                    │   Compute:  Rule | Score | LLM | Pipeline       │
                    │   Timing:   Sync | Blocking | Background        │
                    │   Output:   StateMut | Instruction | Injection  │
                    │             | ToolResponse | SideEffect         │
                    └─────────────────────────────────────────────────┘
```

### 6.2 Without LLM — Deterministic Computations

These are fast (< 1ms), deterministic, and free (no API cost). Use them for
everything you can before reaching for an LLM.

| Computation | Description | When to Use | Primitive |
|-------------|-------------|-------------|-----------|
| **Rule evaluation** | if/else on state values | Phase transitions, guards, routing | `phase().when()`, `gate()`, `route()` |
| **Scoring** | Weighted combination of signals | Engagement, readiness, confidence | `computed()` |
| **Counting** | Increment/track event rates | Error counts, turn counts, interrupts | Session signals (automatic) |
| **Accumulation** | Aggregate values over time | Total price, item count, duration | `computed()` |
| **Template rendering** | String interpolation with state | Instructions, prompts, messages | `instruction_template()`, phase instructions |
| **Threshold detection** | Value crosses boundary | "Order over $50", "3+ errors" | `watch().crossed_above()` |
| **Pattern matching** | String/value comparison | Intent routing, phase detection | `route()`, `watch().changed_to()` |
| **Windowing** | Sliding window over events | "Last 3 turns", "last 60 seconds" | `when_rate()`, `when_turns()` |
| **Debouncing** | Coalesce rapid changes | Avoid re-triggering on rapid extraction updates | `cooldown()` on temporal patterns |
| **Circuit breaking** | Stop after N failures | Tool failure protection | `when_consecutive_failures()` |
| **State transformation** | Reshape, filter, rename | Preparing state for tools/instructions | `S::pick()`, `S::rename()`, `computed()` |

**Example — All without LLM:**

```rust
Live::builder()
    // Rule: phase transitions
    .phase("ordering")
        .transition_to("confirming")
            .when(|s| s.get::<String>("intent") == Some("done_ordering".into()))

    // Scoring: engagement
    .computed("engagement", &["session.interrupt_count", "session.turn_count"], |s| {
        let interrupts: f64 = s.session().get("interrupt_count").unwrap_or(0) as f64;
        let turns: f64 = s.session().get("turn_count").unwrap_or(1) as f64;
        Some(json!((interrupts / turns).clamp(0.0, 1.0)))
    })

    // Accumulation: order total
    .computed("order_total", &["order_items"], |s| {
        let items: Vec<Item> = s.get("order_items").unwrap_or_default();
        Some(json!(items.iter().map(|i| i.price * i.qty as f64).sum::<f64>()))
    })

    // Threshold: discount eligibility
    .watch("order_total").crossed_above(50.0)
        .then(|_, _, state| async move { state.set("discount_eligible", true); })

    // Template: instruction from state
    .instruction_template(|s| {
        let phase = s.session().get::<String>("phase").unwrap_or_default();
        let engaged: f64 = s.get("engagement").unwrap_or(0.5);
        let style = if engaged > 0.7 { "enthusiastic" } else { "calm and patient" };
        Some(format!("Phase: {phase}. Communication style: {style}."))
    })

    // Circuit breaker: tool protection
    .when_consecutive_failures("search_breaker", "search_menu", 2)
        .then(|state| async move { state.set("search_disabled", true); })
```

**Zero LLM calls, zero API cost.** All deterministic, all < 1ms. This is the
skeleton of every voice application.

### 6.3 With LLM — Probabilistic Computations

These are slow (1-10s), probabilistic, and cost money. Use them for tasks that
require natural language understanding.

| Computation | Description | When to Use | Primitive |
|-------------|-------------|-------------|-----------|
| **Extraction** | Structured data from transcript | Order items, symptoms, preferences | `extract_turns::<T>()` |
| **Classification** | Categorize into labels | Intent, sentiment, topic, phase | Agent with constrained output |
| **Summarization** | Compress information | Context for instruction, conversation recap | Background agent → state |
| **Evaluation** | Judge quality/safety | Check if response is appropriate, verify tool results | Agent in `before_tool_response` |
| **Generation** | Produce text/content | Format results, write messages, create descriptions | Agent pipeline in callback |
| **Planning** | Multi-step reasoning | Determine tool sequence, resolve complex queries | Agent-as-tool (blocking or background) |

**Each has a natural timing:**

```
Extraction    → Blocking (on TurnComplete, feeds computed vars and phase machine)
Classification → Blocking or part of extraction (same LLM call)
Summarization → Background (eventual consistency, state updated async)
Evaluation    → Blocking (in before_tool_response, must complete before sending)
Generation    → Background or in tool handler (depends on urgency)
Planning      → Tool handler (model explicitly asked for complex reasoning)
```

### 6.4 The Permutation Matrix — Combining LLM and Non-LLM

The power is in composing both. Here are the real-world permutations:

#### Pattern A: Extract → Compute → React

LLM extracts structured data. Deterministic logic derives values and reacts.

```
Transcript → [LLM: extract items] → order_items
             → [compute: sum prices] → order_total
             → [watch: total > 50] → discount_eligible = true
             → [phase: check guards] → transition to "confirming"
             → [template: render instruction] → model behavior changes
```

**This is the most common pattern.** The LLM does the hard part (understanding
natural language), then deterministic logic handles business rules. One LLM call
per turn, unlimited deterministic reactions.

```rust
Live::builder()
    .extract_turns::<OrderState>(llm, "Extract items, quantities, preferences")
    .computed("order_total", &["OrderState"], |s| { /* sum */ })
    .watch("order_total").crossed_above(50.0).then(/* discount */)
    .phase("ordering").transition_to("confirming").when(/* guard */)
    .instruction_template(/* render from state */)
```

#### Pattern B: Extract → Classify → Route

LLM extracts, then a second LLM classifies, then deterministic routing:

```
Transcript → [LLM: extract intent] → intent = "complaint"
           → [LLM: classify severity] → severity = "high"
           → [route: severity] → "high" → escalation_agent
                                → "low" → self_service_agent
```

```rust
let severity_classifier = AgentBuilder::new("classifier")
    .instruction("Rate complaint severity: high, medium, low. Return JSON.")
    .temperature(0.1);

Live::builder()
    .extract_turns::<IntentState>(llm, "Extract user intent and context")
    .on_extracted_pipeline(
        severity_classifier
        >> fn_step("store", |state| async move {
            let severity = state.get::<String>("classifier_output")?;
            state.set("severity", severity);
            Ok(())
        }),
        llm.clone(),
    )
    .phase("triage")
        .transition_to("escalation").when(|s| s.get("severity") == Some("high"))
        .transition_to("self_service").when(|s| s.get("severity") == Some("low"))
```

#### Pattern C: Tool Result → LLM Post-Processing → State

A tool returns raw data, an LLM processes it, deterministic logic reacts:

```
Model calls search_flights → [API: 3s] → raw flight data
  → [LLM: rank by value] → ranked_flights
  → [compute: cheapest price] → best_price
  → [watch: best_price < budget] → within_budget = true
  → model receives formatted results
```

```rust
Live::builder()
    .tool_background_with_pipeline(
        "search_flights",
        fn_step("parse", |s| async move { /* parse raw */ Ok(()) })
        >> ranker_agent
        >> fn_step("derive", |s| async move {
            let ranked: Vec<Flight> = s.get("ranked_flights")?;
            s.set("best_price", ranked.first().map(|f| f.price).unwrap_or(0.0));
            Ok(())
        }),
        FlightFormatter,
        llm.clone(),
    )
    .computed("within_budget", &["best_price", "user_budget"], |s| {
        let price: f64 = s.get("best_price").unwrap_or(f64::MAX);
        let budget: f64 = s.get("user_budget").unwrap_or(0.0);
        Some(json!(price <= budget))
    })
```

#### Pattern D: Background LLM → State → Instruction

A background LLM agent runs continuously, updating state. The instruction
template reads the latest state:

```
TurnComplete → [dispatch: summarizer (3s)] → conversation_summary
             → [dispatch: sentiment (2s)] → sentiment_label
             ...next turn...
             → instruction_template reads summary + sentiment
             → model behavior adapts
```

```rust
let summarizer = AgentBuilder::new("summarizer")
    .instruction("Summarize conversation in 2 sentences.");
let sentiment = AgentBuilder::new("sentiment")
    .instruction("Classify tone: friendly, neutral, frustrated, confused.");

Live::builder()
    .on_extracted_dispatch(
        ("summary", summarizer),
        ("sentiment", sentiment),
    )
    .instruction_template(|s| {
        let summary = s.bg().get::<String>("summary").unwrap_or_default();
        let tone = s.bg().get::<String>("sentiment").unwrap_or("neutral".into());
        let empathy = if tone == "frustrated" { "Be extra empathetic. " } else { "" };
        Some(format!("{empathy}Context: {summary}"))
    })
```

#### Pattern E: Temporal + LLM — Adaptive Behavior

Temporal patterns detect situations, LLM agents execute adaptive responses:

```
[temporal: 3 turns no items in ordering] → detected
  → [LLM: generate_suggestions(menu, preferences)] → suggestions
  → [inject: client_content with suggestions]
```

```rust
let suggester = AgentBuilder::new("suggester")
    .instruction("Given menu and preferences, suggest 3 items.");

Live::builder()
    .when_turns("no_items_help", |s| {
        s.session().get::<String>("phase") == Some("ordering".into())
        && s.get::<Vec<Item>>("order_items").unwrap_or_default().is_empty()
    }, 3)
    .then_pipeline(
        fn_step("prep", |s| async move {
            s.set("menu", load_menu().await);
            s.set("prefs", s.get::<String>("user_preferences").unwrap_or_default());
            Ok(())
        })
        >> suggester
        >> fn_step("inject", |s| async move {
            let suggestions = s.get::<String>("suggester_output")?;
            // Inject as context for the model
            s.set("pending_injection", suggestions);
            Ok(())
        }),
        llm.clone(),
    )
```

#### Pattern F: Stateful Tool → Computed → Phase Transition

A tool with state access modifies domain state, triggering a cascade:

```
Model calls "place_order" → [tool: writes order_placed=true, order_id="ABC"]
  → [computed: order_total recalculates (no change)]
  → [phase: checks order_placed == true → transition to "complete"]
  → [phase.on_enter("complete"): inject confirmation, change instruction]
  → model says "Your order ABC is confirmed!"
```

This cascade happens within a single tool response cycle. No extra LLM call
needed — the deterministic phase machine handles the transition, and the
phase's instruction change tells the model what to do next.

### 6.5 The Decision Tree — Which Primitive for Which Job?

```
Need to understand natural language?
  YES → Use LLM
    Need structured data from transcript?      → extract_turns::<T>()
    Need to categorize/label?                  → Agent classifier
    Need to compress/summarize?                → Background agent (dispatch)
    Need complex reasoning for tool response?  → Agent-as-tool
    Need to judge/evaluate?                    → Agent in before_tool_response

  NO → Use deterministic computation
    Need a value derived from other values?    → computed()
    Need to react when value changes?          → watch()
    Need conversation phases?                  → phase()
    Need to detect event patterns?             → when_rate() / when_turns()
    Need to detect silence/duration?           → when_sustained()
    Need conditional branching?                → phase transitions / route()
    Need to render text from state?            → instruction_template() / phase instruction
    Need to protect against failures?          → when_consecutive_failures()
```

---

## 7. Higher-Order Patterns — Real-World Compositions

### 7.1 The Adaptive Voice Agent

An agent that changes its communication style based on detected user behavior.
Uses temporal patterns + computed state + phase machine. **No additional LLM
calls beyond extraction.**

```rust
Live::builder()
    .model(GeminiModel::GeminiLive2_5FlashNativeAudio)
    .voice(Voice::Kore)

    // --- Extraction: one LLM call per turn ---
    .extract_turns::<ConversationState>(flash_llm,
        "Extract: intent, entities, sentiment, topic")

    // --- Session signals: automatic, zero cost ---
    // session.turn_count, session.interrupt_count, session.silence_ms, etc.

    // --- Computed state: deterministic, < 1ms ---
    .computed("patience_score",
        &["session.interrupt_count", "session.turn_count", "ConversationState.sentiment"],
        |s| {
            let interrupts: f64 = s.session().get("interrupt_count").unwrap_or(0) as f64;
            let turns: f64 = s.session().get("turn_count").unwrap_or(1) as f64;
            let sentiment = s.get::<String>("ConversationState.sentiment").unwrap_or_default();
            let sentiment_factor = match sentiment.as_str() {
                "frustrated" => 0.3,
                "confused" => 0.5,
                "neutral" => 0.7,
                "happy" => 1.0,
                _ => 0.7,
            };
            let interrupt_factor = 1.0 - (interrupts / turns).min(1.0);
            Some(json!((sentiment_factor * 0.6 + interrupt_factor * 0.4).clamp(0.0, 1.0)))
        })

    .computed("verbosity",
        &["patience_score", "session.turn_count"],
        |s| {
            let patience: f64 = s.get("patience_score").unwrap_or(0.7);
            let turns: u32 = s.session().get("turn_count").unwrap_or(0);
            // Start verbose, become concise as patience drops or conversation lengthens
            let turn_factor = 1.0 - (turns as f64 / 20.0).min(0.5);
            let v = if patience < 0.4 { "minimal" }
                    else if patience < 0.6 || turn_factor < 0.6 { "concise" }
                    else { "conversational" };
            Some(json!(v))
        })

    // --- Watchers: react to computed state changes ---
    .watch("patience_score")
        .crossed_below(0.3)
        .then(|_old, _new, state| async move {
            state.set("escalation_offered", false);
            // Will be picked up by instruction_template
        })

    // --- Temporal: detect patterns ---
    .when_rate("rapid_interrupts",
        |e| matches!(e, SessionEvent::Interrupted), 3, Duration::from_secs(30))
        .then(|state| async move {
            state.set("communication_mode", "bullet_points");
        })

    .when_sustained("long_silence",
        |s| !s.session().get::<bool>("is_user_speaking").unwrap_or(false)
            && !s.session().get::<bool>("is_model_speaking").unwrap_or(false),
        Duration::from_secs(8))
        .then(|state, writer| async move {
            writer.send_client_content(
                vec![Content::user().text("[Gently check if user is still there]")],
                false,
            ).await.ok();
        })
        .cooldown(Duration::from_secs(20))

    // --- Instruction template: compose from all state ---
    .instruction_template(|s| {
        let verbosity = s.get::<String>("verbosity").unwrap_or("conversational".into());
        let patience: f64 = s.get("patience_score").unwrap_or(0.7);
        let mode = s.get::<String>("communication_mode").unwrap_or_default();

        let mut instruction = String::new();
        instruction.push_str(&format!("Communication style: {verbosity}.\n"));

        if patience < 0.4 {
            instruction.push_str("User is losing patience. Be VERY concise. ");
            instruction.push_str("Skip pleasantries. Get to the point.\n");
        }

        if mode == "bullet_points" {
            instruction.push_str("Use short bullet-point responses. No paragraphs.\n");
        }

        Some(instruction)
    })
```

**Cost**: One LLM extraction call per turn. Everything else is deterministic.
The model's behavior adapts in real-time without additional LLM overhead.

### 7.2 The Multi-Phase Transaction

A loan application with strict phase transitions, document verification, and
human-in-the-loop approval.

```rust
Live::builder()
    .extract_turns::<LoanState>(flash_llm, "Extract: applicant info, loan amount, purpose, documents mentioned")

    // --- Phase machine: enforces business process ---
    .phase("intake")
        .instruction("Gather applicant information: name, income, employment. Be thorough but friendly.")
        .tools_enabled(&["check_credit", "verify_identity"])
        .transition_to("documents")
            .when(|s| {
                let loan: LoanState = s.get("LoanState").unwrap_or_default();
                loan.name.is_some() && loan.income.is_some() && loan.employment.is_some()
            })

    .phase("documents")
        .instruction("Ask for required documents: ID, proof of income, bank statements. Guide them through upload.")
        .tools_enabled(&["upload_document", "check_document_status"])
        .on_enter(|state, _writer| async move {
            let required = determine_required_docs(&state).await;
            state.set("required_docs", required);
        })
        .transition_to("review")
            .when(|s| {
                let required: Vec<String> = s.get("required_docs").unwrap_or_default();
                let uploaded: Vec<String> = s.get("uploaded_docs").unwrap_or_default();
                required.iter().all(|r| uploaded.contains(r))
            })

    .phase("review")
        .instruction(|s| {
            let loan: LoanState = s.get("LoanState").unwrap_or_default();
            format!(
                "Review the application with the customer:\n\
                 Name: {}\n\
                 Amount: ${}\n\
                 Purpose: {}\n\
                 Ask for final confirmation before submitting.",
                loan.name.unwrap_or_default(),
                loan.amount.unwrap_or(0),
                loan.purpose.unwrap_or_default(),
            )
        })
        .tools_enabled(&["submit_application"])
        .transition_to("submitted")
            .when(|s| s.get::<bool>("application_submitted").unwrap_or(false))
        .transition_to("intake")
            .when(|s| s.get::<String>("intent") == Some("change_info".into()))

    .phase("submitted")
        .instruction("Application submitted. Provide reference number. Answer follow-up questions about timeline.")
        .terminal()

    .initial_phase("intake")

    // --- Computed: readiness score ---
    .computed("application_completeness", &["LoanState"], |s| {
        let loan: LoanState = s.get("LoanState").unwrap_or_default();
        let fields = [loan.name.is_some(), loan.income.is_some(),
                      loan.employment.is_some(), loan.amount.is_some()];
        let filled = fields.iter().filter(|&&f| f).count();
        Some(json!(filled as f64 / fields.len() as f64))
    })

    // --- Watcher: notify when fully complete ---
    .watch("application_completeness")
        .crossed_above(0.99)
        .then(|_, _, state| async move {
            state.set("ready_for_docs", true);
        })

    // Human approval for submission
    .on_tool_call(|calls| async move {
        for call in &calls {
            if call.name == "submit_application" {
                let approved = show_approval_dialog(&call).await;
                if !approved {
                    return Some(vec![FunctionResponse {
                        name: call.name.clone(),
                        response: json!({"error": "Customer declined to submit"}),
                        id: call.id.clone(),
                    }]);
                }
            }
        }
        None
    })
```

### 7.3 The Knowledge-Augmented Support Agent

Combines vector search (background tool), LLM evaluation (pipeline), and
deterministic confidence routing.

```rust
let evaluator = AgentBuilder::new("evaluator")
    .instruction("Rate relevance of search results to the question. Score 0-10. Return JSON with score and reason.");

Live::builder()
    .extract_turns::<SupportState>(flash_llm, "Extract: issue_category, question, urgency")

    // Background knowledge search with LLM post-processing
    .tool_background_with_pipeline(
        "search_knowledge_base",
        fn_step("prep", |s| async move {
            s.set("search_results", s.get::<Value>("tool_result").unwrap());
            Ok(())
        })
        >> evaluator
        >> fn_step("filter", |s| async move {
            let scored: Vec<Value> = s.get("evaluator_output").unwrap_or_default();
            // Keep only results scoring > 6
            let relevant: Vec<_> = scored.iter()
                .filter(|r| r["score"].as_f64().unwrap_or(0.0) > 6.0)
                .cloned().collect();
            s.set("relevant_results", &relevant);
            s.set("kb_confidence", if relevant.is_empty() { 0.0 }
                                   else { relevant[0]["score"].as_f64().unwrap_or(0.0) / 10.0 });
            Ok(())
        }),
        KBSearchFormatter,
        llm.clone(),
    )

    // Computed: should escalate?
    .computed("should_escalate",
        &["SupportState.urgency", "session.error_count", "kb_confidence"],
        |s| {
            let urgency = s.get::<String>("SupportState.urgency").unwrap_or_default();
            let errors: u32 = s.session().get("error_count").unwrap_or(0);
            let confidence: f64 = s.get("kb_confidence").unwrap_or(1.0);
            let escalate = urgency == "critical"
                || errors > 2
                || confidence < 0.3;
            Some(json!(escalate))
        })

    // Watch: auto-escalate
    .watch("should_escalate")
        .became_true()
        .then(|_, _, state| async move {
            state.set("escalation_reason",
                format!("Auto-escalated: urgency={}, errors={}, confidence={}",
                    state.get::<String>("SupportState.urgency").unwrap_or_default(),
                    state.session().get::<u32>("error_count").unwrap_or(0),
                    state.get::<f64>("kb_confidence").unwrap_or(0.0),
                ));
        })

    .instruction_template(|s| {
        let escalate: bool = s.get("should_escalate").unwrap_or(false);
        if escalate {
            let reason = s.get::<String>("escalation_reason").unwrap_or_default();
            Some(format!(
                "ESCALATION TRIGGERED: {reason}\n\
                 Inform the customer you're connecting them with a specialist.\n\
                 Summarize the issue for the handoff."
            ))
        } else {
            None
        }
    })
```

---

## 8. The Complete Evaluation Pipeline

Putting it all together — the exact order of operations on TurnComplete:

```
TurnComplete event arrives in control lane
│
├── 1. TranscriptBuffer.end_turn()
│       Finalizes current turn transcript. ~0ms.
│
├── 2. Extractors run (LLM calls, blocking)
│       Each extractor: state.set(name, value)
│       Fires on_extracted callback per extractor
│       ~1-5s total (parallelizable across extractors)
│
├── 3. Computed vars recompute (deterministic, sync)
│       Topological order through dependency graph.
│       All derived values refreshed.
│       ~0ms total (pure functions)
│
├── 4. Phase machine evaluates (deterministic, sync)
│       Check transition guards from current phase.
│       If transition: on_exit → update phase → on_enter → update instruction.
│       ~0-50ms (on_enter/on_exit may be async)
│
├── 5. Watchers fire (mostly concurrent)
│       Compare previous values to current.
│       Fire matching predicates.
│       Concurrent watchers spawned; blocking watchers awaited.
│       ~0ms for concurrent, variable for blocking
│
├── 6. Temporal patterns check (deterministic, sync)
│       Rate detectors, turn counters, sustained conditions.
│       Fire actions for matched patterns.
│       ~0ms
│
├── 7. instruction_template evaluates (sync, <1ms)
│       Reads all state (session, domain, computed, bg).
│       Returns instruction if changed.
│       Deduped: only sends update if different from last.
│
├── 8. on_turn_boundary fires (blocking)
│       Context injection via SessionWriter.
│       Reads state, writes client_content.
│
└── 9. on_turn_complete fires (user callback)
        User's custom logic.
```

**Total added latency from steps 3-7**: < 5ms. These are all sync/deterministic
operations. The only expensive step is #2 (extractors), which is already there.
Steps 3-7 are zero-marginal-cost additions that provide massive capability.

---

## 9. The Fluent Surface — How It Reads

### 9.1 Complete Builder API Additions

```rust
impl Live {
    // --- Computed State ---
    fn computed(self,
        key: &str,
        deps: &[&str],
        f: impl Fn(&State) -> Option<Value> + Send + Sync + 'static,
    ) -> Self;

    // --- Phase Machine ---
    fn phase(self, name: &str) -> PhaseBuilder;
    fn initial_phase(self, name: &str) -> Self;

    // --- Watchers ---
    fn watch(self, key: &str) -> WatchBuilder;

    // --- Temporal Patterns ---
    fn when_sustained(self,
        name: &str,
        condition: impl Fn(&State) -> bool + Send + Sync + 'static,
        duration: Duration,
    ) -> TemporalBuilder;

    fn when_rate(self,
        name: &str,
        event_filter: impl Fn(&SessionEvent) -> bool + Send + Sync + 'static,
        count: u32,
        window: Duration,
    ) -> TemporalBuilder;

    fn when_turns(self,
        name: &str,
        condition: impl Fn(&State) -> bool + Send + Sync + 'static,
        turn_count: u32,
    ) -> TemporalBuilder;

    fn when_consecutive_failures(self,
        name: &str,
        tool_name: &str,
        count: u32,
    ) -> TemporalBuilder;
}

// --- Sub-builders ---

impl PhaseBuilder {
    fn instruction(self, text: &str) -> Self;
    fn instruction_fn(self, f: impl Fn(&State) -> String + Send + Sync) -> Self;
    fn tools_enabled(self, tools: &[&str]) -> Self;
    fn on_enter(self, f: impl AsyncFn(State, Arc<dyn SessionWriter>)) -> Self;
    fn on_exit(self, f: impl AsyncFn(State, Arc<dyn SessionWriter>)) -> Self;
    fn guard(self, f: impl Fn(&State) -> bool + Send + Sync) -> Self;
    fn transition_to(self, target: &str) -> TransitionBuilder;
    fn terminal(self) -> Self;
}

impl TransitionBuilder {
    fn when(self, f: impl Fn(&State) -> bool + Send + Sync) -> PhaseBuilder;
}

impl WatchBuilder {
    fn on_change(self) -> WatchActionBuilder;
    fn changed_to(self, value: impl Into<Value>) -> WatchActionBuilder;
    fn changed_from(self, value: impl Into<Value>) -> WatchActionBuilder;
    fn crossed_above(self, threshold: f64) -> WatchActionBuilder;
    fn crossed_below(self, threshold: f64) -> WatchActionBuilder;
    fn became_true(self) -> WatchActionBuilder;
    fn became_false(self) -> WatchActionBuilder;
    fn when(self, f: impl Fn(&Value, &Value) -> bool + Send + Sync) -> WatchActionBuilder;
}

impl WatchActionBuilder {
    fn then(self, f: impl AsyncFn(Value, Value, State)) -> Live;
    fn blocking(self) -> Self;
}

impl TemporalBuilder {
    fn then(self, f: impl AsyncFn(State, Arc<dyn SessionWriter>)) -> Live;
    fn then_pipeline(self, pipeline: Composable, llm: Arc<dyn BaseLlm>) -> Live;
    fn cooldown(self, duration: Duration) -> Self;
}
```

### 9.2 How It All Reads Together

A complete medical triage agent:

```rust
let handle = Live::builder()
    .model(GeminiModel::GeminiLive2_5FlashNativeAudio)
    .voice(Voice::Aoede)
    .tools(dispatcher)

    // --- Extraction ---
    .extract_turns::<TriageState>(flash_llm,
        "Extract: symptoms, severity (1-10), duration, medical history mentioned")

    // --- Computed State ---
    .computed("triage_level",
        &["TriageState.severity", "TriageState.symptoms"],
        |s| {
            let severity: u32 = s.get("TriageState.severity").unwrap_or(1);
            let symptoms: Vec<String> = s.get("TriageState.symptoms").unwrap_or_default();
            let emergency_symptoms = ["chest pain", "difficulty breathing", "severe bleeding"];
            let has_emergency = symptoms.iter()
                .any(|sym| emergency_symptoms.iter().any(|e| sym.to_lowercase().contains(e)));
            let level = if has_emergency || severity >= 9 { "emergency" }
                       else if severity >= 6 { "urgent" }
                       else if severity >= 3 { "standard" }
                       else { "low" };
            Some(json!(level))
        })

    .computed("info_completeness",
        &["TriageState"],
        |s| {
            let t: TriageState = s.get("TriageState").unwrap_or_default();
            let checks = [!t.symptoms.is_empty(), t.severity > 0,
                         t.duration.is_some(), t.history_mentioned];
            let pct = checks.iter().filter(|&&c| c).count() as f64 / checks.len() as f64;
            Some(json!(pct))
        })

    // --- Phase Machine ---
    .phase("assessment")
        .instruction("Gather symptom information. Ask about: what symptoms, severity 1-10, how long, relevant medical history.")
        .tools_enabled(&["check_drug_interactions", "lookup_symptom"])
        .transition_to("emergency")
            .when(|s| s.get::<String>("triage_level") == Some("emergency".into()))
        .transition_to("recommendation")
            .when(|s| s.get::<f64>("info_completeness").unwrap_or(0.0) > 0.75)

    .phase("emergency")
        .instruction("EMERGENCY DETECTED. Calmly instruct patient to call 911 or go to nearest ER. Do NOT diagnose. Stay on the line.")
        .on_enter(|state, _writer| async move {
            alert_medical_team(&state).await;
        })
        .terminal()

    .phase("recommendation")
        .instruction(|s| {
            let t: TriageState = s.get("TriageState").unwrap_or_default();
            let level = s.get::<String>("triage_level").unwrap_or_default();
            format!(
                "Provide recommendation based on triage level: {level}.\n\
                 Symptoms: {:?}\n\
                 Severity: {}/10\n\
                 Suggest appropriate next steps (ER, urgent care, GP, self-care).",
                t.symptoms, t.severity
            )
        })
        .tools_enabled(&["schedule_appointment", "find_nearby_clinic"])
        .transition_to("followup")
            .when(|s| s.get::<bool>("recommendation_given").unwrap_or(false))

    .phase("followup")
        .instruction("Answer any follow-up questions. Remind about when to seek immediate care.")
        .terminal()

    .initial_phase("assessment")

    // --- Watchers ---
    .watch("triage_level")
        .changed_to("emergency")
        .blocking()
        .then(|_, _, state| async move {
            tracing::error!(
                symptoms = ?state.get::<Vec<String>>("TriageState.symptoms"),
                "EMERGENCY triage triggered"
            );
        })

    // --- Temporal ---
    .when_turns("info_stall", |s| {
        s.get::<f64>("info_completeness").unwrap_or(0.0) < 0.5
    }, 4)
    .then(|state, writer| async move {
        writer.send_client_content(
            vec![Content::user().text(
                "[Patient is having difficulty providing information. \
                 Try yes/no questions instead of open-ended ones.]"
            )],
            false,
        ).await.ok();
    })
    .cooldown(Duration::from_secs(60))

    // --- Audio & lifecycle ---
    .on_audio(|data| speaker.write(data))
    .on_error_concurrent(|err| async move { sentry::capture(&err); })

    .connect_vertex("vital-octagon-19612", "us-central1", token)
    .await?;
```

---

## 10. Implementation Scope

### 10.1 Layer Distribution

| Primitive | Layer | Complexity | Dependencies |
|-----------|-------|------------|-------------|
| Session signals | L1 (rs-adk processor) | Small | Modify processor to auto-track counters |
| State namespacing | L1 (rs-adk state) | Small | Add prefix helpers, `session()` accessor |
| `computed()` | L1 (rs-adk) + L2 (fluent) | Medium | Dependency graph, topo-sort, recompute on TurnComplete |
| Phase machine | L1 (rs-adk) + L2 (fluent) | Medium | Phase struct, transition evaluator, instruction override |
| `watch()` | L1 (rs-adk) + L2 (fluent) | Medium | Previous-value tracking, predicate evaluation |
| Temporal patterns | L1 (rs-adk) + L2 (fluent) | Medium | Event ring buffer, timer task, cooldown tracking |
| Evaluation pipeline integration | L1 (rs-adk processor) | Medium | Wire steps 3-7 into TurnComplete handler |

### 10.2 What Does NOT Change

- **rs-genai (L0)**: Zero changes. The wire protocol layer remains untouched.
- **Existing callbacks**: All existing callback registrations continue to work.
- **Existing extractors**: `extract_turns::<T>()` is unchanged — it's step 2 in the
  pipeline, and everything else layers on top.
- **Existing tools**: Tool registration and dispatch unchanged.

### 10.3 Phasing

**Phase 1**: Session signals + State namespacing (enables everything else)
**Phase 2**: Computed state (the foundation — most patterns build on this)
**Phase 3**: Phase machine (the most impactful single feature for devex)
**Phase 4**: Watchers (reactive triggers, builds on computed)
**Phase 5**: Temporal patterns (advanced, builds on session signals)

Each phase is independently useful. A developer can use computed state without
the phase machine. They can use the phase machine without temporal patterns.
The evaluation pipeline (Section 8) gracefully skips unregistered steps.

---

## 11. Relationship to Companion Documents

This document complements, not replaces, the other two design docs:

| Document | Answers | This Document Uses |
|----------|---------|-------------------|
| **callback-mode-design** | HOW callbacks execute (blocking vs concurrent) | Watchers and temporal actions use CallbackMode |
| **fluent-devex-redesign** | WHAT composition verbs exist (dispatch, route, pipeline) | Phase on_enter/on_exit can invoke pipelines; temporal `.then_pipeline()` uses Composable |
| **This document** | WHAT state to track, WHEN to react, HOW to structure control flow | Builds on callbacks (execution) and fluent verbs (composition) |

The three documents together cover the full stack:
- **Wire semantics** → callback-mode-design (how events execute)
- **Composition primitives** → fluent-devex-redesign (what building blocks exist)
- **Application architecture** → this document (how to structure a voice app)
