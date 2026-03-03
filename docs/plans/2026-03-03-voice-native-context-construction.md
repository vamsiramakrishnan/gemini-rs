# Voice-Native Context Construction — Closing the Gaps

**Date**: 2026-03-03
**Status**: Design RFC
**Scope**: Instruction delivery, phase-transition context, Live-aware context policies
**Complements**: `voice-native-state-control-design.md` (S.C.T.P.M),
`primitives-architecture-audit.md` (five primitives), `implementation-architecture.md` (module map)
**Reference implementation**: `cookbooks/ui/src/apps/debt_collection.rs`

---

## Executive Summary

The S.C.T.P.M architecture gives us five primitives and three channels. The
phase machine, watchers, extractors, and computed state all work. What doesn't
work yet is the **seam between phase transitions and model behavior** — the
moment the model absorbs a new instruction and must continue the conversation
naturally rather than resetting to "how can I help you today?"

This document identifies four concrete gaps, proposes deltas to close them, and
shows how each delta improves the debt-collection showcase. The theme: **every
phase transition is a context construction event, not just an instruction swap.**

---

## 1. The Problem: What Breaks Today

### 1.1 The Double Instruction Send

When a phase transition fires during the TurnComplete pipeline, two WebSocket
frames hit the server:

```
TurnComplete pipeline (processor.rs):
  Step 6:  phase transition → writer.update_instruction(phase_inst)     ← FRAME 1
  Step 9:  instruction_amendment → writer.update_instruction(composed)  ← (skipped if no amendment)
  Step 10: instruction_template → writer.update_instruction(templated)  ← FRAME 2
```

The `instruction_template` in `debt_collection.rs` always returns `Some(...)` —
it re-reads `session:phase`, looks up the same constant, and appends `[Context: ...]`.
So every phase transition sends the phase instruction at step 6, then the template
overwrites it at step 10 with a nearly-identical instruction plus context suffix.

**Cost**: Two WebSocket writes per turn, the first always wasted. The dedup guard
(`shared.last_instruction`) doesn't catch it because the template adds dynamic
context that differs from the bare phase instruction.

**Fix direction**: The phase machine should not send a bare instruction when the
developer has registered `instruction_template` or `instruction_amendment` — those
are the developer's declaration that they own instruction composition.

### 1.2 The "How Can I Help You?" Problem

Instructions are sent as:
```json
{
  "client_content": {
    "turns": [{"role": "system", "parts": [{"text": "...instruction..."}]}],
    "turn_complete": false
  }
}
```

`turnComplete: false` means the model absorbs the instruction silently — no
response generated. The model waits for the next user utterance. When the user
speaks, the model has a new system instruction but **no conversational context
about what just happened**. The result: the model treats the new phase as a
cold start and says something like "How can I help you today?" instead of
continuing the conversation naturally.

The `on_enter` callbacks in `debt_collection.rs` only send UI notifications
(phase change events to the devtools panel). They don't inject any model-facing
context. Nothing tells the model: "The customer just confirmed their identity.
Now inform them about the debt."

### 1.3 C (Context) Module Is Inert for Live

The `C` namespace (context policies) operates on `&[Content]` — conversation
history that the client owns. But in Gemini Live, conversation history lives on
the server. The client never holds a `Vec<Content>` to filter.

- `C::window(5)` — nothing to window
- `C::user_only()` — nothing to filter
- `C::from_state(&["user:name"])` — creates a placeholder `[Context keys: ...]`
  message but never accesses actual State values

These policies are useful for text-based LLM calls (extractors, agent dispatch),
but they are **dead code paths for the Live instruction channel**.

### 1.4 TranscriptBuffer Not Exposed to Phase Callbacks

`TranscriptBuffer` lives inside the control lane closure. Phase `on_enter` and
`on_exit` callbacks receive `(State, Arc<dyn SessionWriter>)` but not the
transcript. The developer cannot say "include the last 3 turns of conversation
in the transition context" without manually accumulating transcripts in State —
which is exactly the boilerplate the TranscriptBuffer was built to eliminate.

---

## 2. Design Principles

Before proposing solutions, the principles that constrain them:

1. **Single instruction write per turn.** The TurnComplete pipeline should
   produce at most one `update_instruction` call per turn, not two or three.

2. **Phase transitions are context events.** When a phase changes, the model
   needs: (a) the new instruction, (b) conversational continuity context,
   (c) optionally a response prompt. These should compose into a single
   wire write.

3. **State → Instruction is a first-class bridge.** `C::from_state()` as a
   placeholder is not acceptable. State values must flow into instructions
   without the developer re-reading every key manually.

4. **Tight vs loose control.** Some phases need tight control (disclosure
   scripts, compliance gates). Others need loose control (open negotiation).
   The primitives should make this distinction declarative, not implicit.

5. **No new wire types.** The Gemini Live API has three client message types:
   `realtimeInput`, `clientContent`, `toolResponse`. Everything we build
   composes over `clientContent` and `UpdateInstruction`.

6. **L0 stays thin.** All intelligence lives in L1 (processor, phase machine)
   and L2 (fluent builders). L0 only adds wire commands if they map 1:1 to
   protocol features.

---

## 3. Proposed Deltas

### 3.1 Unified Instruction Composition (Fix the Double Send)

**Layer**: L1 (processor.rs)
**Delta**: The TurnComplete pipeline composes a single instruction from all
sources, then sends one write.

Current pipeline (steps 6, 9, 10 each send independently):
```
Step 6:  phase transition    → update_instruction(phase_inst)
Step 9:  instruction_amendment → update_instruction(base + amendment)
Step 10: instruction_template  → update_instruction(template_output)
```

Proposed pipeline (single composition point):
```
Step 6:  phase transition    → resolved_instruction = phase_inst   (NO SEND)
Step 9:  instruction_amendment → resolved_instruction += amendment  (NO SEND)
Step 10: instruction_template  → resolved_instruction = template()  (NO SEND)
Step 10b: SEND composed instruction (single write)
```

