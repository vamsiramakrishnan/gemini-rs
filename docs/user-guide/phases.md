# Phase System

The phase system models conversations as a state machine. Each phase carries its
own instruction, tool filter, and transition rules. The SDK evaluates transition
guards after every state mutation and automatically switches phases when
conditions are met -- updating the model's instruction, running lifecycle
callbacks, and filtering tools, all without manual wiring.

## What Are Phases?

A phase is a named conversation stage. A debt collection call might have:
`disclosure` -> `verify_identity` -> `inform_debt` -> `negotiate` -> `close`.
A support call might have: `greet` -> `identify` -> `investigate` -> `resolve` -> `close`.

Each phase defines:
- **Instruction** -- what the model should do in this stage
- **Transitions** -- guard conditions that trigger moves to other phases
- **Tools** -- which tools the model can call (optional filter)
- **Lifecycle callbacks** -- `on_enter` / `on_exit` hooks

## Defining Phases

Use the fluent `Live::builder()` API. Each `.phase()` call starts a
`PhaseBuilder` that returns to the main builder via `.done()`:

```rust,ignore
use adk_rs_fluent::prelude::*;

let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .phase("greeting")
        .instruction("Welcome the user warmly and ask how you can help.")
        .transition("main", |s| s.get::<bool>("greeted").unwrap_or(false))
        .done()
    .phase("main")
        .instruction("Handle the user's request.")
        .terminal()
        .done()
    .initial_phase("greeting")
    .connect_vertex(project, location, token)
    .await?;
```

Key points:
- `.initial_phase("greeting")` declares which phase the machine starts in.
- `.terminal()` marks a phase with no outbound transitions.
- The machine is validated at connect time -- missing initial phase or dangling
  transition targets produce clear errors.

## Phase Transitions

Transitions are guard-based: a closure that receives `&State` and returns `bool`.
When the guard returns `true`, the machine transitions to the target phase.

```rust,ignore
.phase("disclosure")
    .instruction(DISCLOSURE_INSTRUCTION)
    // When disclosure_given becomes true, move to verify_identity
    .transition("verify_identity", |s| {
        s.get::<bool>("disclosure_given").unwrap_or(false)
    })
    // Emergency exit: cease-and-desist goes straight to close
    .transition("close", |s| {
        s.get::<bool>("cease_desist_requested").unwrap_or(false)
    })
    .done()
```

Transitions are evaluated in order -- the first guard that returns `true` wins.
This means you should order transitions from most specific to most general.

### Transition Descriptions

Use `transition_with()` to add a human-readable description to each transition.
These descriptions are used by the phase navigation context (see below) to give
the model awareness of where it can go and why:

```rust,ignore
.phase("identify_caller")
    .instruction("Get the caller's full name and organization.")
    .transition_with("determine_purpose", |s| {
        s.get::<String>("caller_name").is_some()
    }, "when caller provides their name")
    .transition_with("take_message", |s| {
        let tc: u32 = s.session().get("turn_count").unwrap_or(0);
        tc >= 12
    }, "after 12 turns if caller refuses to identify")
    .done()
```

The plain `.transition()` method still works and sets `description: None`.

### S:: Predicates

The `S` module provides ergonomic predicate factories that eliminate boilerplate:

```rust,ignore
use adk_rs_fluent::prelude::S;

.transition("verify_identity", S::is_true("disclosure_given"))
.transition("negotiate", S::is_true("debt_acknowledged"))
.transition("arrange_payment", S::one_of("negotiation_intent", &["full_pay", "partial_pay"]))
.transition("tech_support", S::eq("issue_type", "technical"))
```

Available predicates:
- `S::is_true(key)` -- key holds `true`
- `S::eq(key, value)` -- key equals the given string
- `S::one_of(key, &[values])` -- key matches any of the given strings

## Guards

A phase-level guard prevents the phase from being entered unless a condition is
met. This is different from transition guards -- a transition guard decides when
to *leave* a phase, while a phase guard decides whether the phase can be
*entered*.

```rust,ignore
.phase("verify_identity")
    .instruction(VERIFY_IDENTITY_INSTRUCTION)
    // This phase can only be entered after disclosure is acknowledged
    .guard(S::is_true("disclosure_given"))
    .transition("inform_debt", S::is_true("identity_verified"))
    .done()
```

