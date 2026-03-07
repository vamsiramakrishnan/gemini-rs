# Steering Modes

How the SDK delivers phase instructions and per-turn context to the model during a Live session. This is the single most impactful configuration choice for multi-phase voice applications.

## The Three Modes

### ContextInjection (recommended)

The system instruction is set **once at connect time** and never updated. Phase instructions and per-turn modifiers are delivered as model-role context turns via `send_client_content`.

```rust,ignore
Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .instruction("You are a restaurant reservation assistant at Sapore d'Italia.")
    .steering_mode(SteeringMode::ContextInjection)
    .phase("greeting")
        .instruction("Welcome the guest warmly and ask how you can help.")
        .done()
    .phase("booking")
        .instruction("Help the guest find an available time slot.")
        .done()
    .initial_phase("greeting")
```

**What happens on phase transition:**
1. The phase instruction ("Welcome the guest...") is sent as a model-role content turn
2. Per-turn modifiers (`with_context`, `with_state`, `when`) are also sent as model-role turns
3. The system instruction ("You are a restaurant...") is **never touched**

**When to use:** Most multi-phase voice apps. The base persona stays stable across phases, and phase-specific behavior is guided through conversational context. Lower latency, no instruction re-processing spikes.

### InstructionUpdate (default)

The system instruction is **replaced** on every phase transition. Per-turn modifiers are baked into the instruction text.

```rust,ignore
Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .instruction("You are a helpful assistant.")
    .steering_mode(SteeringMode::InstructionUpdate)  // this is the default
    .phase("receptionist")
        .instruction("You are a medical receptionist. Schedule appointments.")
        .done()
    .phase("triage_nurse")
        .instruction("You are a triage nurse. Assess symptom severity.")
        .done()
    .initial_phase("receptionist")
```

**What happens on phase transition:**
1. The entire system instruction is replaced with the new phase's instruction
2. Per-turn modifiers are appended to the instruction text
3. The model re-processes its full context with the new instruction

**When to use:** When phases represent genuinely different personas or roles. The model needs a complete context reset to shift behavior convincingly.

### Hybrid

System instruction is replaced on phase transition (like `InstructionUpdate`), but per-turn modifiers are delivered as model-role context turns (like `ContextInjection`).

```rust,ignore
Live::builder()
    .steering_mode(SteeringMode::Hybrid)
    .phase("sales")
        .instruction("You are a sales representative.")
        .with_context(|s| format!("Customer budget: {}", s.get::<String>("budget").unwrap_or_default()))
        .done()
    .phase("support")
        .instruction("You are a technical support engineer.")
        .with_context(|s| format!("Ticket: {}", s.get::<String>("ticket_id").unwrap_or_default()))
        .done()
```

**When to use:** When you need persona shifts on transition but also want lightweight per-turn context updates within each phase. Uncommon in practice -- pick `ContextInjection` or `InstructionUpdate` unless you have a specific reason for both.

## Decision Matrix

| Question | Yes | No |
|----------|-----|-----|
| Does the model's core persona change between phases? | `InstructionUpdate` | `ContextInjection` |
| Is latency on phase transitions a concern? | `ContextInjection` | Either works |
| Do you need per-turn dynamic context (state summaries, conditional hints)? | `ContextInjection` or `Hybrid` | `InstructionUpdate` is fine |
| Are phases just different *stages* of the same conversation? | `ContextInjection` | -- |
| Are phases genuinely different *agents* (receptionist vs doctor)? | `InstructionUpdate` | -- |

## Anti-Patterns

### Using InstructionUpdate for minor context changes

**Problem:** Every phase has the same persona but slightly different focus areas. Using `InstructionUpdate` causes unnecessary instruction re-processing latency on each transition.

```rust,ignore
// Anti-pattern: same persona, different focus -- InstructionUpdate is overkill
Live::builder()
    .steering_mode(SteeringMode::InstructionUpdate)  // unnecessary latency
    .phase("gather_name")
        .instruction("You are a restaurant host. Ask for the guest's name.")
        .done()
    .phase("gather_party_size")
        .instruction("You are a restaurant host. Ask for the party size.")
        .done()
```

**Fix:** Use `ContextInjection`. The base persona is set once, and phase-specific focus is delivered as context turns.