**Implementation**:
```rust
// processor.rs — TurnComplete handler

// 6. Evaluate phase transitions
let mut resolved_instruction: Option<String> = None;
if let Some(ref pm) = phase_machine {
    let mut machine = pm.lock().await;
    if let Some((target, transition_index)) = machine.evaluate(&state) {
        // transition() runs on_exit, on_enter, records history
        // but does NOT send instruction — it returns the resolved text
        resolved_instruction = machine.transition(&target, &state, &writer, turn, trigger).await;
        state.session().set("phase", machine.current());
    }
}

// 9. Instruction amendment (additive)
if let Some(ref amendment_fn) = callbacks.instruction_amendment {
    if let Some(amendment_text) = amendment_fn(&state) {
        let base = resolved_instruction.clone().unwrap_or_else(|| {
            // No phase transition this turn — use current phase instruction
            phase_machine.as_ref().and_then(|pm| /* resolve current */)
                .unwrap_or_default()
        });
        resolved_instruction = Some(format!("{base}\n\n{amendment_text}"));
    }
}

// 10. Instruction template (full replacement — overrides everything above)
if let Some(ref template) = callbacks.instruction_template {
    if let Some(new_instruction) = template(&state) {
        resolved_instruction = Some(new_instruction);
    }
}

// 10b. Single send — dedup against last instruction
if let Some(instruction) = resolved_instruction {
    let should_update = {
        let last = shared.last_instruction.lock();
        last.as_deref() != Some(&instruction)
    };
    if should_update {
        *shared.last_instruction.lock() = Some(instruction.clone());
        writer.update_instruction(instruction).await.ok();
    }
}
```

**Impact on debt_collection.rs**: Zero code changes. The `instruction_template`
closure works exactly as before. The only difference is one WebSocket frame per
turn instead of two.

### 3.2 Phase Enter Context — `on_enter_context` and `prompt_on_enter`

**Layer**: L1 (phase.rs, processor.rs), L2 (live.rs, live_builders.rs)
**Delta**: New phase-level callbacks that inject conversational context at
transition time.

The key insight: **phase transitions need to send context to the model, not
just swap instructions.** The instruction tells the model what to be. Context
tells the model what just happened and what to do next.

#### 3.2.1 `on_enter_context`: Phase-Scoped Context Injection

A new callback on `Phase` that returns optional `Content` to inject via
`send_client_content(..., false)` immediately after the instruction update:

```rust
// phase.rs
pub struct Phase {
    // ... existing fields ...

    /// Optional context injection on phase entry.
    /// Returns Content to send as client_content (turnComplete: false).
    /// This gives the model conversational continuity across phase transitions.
    pub on_enter_context: Option<Arc<
        dyn Fn(&State, &TranscriptWindow) -> Option<Vec<Content>>
            + Send + Sync
    >>,
}

/// A read-only view of recent transcript turns for context construction.
pub struct TranscriptWindow {
    turns: Vec<TranscriptTurn>,
}

impl TranscriptWindow {
    /// The last N turns as formatted text.
    pub fn formatted(&self) -> String { /* ... */ }
    /// Raw turns.
    pub fn turns(&self) -> &[TranscriptTurn] { &self.turns }
    /// Last user utterance.
    pub fn last_user(&self) -> Option<&str> { /* ... */ }
    /// Last model utterance.
    pub fn last_model(&self) -> Option<&str> { /* ... */ }
}
```

**Fluent API (L2)**:
```rust
// How debt_collection.rs would use it:
.phase("inform_debt")
    .instruction(INFORM_DEBT_INSTRUCTION)
    .tools(&["lookup_account"])
    .on_enter_context(|state, transcript| {
        let name: String = state.get("debtor_name").unwrap_or_default();
        let verified: bool = state.get("identity_verified").unwrap_or(false);
        if verified {
            Some(vec![Content::user(format!(
                "[Context: {name}'s identity has been verified. \
                 Proceed to inform them about the debt. \
                 Use the lookup_account tool to retrieve details.]"
            ))])
        } else {
            None
        }
    })
    .done()
```

#### 3.2.2 `prompt_on_enter`: Make the Model Respond on Phase Entry

Some phases need the model to speak first after a transition (disclosure,
greeting). Others need the model to wait for user input (negotiation,
payment collection). This should be declarative:

```rust
// phase.rs
pub struct Phase {
    // ... existing fields ...

    /// If true, send a turnComplete: true after the instruction + context,
    /// causing the model to generate a response immediately.
    pub prompt_on_enter: bool,
}
```

**Fluent API (L2)**:
```rust
.phase("disclosure")
    .instruction(DISCLOSURE_INSTRUCTION)
    .prompt_on_enter(true)   // Model speaks the disclosure immediately
    .done()

.phase("negotiate")
    .instruction(NEGOTIATE_INSTRUCTION)
    .prompt_on_enter(false)  // Wait for user to speak (default)
    .done()
```

**Wire behavior** when `prompt_on_enter` is true:
```
Frame 1: UpdateInstruction (role: system, turnComplete: false)
Frame 2: on_enter_context content (role: user, turnComplete: false) [if any]
Frame 3: Empty content (turnComplete: true) — triggers model response
```

When false (default), only frames 1 and 2 are sent. The model absorbs
instruction + context silently and responds when the user speaks next.

#### 3.2.3 Combined: The Phase Transition Pipeline

```
Phase transition fires:
  1. on_exit(old_phase)
  2. Update current phase
  3. on_enter(new_phase)  — UI notifications, state setup
  4. Resolve instruction   — returns text, no send yet
  5. on_enter_context()    — returns optional Content[]
  6. Record history

Later in TurnComplete pipeline (step 10b):
  7. Compose final instruction (phase + amendment + template)
  8. Send instruction       — single update_instruction write
  9. Send context content   — single send_client_content (if any)
  10. Send turn-complete    — if prompt_on_enter (triggers response)
```

### 3.3 Live Context Policies — `P::with_state` (State → Instruction Bridge)

**Layer**: L2 (compose/prompt.rs or new compose/live_context.rs)
**Delta**: First-class state-to-instruction bridge that replaces the manual
`format!(...)` in `instruction_template`.