If a transition guard fires but the target phase's guard returns `false`, the
machine skips that transition and evaluates the next one in order.

## Dynamic Instructions

For instructions that depend on runtime state, use `dynamic_instruction`:

```rust,ignore
.phase("discuss")
    .dynamic_instruction(|s| {
        let topic: String = s.get("topic").unwrap_or_default();
        let mood: String = s.get("derived:sentiment").unwrap_or_default();
        format!("Discuss {topic}. The user's mood is {mood}. Adjust tone accordingly.")
    })
    .done()
```

The closure is evaluated at transition time, so the instruction always reflects
current state.

## Instruction Modifiers

Modifiers append context to a phase's instruction without replacing it. Three
types are available:

### StateAppend -- Inject Key/Value Context

```rust,ignore
.phase("negotiate")
    .instruction("Help the customer resolve their debt.")
    // Appends: [Context: emotional_state=frustrated, willingness_to_pay=0.3]
    .with_state(&["emotional_state", "willingness_to_pay", "derived:call_risk_level"])
    .done()
```

### Conditional -- Append Text When True

```rust,ignore
fn risk_is_elevated(s: &State) -> bool {
    let risk: String = s.get("derived:call_risk_level").unwrap_or_default();
    risk == "high" || risk == "critical"
}

.phase("negotiate")
    .instruction("Help the customer resolve their debt.")
    .when(risk_is_elevated, "IMPORTANT: Use extra empathy. Never threaten.")
    .done()
```

### CustomAppend -- Arbitrary Formatting

```rust,ignore
.phase("investigate")
    .instruction("Investigate the issue.")
    .with_context(|state| {
        let items: Vec<String> = state.get("order_items").unwrap_or_default();
        if items.is_empty() {
            String::new()
        } else {
            format!("Current order: {}", items.join(", "))
        }
    })
    .done()
```

## Tool Filtering

Each phase can restrict which tools the model is allowed to call. Tool calls for
tools not in the filter are rejected by the processor:

```rust,ignore
.phase("verify_identity")
    .instruction("Verify the caller's identity.")
    .tools(vec!["verify_identity".into(), "log_compliance_event".into()])
    .done()
.phase("negotiate")
    .instruction("Negotiate a payment plan.")
    .tools(vec!["calculate_payment_plan".into(), "log_compliance_event".into()])
    .done()
```

Omitting `.tools()` means all registered tools are available in that phase.

## Phase Needs

Declare what state keys a phase requires. The SDK uses these to generate the
navigation context (see below), showing the model what information is still
missing. This helps guide the conversation without over-constraining the LLM:

```rust,ignore
.phase("identify_caller")
    .instruction("Get the caller's full name and organization.")
    .needs(&["caller_name", "caller_org"])
    .transition_with("determine_purpose", |s| {
        s.get::<String>("caller_name").is_some()
    }, "when caller provides their name")
    .done()
```

At runtime, `needs` are filtered against the current state -- only keys not
yet present are shown as "still needed" in the navigation context.

## Phase Navigation Context

The `.navigation()` modifier (available on both `PhaseBuilder` and
`phase_defaults`) injects a structured description of the phase graph into the
model's instruction. This gives the model geolocation awareness:

```
[Navigation]
Current phase: identify_caller -- Get the caller's full name and organization.
Previous: greeting (turn 2)
Still needed: caller_org
Possible next:
  -> determine_purpose: when caller provides their name
  -> take_message: after 12 turns if caller refuses to identify
```

This is auto-generated from `.needs()` keys filtered by state, `.transition_with()`
descriptions, and phase history. Apply it via `phase_defaults` so all phases
benefit:

```rust,ignore
Live::builder()
    .phase_defaults(|d| d
        .with_state(&["caller_name", "caller_org"])
        .navigation()  // inject navigation context into every phase
    )
```

The navigation context is stored in `session:navigation_context` and regenerated
on every turn and phase transition.

## Phase Lifecycle Callbacks

`on_enter` and `on_exit` are async callbacks that run during transitions:

