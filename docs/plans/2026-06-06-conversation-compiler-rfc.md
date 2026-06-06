# RFC: The Conversation Compiler — author intent, compile to governed machinery

Status: proposed · 2026-06-06 · supersedes nothing (extends the governed-agents
synthesis)

## Thesis

The runtime substrate (Flow · Extract · Resolver · Reactor over a shared `State`
spine) is strong. The missing layer is **product authoring**: developers should
describe a voice experience in terms of *slots, confirmations, interruptions,
repairs, digressions, commitments, and handoffs* — not hand-wire five low-level
abstractions and mentally coordinate extraction, navigation, grounding, tool
gating, repair, and voice timing.

> **Flow is the bytecode, not the authoring language.** A `Conversation` compiler
> lowers high-level intent into deterministic governed machinery
> (`CompiledFlow` + Extract plan + Resolver bindings + tool policy + reaction
> rules), and the runtime stays small, closed, and auditable.

This is a mental-model simplifier, not new runtime power. Adding more fluent
methods to `FlowBuilder` is a ~2× ergonomic win; a compiler is the ~100× move
because it turns a *systems* SDK into a *product* surface.

## Locked decisions

These two are settled and constrain everything below:

1. **The spec is a serializable data structure first.** `ConversationSpec` is a
   `serde` type. The typed Rust `Conversation` builder is *sugar* that produces it;
   YAML / hot-reload is then nearly free; a future proc-macro or NL-codegen merely
   *emit* the same struct; devtools merely *render* it. **One source of truth, no
   divergence.**
2. **Higher layers only ever emit lower-layer constructs — no privileged
   backdoors.** The compiler's output is hand-readable `Flow` + Extract +
   Resolver. Anything the compiler can express, a user could have written by hand.
   This is what keeps the whole stack auditable and is the invariant that prevents
   the compiler from becoming a second, divergent runtime.

## Three layers (all genuinely usable)

```
High   ConversationSpec  (serde; YAML/hot-reload; the product surface)
            │  compile()
Mid    Conversation       (typed Rust builder — sugar over the spec)
            │  lowers to
Low    Flow + Extract + Resolver + Reactor  (small, closed, auditable IR)
```

Flow remains usable by hand (the escape hatch and the compiler's own target);
its serializable, inspectable nature is its superpower and is preserved.

## Compile target

```rust
CompiledConversation {
    flow: CompiledFlow,          // validated governance/tool-gating IR
    extractors: ExtractPlan,     // recognizers + resolver field bindings
    resolver_bindings: ResolverPlan,
    tool_policy: ToolPolicy,     // every referenced tool + its gates
    reactor_rules: Vec<ReactionRule>,
    tests: FlowTestPlan,         // generated scenario/property fixtures
}
```

## The one-control-structure rule (critical architecture decision)

Today `PhaseMachine` (phases/transitions/guards/needs) and `Flow`
(steps/guards/constraints) already overlap. A hierarchical statechart on top
would make **three** ways to express "what happens next." We will **unify, not
stack**:

- `Conversation` is the *authored* layer.
- It lowers to `Flow` (governance + tool gating) plus a thin runtime
  **resume-stack** for overlays/digressions.
- `PhaseMachine` becomes a *lowering target*, not an authored concept — exactly
  what the synthesis ban-list already wants (`phase`/`transition` are lowering
  details). We do not add a third authored control structure.

## Frames & slots are first-class

Voice authors think in *frames*, not state keys. A slot carries a prompt,
reprompt, validator, recognizer, confidence/confirmation policy, canonicalization,
and PII/redaction policy.

```rust
#[derive(Frame)]
struct Booking {
    #[slot(prompt = "For how many people?", validate = range(1..=12), confirm = "low_confidence")]
    party_size: u8,
    #[slot(prompt = "What day and time?", recognizer = datetime, ask_after = "party_size")]
    slot: DateTime<Utc>,
    #[slot(prompt = "Name for the reservation?", pii, redact_in_logs)]
    name: String,
}
```

A `#[derive(Frame)]` lowers to: recognizers/resolvers (Extract), guards
(`captured`), grounding templates, navigation needs, repair prompts, typed
`StateKey<T>` constants, and test fixtures.

### Slot evidence (high value, low cost — pull forward)

The substrate already records the raw material: recognizers return
`(Value, confidence)`, `Resolver` writes `state_meta:{key}` provenance, and the
`State` mutation journal records seq/old/new/origin/timestamp. So:

```rust
state.meta("party_size").evidence()
// → { transcript span, source (stated/inferred/resolved), confidence,
//     provenance, last_updated_turn }
```

is mostly an **aggregation** over things that exist. It makes confirmations ("I
heard 6, right?") and repair (stale/low-confidence reopen) principled rather than
heuristic.

## Introspection: `explain()` / `why_blocked()` (killer feature)

The hardest voice bug is not "it crashed" — it is "why did the assistant ask
that?" The deterministic control plane can answer, model-readably, without
putting the model in charge:

```rust
flow.explain(&state)         // active steps, blocked tools + reasons, missing slots
flow.explain(&state).why_not("book")
handle.flow().why_blocked()  // human-readable repair-oriented summary
```

This ships in Phase 0 (over `CompiledFlow`) and delivers value before the compiler
exists.

## Phased plan (critical path first)

- **Phase 0 — keystone (this RFC's first commit):** `CompiledFlow` (validated IR +
  structured `FlowErrors` + a `ToolPolicy` artifact) and `FlowMonitor::explain()` /
  `why_blocked()`. Needed by *everything* downstream; valuable on its own.
- **Phase 1 — compiler MVP:** serializable `ConversationSpec` + `#[derive(Frame)]`
  slots + slot evidence + a compiler lowering `stage/collect/confirm/commit/next`
  → `CompiledFlow` + Extract + Resolver bindings.
- **Phase 2 — trust:** deterministic simulation harness (fake user + tool latency)
  + scenario/property tests; `explain()` everywhere.
- **Phase 3 — overlays (hard part, own RFC):** hierarchical digressions + a
  resumable `FlowStack`. Resume × in-progress-slots × tool-admission is subtle and
  warrants a dedicated design.
- **Phase 4 — product surface:** motif stdlib (collect/confirm-commit/identity/
  disclosure/FAQ/payment/handoff) + policy aspects (PCI/commit/safety) + voice
  timing as graph policy.
- **Demand-driven, later:** YAML hot-reload polish, visual devtools, NL-to-flow
  codegen, trace mining, the `voice_flow!` proc-macro (highest maintenance cost,
  lowest marginal value over the typed builder — last, if ever).

## Invariants (carry forward the hardening discipline)

- **Fail loud, never silently weaken.** Motifs and policy aspects encode
  safety-critical lowering (disclosure-before-commit, PCI redaction). A
  mis-lowered motif is the just-fixed custom-guard-erasure bug one altitude up:
  they must compile *through* the validated `CompiledFlow` and reject ambiguity.
- **Model-free where it matters.** The control plane is deterministic and
  testable without live API calls; the model reads explanations, it does not drive
  control flow. The model *may* draft a spec (authoring assistant), never govern
  the live session.

## Open questions (to resolve in Phase 1 / Phase 3)

1. Does `PhaseMachine` survive as an internal lowering target, or fold entirely
   into `Flow` + resume-stack?
2. Resume semantics: when a digression interrupts mid-slot, are partial slot
   values preserved, discarded, or re-confirmed on resume?
3. Tool admission during an overlay: does the overlay get its own allow/deny
   scope layered over the suspended step's?
4. Slot evidence retention vs the bounded mutation journal — is a per-slot
   provenance record durable, or recomputed from the journal window?