The current `instruction_template` in `debt_collection.rs` does this:
```rust
.instruction_template(|state| {
    let phase: String = state.get("session:phase").unwrap_or_default();
    let risk: String = state.get("derived:call_risk_level").unwrap_or("low".into());

    let base = match phase.as_str() {
        "disclosure" => DISCLOSURE_INSTRUCTION,
        // ... 7 match arms ...
    };

    let mut instruction = base.to_string();

    // Manual state reads, manual format!()
    let emotion: String = state.get("emotional_state").unwrap_or_else(|| "unknown".into());
    let willingness: f64 = state.get("willingness_to_pay").unwrap_or(0.5);
    // ... more manual reads ...

    instruction.push_str(&format!(
        "\n\n[Context: Emotional state: {emotion}, Willingness: {willingness:.1}, ...]"
    ));

    Some(instruction)
})
```

This has two problems:
1. The developer re-implements phase instruction lookup (the phase machine
   already knows the instruction).
2. State reads are manual and repetitive.

**Proposed**: `P::with_state()` — a prompt modifier that auto-appends state
values to the phase instruction:

```rust
// compose/prompt.rs — new methods on the P module
impl P {
    /// Append formatted state values to the instruction.
    ///
    /// Keys are read from State at instruction-composition time and formatted
    /// as `[Context: key1=value1, key2=value2, ...]`.
    pub fn with_state(keys: &[&str]) -> InstructionModifier {
        let owned: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        InstructionModifier::StateAppend(owned)
    }

    /// Append state values with a custom formatter.
    pub fn with_state_formatted(
        f: impl Fn(&State) -> String + Send + Sync + 'static,
    ) -> InstructionModifier {
        InstructionModifier::CustomAppend(Arc::new(f))
    }

    /// Conditionally append text based on state.
    pub fn when(
        predicate: impl Fn(&State) -> bool + Send + Sync + 'static,
        text: impl Into<String>,
    ) -> InstructionModifier {
        let text = text.into();
        InstructionModifier::Conditional {
            predicate: Arc::new(predicate),
            text,
        }
    }
}

pub enum InstructionModifier {
    StateAppend(Vec<String>),
    CustomAppend(Arc<dyn Fn(&State) -> String + Send + Sync>),
    Conditional {
        predicate: Arc<dyn Fn(&State) -> bool + Send + Sync>,
        text: String,
    },
}
```

**Fluent API — how debt_collection.rs would simplify**:

```rust
// BEFORE (manual instruction_template):
.instruction_template(|state| {
    let phase = state.get("session:phase").unwrap_or_default();
    let base = match phase.as_str() { ... };
    let emotion = state.get("emotional_state").unwrap_or(...);
    let willingness = state.get("willingness_to_pay").unwrap_or(0.5);
    // ... 15 lines of manual formatting ...
    Some(formatted)
})

// AFTER (declarative modifiers applied to each phase):
.phase("disclosure")
    .instruction(DISCLOSURE_INSTRUCTION)
    .with_state(&["emotional_state", "willingness_to_pay", "derived:call_risk_level"])
    .when(|s| s.get::<String>("derived:call_risk_level").unwrap_or_default() == "high",
          "IMPORTANT: Caller is showing distress. Use extra empathy.")
    .prompt_on_enter(true)
    .done()
```

**Runtime behavior**: During instruction composition (step 10b), the processor
applies modifiers in order:

1. Start with `phase.instruction.resolve(&state)` → base instruction
2. Apply each `InstructionModifier`:
   - `StateAppend` → reads keys, formats `[Context: k=v, ...]`, appends
   - `CustomAppend` → calls formatter, appends result
   - `Conditional` → checks predicate, appends text if true
3. Apply `instruction_amendment` (if registered) → appends
4. Apply `instruction_template` (if registered) → replaces entirely
5. Send composed result

This eliminates `instruction_template` for the common case. The template
becomes the escape hatch it was meant to be, not the default pattern.

### 3.4 Transcript Access in Phase Callbacks

**Layer**: L1 (processor.rs, phase.rs)
**Delta**: Pass a `TranscriptWindow` to `on_enter_context` (already shown
above) and optionally to `on_enter` / `on_exit`.

Current `on_enter` signature:
```rust
pub on_enter: Option<Arc<
    dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync
>>,
```

The `on_enter_context` (section 3.2.1) receives `TranscriptWindow`. For
backward compatibility, `on_enter` and `on_exit` keep their current
signatures — they're for side effects (UI notifications, state mutations),
not context construction. The `on_enter_context` is specifically for producing
context content.

**Building the `TranscriptWindow`**: The control lane holds
`Arc<parking_lot::Mutex<TranscriptBuffer>>`. Before calling `on_enter_context`,
the processor snapshots the last N turns:

```rust
let transcript_window = {
    let buf = transcript.lock();
    TranscriptWindow {
        turns: buf.window(5).to_vec(),
    }
};
```

This is cheap (clone of 5 small structs) and the mutex is held only for
the snapshot.

---

## 4. Conversation Control Patterns

These deltas enable a taxonomy of conversation control patterns — from tight
scripted flows to loose open-ended ones:

### 4.1 Tight Control (Scripted Phases)

**Pattern**: Disclosure scripts, compliance gates, identity verification.
The model must say specific things in a specific order.

```rust
.phase("disclosure")
    .instruction(DISCLOSURE_INSTRUCTION)
    .prompt_on_enter(true)       // Model speaks immediately
    .tools(&[])                  // No tools available
    .transition("verify_identity")
        .when(|s| s.get::<bool>("disclosure_given").unwrap_or(false))
    .done()
```

**What happens**:
1. Phase entered → instruction sent → turnComplete: true → model delivers disclosure
2. Extractor detects "disclosure_given" in transcript
3. Guard fires → transition to verify_identity
4. No tool calls allowed (empty tool filter)

### 4.2 Loose Control (Open Conversation)

**Pattern**: Negotiation, general Q&A. The model has freedom within guardrails.