```rust,ignore
.phase("verify_identity")
    .instruction(VERIFY_IDENTITY_INSTRUCTION)
    .on_enter(|state, writer| async move {
        // Log the transition, initialize phase-specific state
        state.set("verification_attempts", 0u32);
        tracing::info!("Entered verify_identity phase");
    })
    .on_exit(|state, writer| async move {
        // Clean up, log compliance event
        tracing::info!("Exiting verify_identity phase");
    })
    .done()
```

The callbacks receive `State` and `Arc<dyn SessionWriter>`, so you can both
mutate state and send messages to the model.

### enter_prompt -- Model Speaks on Entry

Use `enter_prompt` to inject a model-role bridge message and prompt the model to
respond immediately when entering a phase. This prevents the "cold start" problem
where the model says "how can I help?" after a phase transition:

```rust,ignore
.phase("verify_identity")
    .instruction(VERIFY_IDENTITY_INSTRUCTION)
    // Model will say this, then continue with the phase instruction
    .enter_prompt("The caller confirmed the disclosure. I'll now verify their identity.")
    .done()
```

For state-dependent prompts, use `enter_prompt_fn`:

```rust,ignore
.phase("close")
    .instruction(CLOSE_INSTRUCTION)
    .enter_prompt_fn(|state, _tw| {
        if state.get::<bool>("cease_desist_requested").unwrap_or(false) {
            "Cease-and-desist requested. Closing call respectfully.".into()
        } else {
            "Wrapping up the call.".into()
        }
    })
    .done()
```

## Phase Defaults

Settings shared across all phases are declared with `phase_defaults`. These are
merged into each phase -- phase-specific modifiers extend (not replace) the
defaults:

```rust,ignore
const DEBT_STATE_KEYS: &[&str] = &[
    "emotional_state",
    "willingness_to_pay",
    "derived:call_risk_level",
    "identity_verified",
    "disclosure_given",
];

Live::builder()
    .phase_defaults(|d| d
        .with_state(DEBT_STATE_KEYS)
        .when(risk_is_elevated, "IMPORTANT: Use extra empathy.")
        .prompt_on_enter(true)
    )
    .phase("disclosure")
        .instruction(DISCLOSURE_INSTRUCTION)
        // Inherits with_state, when(), and prompt_on_enter from defaults
        .done()
    .phase("negotiate")
        .instruction(NEGOTIATE_INSTRUCTION)
        // Phase-specific modifier is appended after defaults
        .with_state(&["negotiation_intent"])
        .done()
```

## Multi-Phase Example

A 3-phase conversation (greeting -> service -> close) combining the features
covered above:

```rust,ignore
Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .greeting("Greet the user warmly.")
    .phase_defaults(|d| d
        .with_state(&["customer_name", "derived:sentiment"])
        .prompt_on_enter(true)
    )
    .phase("greeting")
        .instruction("Welcome the customer. Ask for their name.")
        .transition("service", |s| s.contains("customer_name"))
        .done()
    .phase("service")
        .instruction("Help the customer with their request.")
        .guard(|s| s.contains("customer_name"))
        .tools(vec!["lookup_account".into(), "process_refund".into()])
        .transition("close", S::is_true("resolved"))
        .when(|s| s.get::<String>("derived:sentiment").unwrap_or_default() == "negative",
              "The customer seems upset. Use extra empathy.")
        .enter_prompt("I have the customer's name. I'll help them now.")
        .done()
    .phase("close")
        .instruction("Thank the customer and wrap up.")
        .terminal()
        .done()
    .initial_phase("greeting")
    .connect_vertex(project, location, token)
    .await?;
```

For a full 7-phase example with compliance gates, computed state, watchers, and
temporal patterns, see `apps/adk-web/src/apps/debt_collection.rs`.

## How Transitions Are Evaluated

The processor evaluates transitions after every state mutation cycle (extractors
run, computed variables update, watchers fire). The evaluation is pure -- it
checks guards without side effects:

1. Get the current phase's transition list.
2. For each transition (in order), check the guard.
3. If the guard returns `true`, check the target phase's guard (if any).
4. If both pass, execute the transition: `on_exit` -> update current -> `on_enter`.
5. The new phase's instruction (with modifiers applied) is sent to the model.

Terminal phases skip transition evaluation entirely.

For the full turn-complete pipeline, timing diagrams, background agent
dispatch, and common pitfalls, see [Phase Transitions Deep Dive](phase-transitions-deep-dive.md).