```rust,ignore
// Better: stable persona, lightweight phase steering
Live::builder()
    .instruction("You are a friendly host at Sapore d'Italia.")
    .steering_mode(SteeringMode::ContextInjection)
    .phase("gather_name")
        .instruction("Ask for the guest's name for the reservation.")
        .done()
    .phase("gather_party_size")
        .instruction("Ask how many guests will be dining.")
        .done()
```

### Using ContextInjection when personas differ radically

**Problem:** Phases represent genuinely different agent personas (e.g., switching from a receptionist to a clinical nurse). Context injection is too subtle -- the model may not fully shift behavior.

```rust,ignore
// Anti-pattern: radically different personas via context injection
Live::builder()
    .instruction("You work at a medical clinic.")
    .steering_mode(SteeringMode::ContextInjection)  // too subtle for persona shift
    .phase("receptionist")
        .instruction("You are the front desk receptionist. Be warm and administrative.")
        .done()
    .phase("triage")
        .instruction("You are a clinical triage nurse. Be precise and medical.")
        .done()
```

**Fix:** Use `InstructionUpdate` so the model gets a clean persona reset.

### Over-engineering with Hybrid

**Problem:** Using `Hybrid` when `ContextInjection` alone would suffice. Adds complexity without benefit.

```rust,ignore
// Anti-pattern: Hybrid when the persona doesn't actually change
Live::builder()
    .steering_mode(SteeringMode::Hybrid)  // unnecessary complexity
    .phase("greeting").instruction("Welcome the user.").done()
    .phase("main").instruction("Help with their request.").done()
```

**Fix:** Use `ContextInjection`. If the persona is stable, there's no reason to replace the system instruction.

### Putting volatile state in the base instruction

**Problem:** The base instruction (set at connect time) includes dynamic state that changes every turn. With `ContextInjection`, this instruction is never updated.

```rust,ignore
// Anti-pattern: dynamic content in the base instruction
Live::builder()
    .instruction(format!("You are helping {}. Their order has {} items.",
        customer_name, order_count))  // stale after the first turn
    .steering_mode(SteeringMode::ContextInjection)
```

**Fix:** Keep the base instruction static. Use `with_context()` modifiers for dynamic state.

```rust,ignore
// Better: static base, dynamic context via modifiers
Live::builder()
    .instruction("You are a helpful order assistant.")
    .steering_mode(SteeringMode::ContextInjection)
    .phase_defaults(|d| d.with_context(|s| {
        format!("Customer: {}. Items in order: {}.",
            s.get::<String>("customer_name").unwrap_or_default(),
            s.get::<u32>("order_count").unwrap_or(0))
    }))
```

## How It Works Under the Hood

The three-lane processor evaluates steering at two points in the turn lifecycle:

```
  TurnComplete event
       |
  [Step 7]  Phase machine evaluates transitions
       |    --> if transition fires, resolved_instruction is set
       |
  [Step 7f] Context injection (ContextInjection / Hybrid modes)
       |    --> per-turn modifiers sent as model-role Content::model() turns
       |
  [Step 12] Instruction delivery (gated by SteeringMode)
       |    --> InstructionUpdate/Hybrid: writer.update_instruction()
       |    --> ContextInjection: writer.send_client_content()
       |
  [Step 13] on_enter_context (phase transition context)
  [Step 14] prompt_on_enter (triggers model response)
```

The key insight: with `ContextInjection`, step 12 sends the phase instruction as `Content::model(instruction_text)`. The model sees it as its own prior speech, which naturally steers its behavior without the overhead of system instruction replacement.

## Interaction with Other Features

| Feature | InstructionUpdate | ContextInjection | Hybrid |
|---------|-------------------|------------------|--------|
| `with_context(fn)` | Appended to instruction text | Sent as model-role turn | Sent as model-role turn |
| `with_state(&[keys])` | Baked into instruction | Sent as model-role turn | Sent as model-role turn |
| `when(pred, text)` | Baked into instruction | Sent as model-role turn | Sent as model-role turn |
| `instruction_amendment` | Appended to instruction | Appended to context turn | Appended to instruction |
| `instruction_template` | Replaces instruction | Sent as context turn | Replaces instruction |
| `navigation()` | Baked into instruction | Baked into instruction | Baked into instruction |
| `greeting()` | Works normally | Works normally | Works normally |
| `prompt_on_enter` | Works normally | Works normally | Works normally |
| `enter_prompt` | Works normally | Works normally | Works normally |