```rust
.phase("negotiate")
    .instruction(NEGOTIATE_INSTRUCTION)
    .with_state(&["emotional_state", "willingness_to_pay", "derived:call_risk_level"])
    .when(|s| s.get::<f64>("willingness_to_pay").unwrap_or(0.0) > 0.7,
          "The debtor seems willing to pay. Push gently toward a plan.")
    .tools(&["calculate_payment_plan"])
    // No prompt_on_enter — wait for user
    .transition("arrange_payment")
        .when(|s| s.get::<bool>("payment_plan_agreed").unwrap_or(false))
    .transition("close")
        .when(|s| s.get::<bool>("cease_desist_requested").unwrap_or(false))
    .done()
```

**What happens**:
1. Phase entered → instruction + state context sent (turnComplete: false)
2. Model absorbs instruction silently, waits for user
3. Each turn: extractor updates emotional_state, willingness; computed updates risk
4. Instruction auto-recomposes with fresh state values
5. Model adapts tone based on `[Context: ...]` — no explicit prompting

### 4.3 Tool-Driven Phase Transitions

**Pattern**: Phase advances via tool result, not transcript extraction.

```rust
.phase("inform_debt")
    .instruction(INFORM_DEBT_INSTRUCTION)
    .tools(&["lookup_account"])
    .on_enter_context(|state, _transcript| {
        let name: String = state.get("debtor_name").unwrap_or_default();
        Some(vec![Content::user(format!(
            "[The customer {name} has been verified. Look up their account.]"
        ))])
    })
    .done()

// In on_tool_call:
.on_tool_call(|calls, state| async move {
    for call in &calls {
        if call.name == "lookup_account" {
            // Tool result gets promoted to state automatically via before_tool_response
            state.set("account_loaded", true);
        }
    }
    None // Use auto-dispatch
})
```

The transition guard checks `state.get("account_loaded")` — no LLM
extraction needed. This is the **non-LLM fast path**: tool result → state →
guard → transition, all within one TurnComplete cycle.

### 4.4 Agent Dispatch on Phase Entry

**Pattern**: Phase transition triggers a background agent (e.g., lookup,
summarization, parallel work).

```rust
.phase("inform_debt")
    .instruction(INFORM_DEBT_INSTRUCTION)
    .on_enter(|state, writer| async move {
        // Spawn background agent task
        let account_id: String = state.get("account_id").unwrap_or_default();
        tokio::spawn(async move {
            let result = external_api::lookup_account(&account_id).await;
            state.set("account_data", result);
            // Inject context once data arrives
            writer.send_client_content(
                vec![Content::user(format!(
                    "[Account data loaded: balance=${}, days past due={}]",
                    result.balance, result.days_past_due
                ))],
                false,
            ).await.ok();
        });
    })
    .done()
```

This uses `on_enter` (not `on_enter_context`) because it's async and
side-effectful. The context injection happens when the background task
completes, not at transition time.

---

## 5. Delta Summary — Layer by Layer

### 5.1 L0 (rs-genai) — No Changes

All proposed features compose over existing `SessionCommand` variants:
- `UpdateInstruction(String)` for instruction updates
- `SendClientContent { turns, turn_complete }` for context injection
- No new wire types needed

### 5.2 L1 (rs-adk) — Three Changes

| File | Delta | Lines (est.) |
|------|-------|-------------|
| `processor.rs` | Unify instruction composition into single send at step 10b | ~40 refactor |
| `phase.rs` | Add `on_enter_context`, `prompt_on_enter`, `TranscriptWindow` | ~60 new |
| `phase.rs` | Add `InstructionModifier` support per-phase | ~40 new |

**processor.rs changes**:
- Steps 6, 9, 10 accumulate into `resolved_instruction: Option<String>` instead
  of sending independently
- New step 10b: single `update_instruction` call
- New step 10c: send `on_enter_context` content if phase transitioned
- New step 10d: send turnComplete if `prompt_on_enter`
- Build `TranscriptWindow` snapshot before phase evaluation

**phase.rs changes**:
- `Phase` struct gains `on_enter_context`, `prompt_on_enter`, `modifiers` fields
- `TranscriptWindow` struct with `formatted()`, `last_user()`, `last_model()`
- Modifier application in `instruction.resolve_with_modifiers(&state)` or
  separate `apply_modifiers(base, &state)` function

### 5.3 L2 (adk-rs-fluent) — Three Changes

| File | Delta | Lines (est.) |
|------|-------|-------------|
| `live.rs` | Add `prompt_on_enter()`, `on_enter_context()` to `PhaseBuilder` | ~30 new |
| `live.rs` | Add `with_state()`, `when()` to `PhaseBuilder` | ~30 new |
| `compose/prompt.rs` | `P::with_state()`, `P::when()` factory methods | ~50 new |

---

## 6. Before & After: debt_collection.rs

### Before (current — ~80 lines of instruction_template)

```rust
// instruction_template re-implements phase lookup + manual state reads
.instruction_template(|state| {
    let phase: String = state.get("session:phase").unwrap_or_default();
    let risk: String = state.get("derived:call_risk_level").unwrap_or("low".into());

    let base = match phase.as_str() {
        "disclosure" => DISCLOSURE_INSTRUCTION,
        "verify_identity" => VERIFY_IDENTITY_INSTRUCTION,
        "inform_debt" => INFORM_DEBT_INSTRUCTION,
        "negotiate" => NEGOTIATE_INSTRUCTION,
        "arrange_payment" => ARRANGE_PAYMENT_INSTRUCTION,
        "confirm" => CONFIRM_INSTRUCTION,
        "close" => CLOSE_INSTRUCTION,
        _ => DISCLOSURE_INSTRUCTION,
    };

    let mut instruction = base.to_string();

    if risk == "high" || risk == "critical" {
        instruction.push_str("\n\nIMPORTANT: The caller is showing...");
    }

    let emotion: String = state.get("emotional_state").unwrap_or_else(|| "unknown".into());
    let willingness: f64 = state.get("willingness_to_pay").unwrap_or(0.5);
    let verified: bool = state.get("identity_verified").unwrap_or(false);
    let disclosed: bool = state.get("disclosure_given").unwrap_or(false);

    instruction.push_str(&format!(
        "\n\n[Context: Emotional state: {emotion}, Willingness: {willingness:.1}, ...]"
    ));

    Some(instruction)
})

// on_enter callbacks only do UI work — no model context
.on_enter(|state, _writer| async move {
    let _ = tx.send(ServerMessage::PhaseChange { ... });
    let _ = tx.send(ServerMessage::StateUpdate { ... });
})
```

### After (proposed — declarative per-phase)

```rust
.phase("disclosure")
    .instruction(DISCLOSURE_INSTRUCTION)
    .with_state(&["emotional_state", "willingness_to_pay",
                   "derived:call_risk_level", "identity_verified", "disclosure_given"])
    .when(|s| {
        let risk = s.get::<String>("derived:call_risk_level").unwrap_or_default();
        risk == "high" || risk == "critical"
    }, "IMPORTANT: The caller is showing signs of distress. Use extra empathy. \
        Never threaten, harass, or use deceptive language.")
    .prompt_on_enter(true)
    .on_enter(/* UI notifications — same as before */)
    .transition("verify_identity")
        .when(|s| s.get::<bool>("disclosure_given").unwrap_or(false))
    .done()

.phase("inform_debt")
    .instruction(INFORM_DEBT_INSTRUCTION)
    .with_state(&["emotional_state", "willingness_to_pay",
                   "derived:call_risk_level", "identity_verified"])
    .when(|s| /* high risk */, "IMPORTANT: ...")
    .tools(&["lookup_account"])
    .on_enter_context(|state, transcript| {
        let name: String = state.get("debtor_name").unwrap_or_default();
        Some(vec![Content::user(format!(
            "[{name}'s identity is verified. Look up their account and inform \
             them about the debt. Recent conversation:\n{}]",
            transcript.formatted()
        ))])
    })
    .on_enter(/* UI notifications */)
    .transition("negotiate")
        .when(|s| s.get::<bool>("debt_acknowledged").unwrap_or(false))
    .transition("close")
        .when(|s| s.get::<String>("negotiation_intent")
            .map(|i| i == "dispute").unwrap_or(false))
    .done()

// No instruction_template needed — modifiers handle it per-phase
// Result: ~40 lines of instruction_template eliminated
// Each phase self-describes its context needs
```

**What improves**:

1. **No double send** — instruction composition is unified
2. **Phase-local context** — each phase declares what state it needs, what
   conditions trigger extra instructions, and what conversational context to
   inject on entry
3. **No phase lookup duplication** — the phase machine already knows the
   current instruction; `with_state` appends to it
4. **Prompt-on-enter is declarative** — disclosure says `prompt_on_enter(true)`,
   negotiate says nothing (defaults to false)
5. **Transcript available** — `on_enter_context` can reference recent
   conversation for continuity

---

## 7. Interaction with Existing Primitives

### 7.1 S (State) — Unchanged

State continues as the central coordination point. The new `with_state()`
modifier reads from State at instruction-composition time. No State API
changes needed.

### 7.2 C (Context) — Deferred

`C` policies remain useful for text-based LLM calls (extractor prompts,
agent dispatch). For Live sessions, `on_enter_context` and `with_state()`
replace the need for Live-specific context policies. `C::from_state()` can
be deprecated for Live use cases once `with_state()` is implemented.

### 7.3 T (Tools) — Unchanged

Phase-scoped tool filtering continues to work via runtime rejection. No
interaction with context construction.

### 7.4 P (Prompt) — Extended

`P::with_state()` and `P::when()` are new prompt composition primitives.
They compose with the existing `instruction_template` and
`instruction_amendment` as the innermost layer (applied before amendments,
overridable by template).

### 7.5 M (Middleware) — Unchanged

Middleware observes the composed instruction via existing hooks. No changes
needed.

---

## 8. Implementation Priority

| Priority | Delta | Effort | Impact |
|----------|-------|--------|--------|
| **P0** | Unified instruction composition (3.1) | ~2h | Eliminates double-send bug |
| **P1** | `prompt_on_enter` (3.2.2) | ~1h | Fixes "how can I help" cold-start |
| **P1** | `on_enter_context` + TranscriptWindow (3.2.1, 3.4) | ~3h | Enables conversational continuity |
| **P2** | `P::with_state()` / `P::when()` modifiers (3.3) | ~3h | Eliminates instruction_template boilerplate |

P0 is a bug fix. P1 enables the core use case (natural phase transitions).
P2 is developer experience — it makes the common case elegant but doesn't
unlock new capabilities.

---

## 9. Non-Goals

- **Server-side context window manipulation**: We don't control Gemini's
  context window beyond `context_window_compression` config. Our context
  injection is additive.
- **Multi-turn prompt engineering**: This doc covers single-turn context at
  phase boundaries. Multi-turn prompt strategies (few-shot injection,
  chain-of-thought) are out of scope.
- **Agent orchestration**: Background agent dispatch patterns (4.4) are
  sketched but not designed in detail. That belongs in a separate agent
  architecture doc.
- **C module redesign for Live**: Deferred. The existing C module works for
  text-based calls. A Live-specific context module may be warranted later
  but is not needed for the patterns described here.

---

## Appendix A: Architectural Diagrams

### A.1 The Double Send Problem → Unified Composition

**Before (current)**:
```
TurnComplete Pipeline                          WebSocket Frames
━━━━━━━━━━━━━━━━━━━━                          ━━━━━━━━━━━━━━━━

Step 6: Phase transition fires
  └─ machine.transition("inform_debt")
     └─ writer.update_instruction(INFORM_DEBT) ──────► FRAME 1  ◄── WASTED
                                                        role: system
                                                        turnComplete: false

Step 9: instruction_amendment
  └─ (none registered, skip)

Step 10: instruction_template
  └─ template(&state) returns Some(...)
     └─ INFORM_DEBT + "\n\n[Context: emotion=frustrated, ...]"
        └─ writer.update_instruction(composed) ──────► FRAME 2  ◄── ACTUAL
                                                        role: system
                                                        turnComplete: false

Total: 2 frames sent, Frame 1 always overwritten by Frame 2
```

**After (unified)**:
```
TurnComplete Pipeline                          WebSocket Frames
━━━━━━━━━━━━━━━━━━━━                          ━━━━━━━━━━━━━━━━

Step 6: Phase transition fires
  └─ machine.transition("inform_debt")
     └─ resolved_instruction = INFORM_DEBT     ──── ► (held in memory)

Step 9: instruction_amendment
  └─ resolved_instruction += amendment         ──── ► (held in memory)

Step 10: instruction_template
  └─ resolved_instruction = template(&state)   ──── ► (held in memory)

Step 10b: SEND
  └─ dedup check against last_instruction
     └─ writer.update_instruction(resolved) ─────── ► FRAME 1  ◄── ONLY FRAME
                                                       role: system
                                                       turnComplete: false

Total: 1 frame sent
```

---

### A.2 The "How Can I Help You?" Problem → Phase Entry Context

**Before (cold-start)**:
```
Phase: verify_identity ──── guard fires ────► Phase: inform_debt

  ┌─────────────────────────────────────────────────────────────────────┐
  │                        GEMINI SERVER                                │
  │                                                                     │
  │  Context window:                                                    │
  │  ┌───────────────────────────────────────────────────────────────┐  │
  │  │ [system] "You need to verify the customer's identity..."     │  │
  │  │ [user]   "My name is Jane Smith, DOB March 15 1985"          │  │
  │  │ [model]  "Thank you Jane, let me verify that..."             │  │
  │  │ [tool]   verify_identity → { verified: true }                │  │
  │  │ [model]  "Your identity has been confirmed."                 │  │
  │  │                                                               │  │
  │  │  ┌─── INSTRUCTION UPDATE ARRIVES ───┐                        │  │
  │  │  │ [system] "Inform them about the  │                        │  │
  │  │  │  debt. Use lookup_account tool." │                        │  │
  │  │  └──────────────────────────────────┘                        │  │
  │  │                                                               │  │
  │  │  ... user speaks ...                                          │  │
  │  │                                                               │  │
  │  │  Model thinks: "New instruction. Fresh start."                │  │
  │  │  [model] "Hello! How can I help you today?"  ◄── WRONG       │  │
  │  └───────────────────────────────────────────────────────────────┘  │
  └─────────────────────────────────────────────────────────────────────┘

  Problem: No bridge between old phase and new phase.
           Model has new instruction but no continuity signal.
```

**After (with on_enter_context)**:
```
Phase: verify_identity ──── guard fires ────► Phase: inform_debt

  ┌─────────────────────────────────────────────────────────────────────┐
  │                        GEMINI SERVER                                │
  │                                                                     │
  │  Context window:                                                    │
  │  ┌───────────────────────────────────────────────────────────────┐  │
  │  │ [system] "You need to verify the customer's identity..."     │  │
  │  │ [user]   "My name is Jane Smith, DOB March 15 1985"          │  │
  │  │ [model]  "Thank you Jane, let me verify that..."             │  │
  │  │ [tool]   verify_identity → { verified: true }                │  │
  │  │ [model]  "Your identity has been confirmed."                 │  │
  │  │                                                               │  │
  │  │  FRAME 1 ─── instruction update                               │  │
  │  │  [system] "Inform them about the debt..."                     │  │
  │  │                                                               │  │
  │  │  FRAME 2 ─── on_enter_context (turnComplete: false)           │  │
  │  │  [user]   "[Jane's identity is verified. Proceed to inform    │  │
  │  │            them about the debt. Use lookup_account.]"          │  │
  │  │                                                               │  │
  │  │  ... user speaks ...                                          │  │
  │  │                                                               │  │
  │  │  Model thinks: "Jane is verified, I need to look up the debt" │  │
  │  │  [model] "Thank you Jane. Let me pull up your account..."     │  │
  │  │          ◄── CORRECT, continues naturally                     │  │
  │  └───────────────────────────────────────────────────────────────┘  │
  └─────────────────────────────────────────────────────────────────────┘
```

---

### A.3 Tight vs Loose Control — `prompt_on_enter`

```
TIGHT CONTROL: disclosure phase                LOOSE CONTROL: negotiate phase
(model must speak first)                       (model waits for user)

┌─────────────────────────┐                    ┌─────────────────────────┐
│  .prompt_on_enter(true) │                    │ .prompt_on_enter(false) │
│                         │                    │       (default)         │
│  FRAME 1: instruction   │                    │  FRAME 1: instruction   │
│    role: system         │                    │    role: system         │
│    turnComplete: false  │                    │    turnComplete: false  │
│           │             │                    │           │             │
│           ▼             │                    │           ▼             │
│  FRAME 2: context       │                    │  FRAME 2: context       │
│    role: user           │                    │    role: user           │
│    turnComplete: false  │                    │    turnComplete: false  │
│           │             │                    │           │             │
│           ▼             │                    │           ▼             │
│  FRAME 3: trigger       │                    │     (no frame 3)        │
│    turnComplete: true ──┼──► MODEL RESPONDS  │           │             │
│           │             │    IMMEDIATELY      │           │             │
│           ▼             │                    │     ┌─────┴─────┐       │
│  Model: "This is an    │                    │     │ WAITING   │       │
│  attempt to collect a  │                    │     │ for user  │       │
│  debt..."              │                    │     │ to speak  │       │
│                         │                    │     └───────────┘       │
│  User LISTENS first     │                    │  User LEADS the turn    │
└─────────────────────────┘                    └─────────────────────────┘
```

---

### A.4 State → Instruction Bridge

**Before (manual instruction_template)**:
```
                    instruction_template closure
                    ━━━━━━━━━━━━━━━━━━━━━━━━━━━━

State (DashMap)          Developer writes ALL of this manually
┌──────────────────┐     ┌─────────────────────────────────────────────┐
│ session:phase    │────►│ match phase.as_str() {                      │
│ emotional_state  │────►│   "disclosure" => DISCLOSURE_INSTRUCTION,   │
│ willingness_to_  │────►│   "verify_identity" => VERIFY_IDENTITY...,  │
│ derived:risk     │────►│   "inform_debt" => INFORM_DEBT...,          │
│ identity_verif.  │────►│   "negotiate" => NEGOTIATE...,              │
│ disclosure_given │────►│   "arrange_payment" => ARRANGE_PAYMENT...,  │
│ ...              │     │   "confirm" => CONFIRM...,                  │
│                  │     │   "close" => CLOSE...,                      │
│                  │     │ };                                           │
│                  │     │                                              │
│                  │     │ if risk == "high" { instruction += "..." }   │
│                  │     │                                              │
│                  │     │ let emotion = state.get("emotional_state")   │
│                  │     │ let willingness = state.get("willingness")   │
│                  │     │ let verified = state.get("identity_verified")│
│                  │     │ let disclosed = state.get("disclosure_given")│
│                  │     │                                              │
│                  │     │ instruction += format!("[Context: ...]")     │
│                  │     │                                              │
│                  │     │ Some(instruction)   // ~40 lines of this     │
└──────────────────┘     └─────────────────────────────────────────────┘
                                        │
                          DUPLICATES phase machine logic
                          (phase machine ALREADY knows the instruction)
```

**After (P::with_state + P::when per-phase)**:
```
                    Phase machine handles instruction lookup
                    with_state / when modifiers compose automatically
                    ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

State (DashMap)       Phase Definition                Composed Output
┌────────────────┐    ┌────────────────────┐          ┌──────────────────────┐
│ emotional_state│    │ .phase("negotiate")│          │                      │
│ willingness    │    │   .instruction(    │          │ "The customer has    │
│ derived:risk   │    │     NEGOTIATE_INST)│──base──► │  acknowledged the    │
│ identity_verif.│    │                    │          │  debt. Now work..."  │
│ disclosure_gvn │    │   .with_state(&[   │          │                      │
└───────┬────────┘    │     "emotional_..",├──read──► │ [Context:            │
        │             │     "willingness", │   &      │  emotional_state=    │
        │             │     "derived:risk",│  format  │  frustrated,         │
        │             │     "identity_..", │────────► │  willingness=0.3,    │
        │             │   ])               │          │  call_risk_level=    │
        │             │                    │          │  high, ...]          │
        │             │   .when(           │          │                      │
        │             │     |s| risk==high,│──check─► │ IMPORTANT: Caller    │
        │             │     "IMPORTANT:..")│   &      │ is showing distress. │
        │             │                    │ append   │ Use extra empathy.   │
        │             │ .done()            │          │                      │
        │             └────────────────────┘          └──────────────────────┘
        │
        │             NO instruction_template needed
        │             NO phase lookup duplication
        │             Each phase self-describes its context needs
```

---

### A.5 Full Phase Transition Pipeline — End to End

```
                    ┌─────────────────────┐
                    │   TurnComplete      │
                    │   event arrives     │
                    └────────┬────────────┘
                             │
              ┌──────────────▼──────────────┐
              │  1. Clear turn-scoped state  │
              │  2. Finalize transcript      │
              │  3. Snapshot watched keys     │
              │  4. Run extractors (OOB LLM) │
              │  5. Recompute derived state   │
              └──────────────┬───────────────┘
                             │
              ┌──────────────▼──────────────┐
              │  6. Evaluate phase guards    │
              │     guard(&state) == true?   │
              │              │               │
              │     ┌── YES ─┴─── NO ──┐     │
              │     ▼                  ▼     │
              │  transition()    skip phase  │
              │  ┌───────────┐   evaluation  │
              │  │ on_exit() │               │
              │  │ on_enter()│               │
              │  │ on_enter_ │               │
              │  │  context()│ ──► context[] │ ◄── NEW: transcript window
              │  │ resolve   │               │      available here
              │  │  instruct.│ ──► base_inst │
              │  └───────────┘               │
              └──────────────┬───────────────┘
                             │
              ┌──────────────▼───────────────┐
              │  7. Fire watchers             │
              │  8. Check temporal patterns   │
              └──────────────┬───────────────┘
                             │
     ┌───────────────────────▼───────────────────────────┐
     │             INSTRUCTION COMPOSITION               │
     │          (NEW — single composition point)          │
     │                                                    │
     │  base_inst ─── from phase (step 6)                │
     │       │                                            │
     │       ▼                                            │
     │  + with_state(&["emotion", "risk",...])  ◄── NEW  │
     │       │   appends [Context: k=v, ...]              │
     │       ▼                                            │
     │  + when(|s| risk == "high", "IMPORTANT:") ◄── NEW │
     │       │   conditionally appends                    │
     │       ▼                                            │
     │  + instruction_amendment (additive)                │
     │       │   appends developer text                   │
     │       ▼                                            │
     │  instruction_template (full override) ─── ESCAPE  │
     │       │   replaces everything if set      HATCH   │
     │       ▼                                            │
     │  ┌─────────────────────────┐                      │
     │  │ final_instruction: String│                      │
     │  └────────────┬────────────┘                      │
     └───────────────┼───────────────────────────────────┘
                     │
    ┌────────────────▼────────────────────────────────────┐
    │              WIRE SENDS (sequential)                │
    │                                                     │
    │  ┌─ dedup check ──────────────────────────────────┐ │
    │  │ if final_instruction != last_sent:             │ │
    │  │                                                │ │
    │  │  FRAME 1: update_instruction ──────────────► WS│ │
    │  │    { role: system, turnComplete: false }       │ │
    │  │                                                │ │
    │  │ if context[] from on_enter_context:            │ │
    │  │                                                │ │
    │  │  FRAME 2: send_client_content ─────────────► WS│ │
    │  │    { role: user, turnComplete: false }         │ │
    │  │                                                │ │
    │  │ if prompt_on_enter == true:                    │ │
    │  │                                                │ │
    │  │  FRAME 3: send_client_content ─────────────► WS│ │
    │  │    { turns: [], turnComplete: true }           │ │
    │  │    ──► MODEL GENERATES RESPONSE               │ │
    │  └───────────────────────────────────────────────┘ │
    └────────────────────────────────────────────────────┘
                     │
              ┌──────▼──────────────┐
              │ 11. Turn boundary   │
              │ 12. on_turn_complete│
              │ 13. Increment turn  │
              └─────────────────────┘
```

---

### A.6 Debt Collection State Machine — With New Primitives

```
┌─────────────────────────────────────────────────────────────────────┐
│                     DEBT COLLECTION FLOW                            │
│                                                                     │
│  ┌───────────┐   disclosed   ┌─────────────┐  verified  ┌────────┐ │
│  │DISCLOSURE │──────────────►│VERIFY       │───────────►│INFORM  │ │
│  │           │               │IDENTITY     │            │DEBT    │ │
│  │ prompt:   │               │             │            │        │ │
│  │  TRUE  ◄──── model talks  │ prompt:     │            │context:│ │
│  │           │    first       │  FALSE      │            │ "{name}│ │
│  │ tools: [] │               │             │            │  is    │ │
│  │           │               │ tools:      │            │verified│ │
│  │ state: [] │               │  [verify_id]│            │. Look  │ │
│  │           │               │             │            │up acct"│ │
│  └───────────┘               │ state:      │            │        │ │
│       │                      │  [emotion,  │            │ tools: │ │
│       │                      │   risk]     │            │ [look  │ │
│       │                      └─────────────┘            │  up]   │ │
│       │                                                  └───┬────┘ │
│       │                                                      │      │
│       │    cease &                  acknowledged              │      │
│       │    desist     ┌───────────┐◄─────────────────────────┘      │
│       │    ┌─────────►│NEGOTIATE  │                                  │
│       │    │          │           │  disputed                        │
│       │    │          │ prompt:   │──────────┐                       │
│       │    │          │  FALSE    │          │                       │
│       │    │          │           │          │                       │
│       │    │          │ state:    │          │                       │
│       │    │          │  [emotion,│          │                       │
│       │    │          │   willing,│          │                       │
│       │    │          │   risk]   │          │                       │
│       │    │          │           │          │                       │
│       │    │          │ when:     │          │                       │
│       │    │          │  risk=high│          │                       │
│       │    │          │  → empathy│          │                       │
│       │    │          │  warning  │          │                       │
│       │    │          │           │          │                       │
│       │    │          │ tools:    │          │                       │
│       │    │          │  [calc_   │          │                       │
│       │    │          │   plan]   │          │                       │
│       │    │          └─────┬─────┘          │                       │
│       │    │           agreed│               │                       │
│       │    │                ▼                ▼                       │
│       │    │     ┌──────────────┐     ┌───────────┐                 │
│       │    │     │ARRANGE       │     │           │                 │
│       │    │     │PAYMENT       │     │   CLOSE   │◄── terminal     │
│       │    │     │              │     │           │                 │
│       │    │     │ tools:       │     │ prompt:   │                 │
│       │    │     │  [process_   │     │  FALSE    │                 │
│       │    │     │   payment]   │     │           │                 │
│       │    │     └──────┬───────┘     └───────────┘                 │
│       │    │            │ processed         ▲                       │
│       │    │            ▼                   │                       │
│       │    │     ┌─────────────┐            │                       │
│       │    │     │CONFIRM      │────────────┘                       │
│       │    │     │             │  confirmed                         │
│       │    │     │ prompt:     │                                     │
│       │    │     │  FALSE      │                                     │
│       │    │     └─────────────┘                                     │
│       │    │                                                         │
│       └────┼──── ANY PHASE can transition to CLOSE via:              │
│            │      cease_desist_requested == true                     │
│            │      negotiation_intent == "dispute"                    │
│            └─────────────────────────────────────────────────────────│
└─────────────────────────────────────────────────────────────────────┘

LEGEND:
  prompt: TRUE   = model speaks first on entry (prompt_on_enter)
  prompt: FALSE  = model waits for user (default)
  state: [...]   = auto-appended via with_state()
  when: ...      = conditional instruction via P::when()
  context: "..." = on_enter_context injection
  tools: [...]   = phase-scoped tool filter
```

---

### A.7 Before vs After — Wire Traffic Per Phase Transition

```
BEFORE                                 AFTER
━━━━━━                                 ━━━━━

Phase guard fires                      Phase guard fires
       │                                      │
       ▼                                      ▼
┌──────────────────┐                   ┌──────────────────┐
│ FRAME 1 (wasted) │                   │ (held in memory) │
│ update_instruction│                  │ resolved = inst  │
│ bare phase inst  │                   └────────┬─────────┘
└────────┬─────────┘                            │
         │                              + with_state()
         ▼                              + when()
┌──────────────────┐                    + amendment
│ FRAME 2 (actual) │                            │
│ update_instruction│                           ▼
│ template output  │                   ┌──────────────────┐
│ inst + [Context] │                   │ FRAME 1 (only)   │
└────────┬─────────┘                   │ update_instruction│
         │                             │ composed result  │
         ▼                             └────────┬─────────┘
   (no context                                  │
    injection)                          if on_enter_context:
         │                                      ▼
         ▼                             ┌──────────────────┐
   (user speaks)                       │ FRAME 2 (bridge) │
         │                             │ send_client_cont │
         ▼                             │ "[Jane verified, │
┌──────────────────┐                   │  look up acct]"  │
│ "How can I help  │                   └────────┬─────────┘
│  you today?" ✗   │                            │
└──────────────────┘                    if prompt_on_enter:
                                                ▼
                                       ┌──────────────────┐
                                       │ FRAME 3 (trigger)│
                                       │ turnComplete:true │
                                       │ → model responds │
                                       └────────┬─────────┘
                                                │
                                                ▼
                                       ┌──────────────────┐
                                       │ "Thank you Jane. │
                                       │  Let me pull up  │
                                       │  your account."✓ │
                                       └──────────────────┘

 Frames: 2 (1 wasted)                  Frames: 1-3 (0 wasted)
 Context: none                          Context: state + transcript
 Model: cold start                      Model: natural continuation
```
