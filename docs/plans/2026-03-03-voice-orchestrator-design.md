# Voice Orchestrator — Gemini Live as Agent Dispatcher

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Date**: 2026-03-03
**Status**: Design RFC
**Scope**: Bridge live voice sessions to text-mode agent pipelines via tool dispatch and event hooks
**Complements**: `primitives-architecture-audit.md` (five-primitive taxonomy), `voice-native-context-construction.md` (instruction composition), `callback-mode-design.md` (execution semantics)
**Reference**: Python `adk-fluent` patterns (Route, Dispatch/Join, AgentTool), upstream ADK `AgentTool`

---

## Executive Summary

Gemini Live excels at real-time, full-duplex voice conversation. But voice models
are poor at multi-step reasoning, arithmetic, database queries, and any task
requiring more than one LLM round-trip. Today, our Live sessions can call tools
(pure functions), but they cannot delegate to **agent pipelines** — multi-step
LLM workflows with their own tool loops, routing, and composition.

This document designs the **voice orchestrator** pattern: Gemini Live acts as the
conversational front-end, dispatching complex reasoning tasks to text-mode
`TextAgent` pipelines. Results flow back through the tool response channel
(synchronous) or through state mutations + watchers (asynchronous).

```
┌─────────────────────────────────────────────────────────────┐
│                    GEMINI LIVE SESSION                       │
│              (voice-native, real-time, full-duplex)          │
│                                                             │
│  Phases ──► Instructions ──► Model speaks                   │
│                                                             │
│  ┌──────────────── TOOL DISPATCH BOUNDARY ───────────────┐  │
│  │                                                       │  │
│  │  ┌──────────────────┐    ┌────────────────────────┐   │  │
│  │  │ TextAgentTool    │    │ BackgroundDispatcher   │   │  │
│  │  │ (sync, blocking) │    │ (async, fire-forget)   │   │  │
│  │  └────────┬─────────┘    └────────────┬───────────┘   │  │
│  └───────────┼───────────────────────────┼───────────────┘  │
└──────────────┼───────────────────────────┼──────────────────┘
               │                           │
               ▼                           ▼
┌──────────────────────┐    ┌─────────────────────────────────┐
│ TEXT AGENT PIPELINE   │    │ BACKGROUND AGENT PIPELINE       │
│                       │    │                                 │
│ LlmTextAgent          │    │  Sequential:                    │
│   → tool call loop    │    │    EnrichAgent >> ScoreAgent    │
│   → multi-step reason │    │                                 │
│   → returns String    │    │  Results → State → Watcher      │
│   → tool response     │    │  → instruction modifier         │
│   → model continues   │    │  → model informed next turn     │
└───────────────────────┘    └─────────────────────────────────┘
```

### Why This Matters

| Capability | Without orchestrator | With orchestrator |
|---|---|---|
| Account lookup | Single tool call, model interprets raw JSON | Agent pipeline: lookup → enrich → format natural summary |
| Payment calculation | Pure function returns numbers | Agent reasons about debtor situation, generates empathetic options |
| Compliance check | Static rules in instruction | Background agent analyzes full transcript each turn |
| Multi-step verification | Model must chain tool calls (unreliable in voice) | Agent pipeline handles retries and fallbacks |
| Routing by intent | Phase guards (limited to boolean state) | Route agent dispatches to specialist pipelines |

---

## Gap Analysis: Current State vs. Target

### What Exists Today

| Component | Location | Status |
|---|---|---|
| `TextAgent` trait + 13 implementations | `crates/gemini-adk-rs/src/text.rs` | Complete |
| `AgentTool` (wraps `Agent` as `ToolFunction`) | `crates/gemini-adk-rs/src/agent_tool.rs` | Works but wraps live `Agent`, not `TextAgent` |
| `Composable` operators (`>>`, `\|`, `*`, `/`) | `crates/gemini-adk-fluent-rs/src/operators.rs` | Complete |
| `compile()` IR → `TextAgent` | `crates/gemini-adk-fluent-rs/src/operators.rs` | Complete |
| `ToolDispatcher` with auto-dispatch | `crates/gemini-adk-rs/src/tool.rs` | Complete |
| `on_tool_call` callback with `State` | `crates/gemini-adk-rs/src/live/callbacks.rs:59` | Complete |
| Phase machine + transition guards | `crates/gemini-adk-rs/src/live/phase.rs` | Complete |
| Watchers (state change reactions) | `crates/gemini-adk-rs/src/live/watcher.rs` | Complete |
| `InstructionModifier` system | `crates/gemini-adk-rs/src/live/phase.rs:62` | Complete |

### What's Missing (Gaps)

#### Gap 1: `TextAgentTool` — the critical bridge (P0)

**Problem**: `AgentTool` wraps `Agent` (which has `run_live()` requiring a WebSocket
context). There is no way to wrap a `TextAgent` (which uses `BaseLlm::generate()`,
request-response) as a `ToolFunction` for the live session's `ToolDispatcher`.

**Impact**: Cannot dispatch multi-step text reasoning from a voice session. The
entire orchestrator pattern is blocked.

**What Python ADK has**: `AgentTool` wraps any agent type. The wrapped agent runs
in isolation — its own session, no WebSocket. Text output becomes the tool response.

**What we need**: A `TextAgentTool` that:
1. Takes a `TextAgent` (or compiled `Composable` pipeline)
2. Implements `ToolFunction`
3. On `call()`: copies relevant state, runs the text pipeline, returns result
4. Optionally promotes state mutations back to the parent session

#### Gap 2: Fluent `.agent_tool()` registration (P0)

**Problem**: Even with `TextAgentTool`, registering it requires manual construction.
No fluent API bridges the `Live` builder to text agent composition.

**What we need**:
```rust
Live::builder()
    .agent_tool("lookup", "Look up account details",
        lookup_agent >> enrich_agent >> format_agent
    )
```

#### Gap 3: Background agent dispatch (P1)

**Problem**: No way to fire-and-forget an agent pipeline from a live callback.
All tool calls are synchronous (model waits for response). Background work
(compliance checking, enrichment, scoring) has no dispatch mechanism.

**What Python ADK has**: `dispatch(*agents)` fires background `asyncio.Task`s,
`join()` waits. Uses `ContextVar` for per-invocation tracking and global task
budget semaphore.

**What we need**: `BackgroundAgentDispatcher`:
1. Spawns `TextAgent` on a `tokio` task
2. Results write to `State` under a namespaced key
3. Watchers can react to results arriving
4. Optional task budget (semaphore) to prevent explosion
5. Cancellation on session disconnect

#### Gap 4: State handoff protocol (P1)

**Problem**: When a `TextAgentTool` runs, which state keys does it see? Which
mutations propagate back? Today, `AgentTool` creates a completely isolated
`AgentSession` — no state sharing.

**What we need**: Configurable state handoff:
- **Input projection**: Which parent state keys the child sees (default: all)
- **Output promotion**: Which child state keys propagate back (default: none except tool result)
- **Isolation levels**: `full` (copy everything), `projected` (selected keys), `isolated` (nothing)

#### Gap 5: `ToolFunction::call()` lacks `State` parameter (P1)

**Problem**: The `ToolFunction` trait signature is:
```rust
async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError>;
```

No `State` access. `TextAgentTool` needs state to pass context to the child agent.
Currently the live processor passes state to `on_tool_call` but not to individual
`ToolFunction::call()`.

**Options**:
1. **Add `State` parameter** to `ToolFunction::call()` — breaking change, clean
2. **Inject state via constructor** — `TextAgentTool` captures `State` clone at creation
3. **Use `Arc<State>` in ToolDispatcher** — dispatcher passes state internally

**Recommendation**: Option 3. Extend `ToolDispatcher::call_function()` to accept
optional `State` reference. Pass it through from the processor. Non-state-aware
tools ignore it. `TextAgentTool` uses it.

#### Gap 6: P module doesn't bridge to TextAgent instructions (P2)

**Problem**: `P::role()`, `P::task()` compose `PromptComposite` for live phase
instructions. They can't build instructions for `LlmTextAgent`.

**What we need**: `PromptComposite::render()` already returns a `String`. Just
need an ergonomic bridge:
```rust
let agent = LlmTextAgent::new("helper", flash)
    .instruction(P::role("analyst") + P::task("analyze data"));
```

This requires `LlmTextAgent::instruction()` to accept `impl Into<String>` OR
`PromptComposite`. Simplest: implement `Into<String>` for `PromptComposite`.

#### Gap 7: No evaluation framework (P3)

Python has `E` module with inline eval, eval suites, LLM-as-judge, trajectory
matching. We have nothing. Not blocking for orchestrator, but needed for
validating agent pipeline quality.

---

## The Five Primitives Across Voice and Text Modes

Each primitive (S, C, T, P, M, plus A) operates in **both** the voice session and
the dispatched text agent, but with different strengths, constraints, and roles.
The orchestrator pattern's power comes from **cross-leveraging** — using each
primitive where it's strongest.

### Primitive Comparison Matrix

| Primitive | Voice Mode (Gemini Live) | Text Mode (TextAgent) | Cross-Leverage |
|---|---|---|---|
| **S — State** | Continuous: extractors, watchers, computed vars update every turn. State is the central nervous system. | Transactional: agent reads state at start, writes at end. Clean input → output. | Voice extracts → Text reads. Text writes → Voice watchers fire. State is the **bridge**. |
| **C — Context** | Server-managed history. Can only inject via `send_client_content()`. Cannot replay. | Client-managed: full conversation rebuilt each `generate()` call. Can curate, filter, summarize. | Text agents can build **perfect context** (window, filter, summarize) which voice cannot. Use text agents for context-heavy reasoning. |
| **T — Tools** | Immutable after setup. Phase filtering is runtime rejection. Tool calls are interruptible. | Mutable per-request. Multi-round tool loops (up to 10 rounds). Sequential dispatch. | Register text agent **pipelines as tools**. Voice model invokes; pipeline handles multi-step. |
| **P — Prompt** | Single `system_instruction` updated via `update_instruction()`. Modifiers compose dynamically. | Per-request system instruction. Full control. Can use P module composition. | Voice uses P for **steering** (phase instructions + modifiers). Text uses P for **deep reasoning** (detailed analyst prompts). |
| **M — Middleware** | Three lanes: fast (sync audio), control (async tools), telemetry. Lifecycle hooks on session events. | Request/response hooks: before_generate, after_generate, on_tool, on_error. | Voice middleware handles **real-time concerns** (latency, interruption). Text middleware handles **quality concerns** (retry, cost tracking). |
| **A — Artifacts** | Not applicable mid-session. Artifacts are post-session. | Can produce artifacts (reports, summaries, structured data). | Text agents produce **artifacts** that voice session references via state. |

### Where Each Primitive is Strongest

```
                    VOICE MODE                          TEXT MODE
                    ──────────                          ─────────
S (State)      ★★★★★ Continuous extraction         ★★★ Transactional R/W
               Real-time emotional tracking         Clean input/output contracts
               Watchers fire on every change         Predictable state flow

C (Context)    ★★ Limited (server-managed)          ★★★★★ Full control
               Can only inject, never curate         Window, filter, summarize
               No conversation replay                Perfect context engineering

T (Tools)      ★★★ Single-shot, interruptible       ★★★★★ Multi-round loops
               Immutable declarations                Build/rebuild per request
               Phase-scoped filtering                Sequential chaining

P (Prompt)     ★★★★ Dynamic instruction updates     ★★★★ Full composition
               Modifiers compose per-turn            P module builds rich prompts
               Fast switching via phases             Detailed specialist instructions

M (Middleware) ★★★★ Real-time lifecycle hooks       ★★★ Request/response hooks
               Audio/VAD/interruption handling       Retry, cost, logging
               Three-lane architecture               Simple before/after chain

A (Artifacts)  ★ Not mid-session                    ★★★★ Full artifact support
               Post-session only                     Reports, structured output
                                                     Versioned, typed
```

### Cross-Leverage Patterns

#### Pattern: State as the Bridge (S ↔ S)

State flows bidirectionally between voice and text:

```
Voice extracts emotional_state every turn (S)
  ↓
TextAgentTool reads emotional_state (S)
  → adjusts reasoning tone
  → writes recommendation to state (S)
  ↓
Voice watcher fires on recommendation change
  → InstructionModifier appends to next instruction (P)
  → Model incorporates recommendation naturally
```

**Key insight**: Voice S is **continuous** (updated every turn by extractors).
Text S is **transactional** (read at start, write at end). The orchestrator
combines both — continuous extraction feeds precise transactional reasoning.

#### Pattern: Context Delegation (C in Text, not Voice)

Voice mode cannot curate its conversation history. Text mode can. Use text
agents for any task requiring careful context engineering:

```
Voice session has 50 turns of conversation
  → Cannot filter or summarize for the voice model
  → BUT can pass transcript window to a text agent

TextAgent receives TranscriptWindow via state (S → C bridge)
  → C::window(5) — only last 5 relevant turns
  → C::user_only() — filter to user statements
  → C::from_state("account_data") — inject structured data
  → Makes a focused generate() call with perfect context
  → Returns concise summary as tool result
```

**Key insight**: Voice C is impoverished (server-managed, no filtering). Text C
is rich (full conversation control). Delegate context-heavy work to text agents.

#### Pattern: Tool Chain Delegation (T in Text, not Voice)

Voice tool calls are single-shot. The model calls a tool, gets one response.
Complex workflows requiring multiple sequential tool calls are unreliable in
voice mode (model may not chain correctly, and interruptions break the chain).

Text agents handle multi-step tool chains naturally:

```
Voice model calls "verify_identity" tool (single call)
  ↓
TextAgentTool dispatches to verification pipeline:
  Step 1: db_lookup tool → get account record
  Step 2: compare tool → match name, DOB, SSN
  Step 3: risk_check tool → verify no fraud flags
  Step 4: format result → "Verified: Jane Smith, match confidence 98%"
  ↓
Single tool response returned to voice model
Voice model speaks: "Your identity has been verified, Jane."
```

**Key insight**: Voice T is single-shot, interruptible. Text T is multi-round
(up to 10 tool loops). Use text agents for any task requiring multiple tool calls.

#### Pattern: Prompt Specialization (P in Both, Different Roles)

Voice P steers the **conversation style** — empathetic, professional, compliant.
Text P steers the **reasoning quality** — analytical, structured, thorough.

```
Voice phase instruction (P):
  "You are a professional debt collector. Be empathetic.
   [Context: emotional_state=frustrated, risk_level=high]
   IMPORTANT: Show extra empathy due to elevated risk."

Text agent instruction (P):
  "You are a financial analyst. Given the debtor's account history
   and current financial situation, generate exactly 3 payment plan
   options. Each option must include: monthly_amount, duration_months,
   total_cost, interest_rate. Format as JSON array."
```

**Key insight**: Voice P is conversational and adaptive (modifiers change it
per-turn). Text P is precise and task-specific (one shot, get it right).

### Debt Collection: Full Primitive Interplay

Here's how all five primitives work together in a real debt collection call,
showing exactly where voice mode and text mode each contribute:

```
┌─────────────────────────────────────────────────────────────────────────┐
│ TURN 1: Disclosure Phase                                                │
│                                                                         │
│ P (Voice): DISCLOSURE_INSTRUCTION — reads the Mini-Miranda              │
│ S (Voice): Extractor detects disclosure_given=true                      │
│ S (Voice): Phase transition fires → verify_identity                     │
│ P (Voice): Instruction switches to VERIFY_IDENTITY_INSTRUCTION          │
│ C (Voice): enter_prompt injects model bridge message                    │
└─────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────┐
│ TURN 3: Verify Identity Phase                                           │
│                                                                         │
│ P (Voice): "Ask for name, DOB, SSN last 4. Use verify_identity tool."   │
│ T (Voice): Model calls verify_identity tool                             │
│                                                                         │
│   ┌─── TEXT AGENT DISPATCHED ──────────────────────────────────────┐    │
│   │ S (Text): Reads name, dob, ssn_last4 from parent state        │    │
│   │ T (Text): Step 1 — db_lookup(account_id) → account record     │    │
│   │ T (Text): Step 2 — compare(provided, record) → match score    │    │
│   │ C (Text): Full context: "Verify: Jane Smith, DOB 1985-03-15"  │    │
│   │ P (Text): "Cross-reference identity. Return verified/failed." │    │
│   │ S (Text): Writes identity_verified=true to shared state       │    │
│   │ Returns: "Verified: Jane Smith, 98% confidence"               │    │
│   └────────────────────────────────────────────────────────────────┘    │
│                                                                         │
│ T (Voice): Tool response → model speaks naturally                       │
│ S (Voice): Watcher fires on identity_verified → phase transition        │
│ S (Voice): Extractor captures emotional_state=cooperative               │
└─────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────┐
│ TURN 5: Negotiate Phase                                                 │
│                                                                         │
│ P (Voice): NEGOTIATE_INSTRUCTION + [Context: emotional_state=calm,      │
│            willingness_to_pay=0.7, risk_level=low]                      │
│ S (Voice): Modifiers inject state into instruction dynamically          │
│ T (Voice): Model calls calculate_payment_plans tool                     │
│                                                                         │
│   ┌─── TEXT AGENT PIPELINE DISPATCHED ─────────────────────────────┐   │
│   │                                                                │   │
│   │ Agent 1: FinancialAnalyst (LlmTextAgent)                       │   │
│   │   S: Reads balance=$4,250, income hints from conversation      │   │
│   │   P: "Analyze debtor's ability to pay given $4,250 balance"    │   │
│   │   C: Full context with account history                         │   │
│   │   Writes: financial_assessment to state                        │   │
│   │                  │                                             │   │
│   │                  ▼  (>> sequential)                             │   │
│   │ Agent 2: PlanCalculator (LlmTextAgent)                         │   │
│   │   S: Reads financial_assessment from previous agent            │   │
│   │   T: calculate_plans(balance, assessment) → 3 options          │   │
│   │   P: "Generate 3 payment plans: aggressive, moderate, gentle"  │   │
│   │   Writes: payment_options to state                             │   │
│   │                  │                                             │   │
│   │                  ▼  (>> sequential)                             │   │
│   │ Agent 3: Formatter (FnTextAgent — no LLM)                      │   │
│   │   S: Reads payment_options, formats for voice presentation     │   │
│   │   Returns: "Option 1: $425/mo for 10 months (save $200)..."   │   │
│   │                                                                │   │
│   └────────────────────────────────────────────────────────────────┘   │
│                                                                         │
│ T (Voice): Tool response → model presents options conversationally      │
│ M (Voice): Latency tracked — pipeline took 3.2s                        │
│ S (Voice): Extractor captures negotiation_intent=partial_pay            │
└─────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────┐
│ BACKGROUND: Compliance Agent (every turn)                               │
│                                                                         │
│ M (Voice): on_turn_complete fires after each model turn                 │
│                                                                         │
│   ┌─── BACKGROUND TEXT AGENT ──────────────────────────────────────┐   │
│   │ S (Text): Reads full transcript window from state              │   │
│   │ C (Text): C::window(10) — last 10 turns for analysis           │   │
│   │ P (Text): "Check for FDCPA violations: threats, harassment,    │   │
│   │           deception, false urgency, unauthorized disclosure"    │   │
│   │ T (Text): Multi-step analysis with checklist tool              │   │
│   │ S (Text): Writes compliance_status to shared state             │   │
│   │ A (Text): Produces compliance_report artifact for audit        │   │
│   └────────────────────────────────────────────────────────────────┘   │
│                                                                         │
│ S (Voice): Watcher fires on compliance_status change                    │
│ P (Voice): InstructionModifier appends compliance warning if needed     │
│ Model adjusts language in next response (never knows why — just follows │
│   updated instruction)                                                  │
└─────────────────────────────────────────────────────────────────────────┘
```

### Summary: When to Use Which Primitive Where

| Task | Voice Primitive | Text Primitive | Why |
|---|---|---|---|
| Track caller emotion | S (extractor) | — | Continuous, every turn |
| Verify identity | T (tool call) | S+T+C (multi-step pipeline) | Requires 3 tool calls |
| Calculate payment plans | T (tool call) | S+T+P+C (pipeline) | Requires analysis + calculation + formatting |
| Check compliance | M (on_turn_complete) | S+C+P+A (background agent) | Async, produces artifacts |
| Steer conversation tone | P (modifiers) | — | Dynamic instruction updates |
| Route by intent | S (phase transitions) | — | Deterministic guard evaluation |
| Detect distress | S (watcher + computed) | — | Real-time state monitoring |
| Generate call summary | — | S+C+P+A (post-call agent) | Needs full context curation |
| Risk scoring | S (computed var) | P+C (if complex) | Simple: computed. Complex: delegate |
| Cease-and-desist handling | S (watcher, blocking) | — | Must interrupt immediately |

**Rule of thumb**:
- **Voice mode** handles anything that must be **real-time** and **conversational**
- **Text mode** handles anything that requires **multi-step reasoning**, **context curation**, or **artifact production**
- **State** is always the bridge between them

---

## Phase Transitions and Tool Call Timing

Understanding when phase transitions fire relative to tool call responses is
critical for the orchestrator pattern. The timing is:

```
┌─ MODEL TURN ─────────────────────────────────────────────────────────────┐
│                                                                          │
│  1. User speaks: "My date of birth is March 15th, 1985"                  │
│  2. Model processes input                                                │
│  3. Model decides to call tool: verify_identity({name, dob, ssn4})       │
│                                                                          │
│  ──── TOOL CALL EVENT ────────────────────────────────────────────────   │
│  4. handle_tool_calls() fires                                            │
│  5. TextAgentTool::call() runs (1-5s of text agent work)                 │
│  6. Tool response sent back: {"result": "Verified: Jane Smith"}          │
│  7. TextAgentTool also writes state: identity_verified = true    ← KEY  │
│                                                                          │
│  ──── MODEL CONTINUES ───────────────────────────────────────────────   │
│  8. Model receives tool response                                         │
│  9. Model speaks: "Thank you, Jane. Your identity is confirmed."         │
│  10. Model signals TurnComplete                                          │
│                                                                          │
│  ──── TURN COMPLETE PIPELINE ────────────────────────────────────────   │
│  11. Transcript finalized                                                │
│  12. Extractors run (LLM + regex)                                        │
│  13. Computed vars updated                                               │
│  14. Phase machine evaluates transitions:                                │
│      → identity_verified == true? YES → transition to inform_debt        │
│  15. New phase instruction composed + sent                               │
│  16. enter_prompt triggers model to speak under new instruction          │
│  17. Watchers fire on state changes                                      │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
```

### Critical Timing Rules

**Rule 1: Tool calls set state, TurnComplete evaluates it.**

The `TextAgentTool` writes to shared state during its execution (step 7). But
phase transitions only evaluate on `TurnComplete` (step 14). This means:

- The tool can set `identity_verified = true` during its run
- The model speaks naturally about the result ("Your identity is confirmed")
- Only AFTER the model finishes speaking does the phase transition evaluate
- The phase switches to `inform_debt` and the model gets a new instruction

This is the **correct** behavior — the model gets to complete its response
about the verification result before being steered to the next topic.

**Rule 2: State mutations from text agents trigger watchers at TurnComplete.**

Watchers also evaluate at TurnComplete, not at the moment state changes. So:

- `TextAgentTool` sets `payment_options = [...]` during tool execution
- The watcher on `payment_options` fires at step 17
- This is fine — the model already incorporated the options in its response

**Rule 3: Background agents write state asynchronously — watchers fire next turn.**

`BackgroundAgentDispatcher` runs text agents on separate tokio tasks. When a
background agent writes to state, the change is visible immediately, but
watchers won't evaluate until the next `TurnComplete`:

```
Turn N: BackgroundAgent starts (compliance check)
Turn N: Model speaks normally
Turn N: TurnComplete → watchers fire (no compliance result yet)

[Background agent completes, writes compliance_warning to state]

Turn N+1: User speaks
Turn N+1: Model responds
Turn N+1: TurnComplete → watchers fire → compliance_warning detected!
         → InstructionModifier appends warning
         → Model adjusts tone in Turn N+2
```

### Phase Transition After Agent Tool: The Full Sequence

Here's the complete debt collection example showing phases + tool dispatch:

```
Phase: verify_identity
  Instruction: "Ask for name, DOB, SSN. Use verify_identity tool."
  Guard: S::is_true("disclosure_given")
  Transition: "inform_debt" when S::is_true("identity_verified")

  Turn 1 (in this phase):
    User: "Jane Smith, March 15 1985, last four 6789"
    Model: calls verify_identity(name="Jane Smith", dob="1985-03-15", ssn4="6789")

    ┌─── TextAgentTool runs ──────────────────────────────┐
    │ LlmTextAgent("verifier"):                            │
    │   → db_lookup(account_id) → gets record              │
    │   → compare(provided, record) → 98% match            │
    │   → state.set("identity_verified", true)    ← SETS   │
    │   → returns "Verified: Jane Smith, 98% match"        │
    └──────────────────────────────────────────────────────┘

    Tool response → Model speaks: "Your identity has been verified."
    TurnComplete fires:
      → Extractor confirms emotional_state = cooperative
      → Phase evaluates: identity_verified == true → TRANSITION
      → Phase switches to: inform_debt
      → New instruction: INFORM_DEBT_INSTRUCTION composed with modifiers
      → enter_prompt: "Identity verified. I'll now inform about the debt."
      → Model speaks under new instruction

Phase: inform_debt
  Model: "I see you have an account with Acme Medical..."
  (now operating under INFORM_DEBT_INSTRUCTION)
```

### State Promotion: Tool Agent → Phase Transition → Next Phase

The key architectural insight is that `TextAgentTool` writes to the **same
shared `State`** as the voice session. This means:

1. Text agent sets `identity_verified = true`
2. This is the same `State` object the `PhaseMachine` evaluates
3. At TurnComplete, the transition guard `S::is_true("identity_verified")` reads it
4. Transition fires naturally — no explicit "promote state from child" step

This is simpler than Python ADK's approach (which copies state between
isolated sessions). Our shared `State` via `Arc<DashMap>` makes state flow
transparent.

### Edge Case: Tool Call Sets State That Blocks Current Phase

What if the text agent sets state that triggers a transition to a phase where
the current tool isn't allowed?

```
Phase: negotiate (tools: [calculate_payment_plans, lookup_account])
  Model calls calculate_payment_plans tool
  TextAgentTool runs:
    → Sets negotiation_intent = "full_pay"
    → Returns payment options
  TurnComplete:
    → Transition evaluates: negotiation_intent in ["full_pay", "partial_pay"] → YES
    → Phase switches to arrange_payment
    → arrange_payment has different tools: [process_payment]
```

This is fine — the tool call completed before the phase transition. The model
gets the tool response, speaks about it, and then gets a new instruction for
the next phase. No conflict.

### Edge Case: Background Agent Triggers Phase Transition

What if a background compliance agent detects a cease-and-desist and sets
`cease_desist_requested = true`?

```
Turn N: Model speaking normally in negotiate phase
  [Background] ComplianceAgent detects cease-and-desist language
  [Background] state.set("cease_desist_requested", true)

Turn N+1: TurnComplete fires
  → Phase evaluates: cease_desist_requested == true → TRANSITION to close
  → Blocking watcher fires (cease_desist_requested)
  → enter_prompt: "Cease-and-desist requested. Closing call respectfully."
  → Model switches to CLOSE_INSTRUCTION
```

The background agent's state mutation is picked up at the next TurnComplete.
The phase machine doesn't check mid-turn — this is by design. The model gets
to finish its current utterance, and the next turn picks up the phase change.

---

## Design: Two Dispatch Patterns

### Pattern 1: Synchronous Tool Dispatch

The live model decides it needs external reasoning and makes a tool call.
The call blocks. A `TextAgent` pipeline runs. The result returns as a tool
response. The model incorporates it naturally.

```
User speaks → Model decides to call "lookup_account" tool
  → TextAgentTool::call() invoked
    → LlmTextAgent runs (possibly multiple LLM round-trips + tool calls)
    → Returns: "Jane Smith owes $4,250 to Acme Medical, 127 days past due"
  → Tool response sent to model
  → Model speaks: "I can see your account with Acme Medical..."
```

**Use when**: The model needs information before it can continue speaking.
Account lookups, calculations, verification checks, any task where the voice
model should wait for the result.

**Latency**: 1-5 seconds typical (one or more `generate()` calls). The voice
session is silent during this time. Use `BackgroundToolTracker` for progress.

### Pattern 2: Asynchronous Background Dispatch

A callback (on_turn_complete, watcher, phase on_enter) fires a background
agent pipeline. Results land in state. Watchers or instruction modifiers
surface the results to the model on the next turn.

```
Model completes turn → on_turn_complete fires
  → BackgroundAgentDispatcher spawns compliance_checker agent
  → Model continues normal conversation
  → [background] ComplianceAgent analyzes transcript
  → [background] Sets state: "compliance_warning" = "FDCPA risk detected"
  → Watcher fires on "compliance_warning" change
  → InstructionModifier appends warning to next instruction
  → Model's next response incorporates compliance guidance
```

**Use when**: The work can happen in the background while the model continues
talking. Risk scoring, compliance checks, enrichment lookups, analytics.

**Latency**: Transparent. Model never waits. Results appear in state within
1-5 seconds and influence subsequent turns.

---

## Detailed Design: `TextAgentTool`

### Type Definition

```rust
// crates/gemini-adk-rs/src/text_agent_tool.rs

pub struct TextAgentTool {
    name: String,
    description: String,
    agent: Arc<dyn TextAgent>,
    parameters: Option<serde_json::Value>,
    /// Which parent state keys to copy into the agent's state.
    /// None = copy all keys.
    input_keys: Option<Vec<String>>,
    /// Which agent state keys to promote back to parent state.
    /// None = don't promote anything (only return tool result).
    output_keys: Option<Vec<String>>,
    /// Reference to parent state (set by ToolDispatcher at dispatch time).
    parent_state: Option<State>,
}
```

### ToolFunction Implementation

```rust
#[async_trait]
impl ToolFunction for TextAgentTool {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn parameters(&self) -> Option<serde_json::Value> { self.parameters.clone() }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        // 1. Create child state
        let child_state = State::new();

        // 2. Project parent state into child (if parent_state available)
        if let Some(parent) = &self.parent_state {
            match &self.input_keys {
                None => {
                    // Copy all parent state
                    child_state.merge_from(parent);
                }
                Some(keys) => {
                    for key in keys {
                        if let Some(val) = parent.get::<serde_json::Value>(key) {
                            child_state.set(key, val);
                        }
                    }
                }
            }
        }

        // 3. Inject tool call args as "input" key (TextAgent convention)
        if let Some(input) = args.get("request").and_then(|r| r.as_str()) {
            child_state.set("input", input);
        }
        child_state.set("args", &args);

        // 4. Run the text agent pipeline
        let result = self.agent.run(&child_state).await
            .map_err(|e| ToolError::ExecutionFailed(format!("{e}")))?;

        // 5. Promote selected keys back to parent
        if let (Some(parent), Some(keys)) = (&self.parent_state, &self.output_keys) {
            for key in keys {
                if let Some(val) = child_state.get::<serde_json::Value>(key) {
                    parent.set(key, val);
                }
            }
        }

        // 6. Return result as tool response
        Ok(json!({"result": result}))
    }
}
```

### Builder API

```rust
impl TextAgentTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        agent: impl TextAgent + 'static,
    ) -> Self { ... }

    /// Build from a Composable expression (compiles to TextAgent).
    pub fn from_composable(
        name: impl Into<String>,
        description: impl Into<String>,
        composable: Composable,
    ) -> Self {
        let agent = composable.compile();
        Self::new(name, description, agent)
    }

    /// Select which parent state keys the agent can see.
    pub fn input_keys(mut self, keys: &[&str]) -> Self { ... }

    /// Select which agent state keys promote back to parent.
    pub fn output_keys(mut self, keys: &[&str]) -> Self { ... }

    /// Override tool parameters schema.
    pub fn parameters(mut self, params: serde_json::Value) -> Self { ... }
}
```

### State Injection via ToolDispatcher

Extend `ToolDispatcher` to pass state through:

```rust
// In crates/gemini-adk-rs/src/tool.rs
impl ToolDispatcher {
    /// Call a tool function with optional parent state for state-aware tools.
    pub async fn call_function_with_state(
        &self,
        name: &str,
        args: Value,
        state: Option<&State>,
    ) -> Result<Value, ToolError> {
        let tool = self.get(name)?;
        // If tool is TextAgentTool and state provided, inject it
        if let Some(state) = state {
            if let Some(tat) = tool.as_any().downcast_ref::<TextAgentTool>() {
                tat.set_parent_state(state.clone());
            }
        }
        tool.call(args).await
    }
}
```

**Alternative** (simpler, avoids downcasting): `TextAgentTool` captures `State`
at construction time via `Arc`. The `Live` builder passes the session's `State`
when constructing the tool.

---

## Detailed Design: `BackgroundAgentDispatcher`

### Type Definition

```rust
// crates/gemini-adk-rs/src/live/background_agent.rs

pub struct BackgroundAgentDispatcher {
    /// Maximum concurrent background agents.
    budget: Arc<tokio::sync::Semaphore>,
    /// Active task handles for cancellation.
    tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}
```

### API

```rust
impl BackgroundAgentDispatcher {
    pub fn new(max_concurrent: usize) -> Self { ... }

    /// Dispatch a TextAgent to run in the background.
    /// Results are written to state under `{task_name}:result`.
    /// Errors are written to `{task_name}:error`.
    pub fn dispatch(
        &self,
        task_name: impl Into<String>,
        agent: Arc<dyn TextAgent>,
        state: State,
    ) {
        let name = task_name.into();
        let budget = self.budget.clone();
        let tasks = self.tasks.clone();

        let handle = tokio::spawn(async move {
            let _permit = budget.acquire().await.unwrap();
            match agent.run(&state).await {
                Ok(result) => {
                    state.set(&format!("{name}:result"), &result);
                }
                Err(e) => {
                    state.set(&format!("{name}:error"), &format!("{e}"));
                }
            }
        });

        self.tasks.lock().unwrap().insert(name, handle);
    }

    /// Cancel all running background agents.
    pub fn cancel_all(&self) { ... }
}
```

### Integration with Live Builder

```rust
Live::builder()
    .background_budget(5)  // max 5 concurrent background agents
    .on_turn_complete(|state, _writer| async move {
        let dispatcher = BackgroundAgentDispatcher::global();
        dispatcher.dispatch("compliance", compliance_agent, state);
    })
    .watch("compliance:result")
        .changed()
        .then(|_old, result, state| async move {
            // React to background agent completing
        })
```

---

## Detailed Design: Fluent `.agent_tool()` on Live Builder

### API on Live

```rust
// crates/gemini-adk-fluent-rs/src/live.rs

impl Live {
    /// Register a TextAgent as a tool the live model can call.
    pub fn agent_tool(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        agent: impl TextAgent + 'static,
    ) -> Self { ... }

    /// Register a Composable pipeline as a tool.
    pub fn agent_tool_pipeline(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        pipeline: Composable,
    ) -> Self {
        let agent = pipeline.compile();
        self.agent_tool(name, description, agent)
    }
}
```

### Usage Example: Debt Collection with Agent Dispatch

```rust
// Define specialist text agents
let flash = Arc::new(GeminiLlm::new(GeminiModel::Gemini2_5Flash));

let verify_agent = LlmTextAgent::new("verifier", flash.clone())
    .instruction("Cross-reference identity info against account record. \
                  Return 'verified' or 'failed' with reason.")
    .tools(Arc::new(verify_dispatcher));

let payment_agent = LlmTextAgent::new("calculator", flash.clone())
    .instruction("Generate 3 payment plan options. Consider debtor's \
                  financial situation and balance. Return formatted options.")
    .tools(Arc::new(calc_dispatcher));

// Register as tools in the voice session
let session = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .voice(Voice::Kore)
    .agent_tool("verify_identity",
        "Verify caller identity against account records",
        verify_agent,
    )
    .agent_tool("calculate_payment_plans",
        "Generate payment plan options for the debtor",
        payment_agent,
    )
    .phase_defaults(|d| d
        .with_state(DEBT_STATE_KEYS)
        .when(risk_is_elevated, RISK_WARNING)
        .prompt_on_enter(true)
    )
    .phase("disclosure")
        .instruction(DISCLOSURE_INSTRUCTION)
        .transition("verify_identity", S::is_true("disclosure_given"))
        .done()
    .phase("verify_identity")
        .instruction("Ask for name, DOB, and last 4 of SSN. Use the \
                     verify_identity tool to check. Be patient.")
        .guard(S::is_true("disclosure_given"))
        .transition("inform_debt", S::is_true("identity_verified"))
        .enter_prompt("I'll now verify your identity.")
        .done()
    // ... remaining phases
    .connect_vertex(project, location, token)
    .await?;
```

**What happens at runtime**:
1. Voice model asks "What is your date of birth?"
2. User responds "March 15th, 1985"
3. Model calls `verify_identity` tool with `{name, dob, ssn_last4}`
4. `TextAgentTool` dispatches to `LlmTextAgent("verifier")`
5. Verifier calls `db_lookup` tool, gets account record, reasons about match
6. Returns: `"Identity verified: Jane Smith, DOB matches, SSN last 4 match"`
7. Tool response returned to voice model
8. Model says: "Thank you, Jane. Your identity has been verified."

---

## Implementation Plan

### Task 1: `TextAgentTool` (P0, ~60 LOC)

**Files:**
- Create: `crates/gemini-adk-rs/src/text_agent_tool.rs`
- Modify: `crates/gemini-adk-rs/src/lib.rs` (add `pub mod text_agent_tool;`)

**Step 1: Write the struct and constructor**

```rust
pub struct TextAgentTool {
    name: String,
    description: String,
    agent: Arc<dyn TextAgent>,
    parameters: Option<serde_json::Value>,
    state: State,  // Captured at construction, shared with parent
}

impl TextAgentTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        agent: impl TextAgent + 'static,
        state: State,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agent: Arc::new(agent),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "The request to process"
                    }
                },
                "required": ["request"]
            })),
            state,
        }
    }

    pub fn with_parameters(mut self, params: serde_json::Value) -> Self {
        self.parameters = Some(params);
        self
    }
}
```

**Step 2: Implement `ToolFunction`**

```rust
#[async_trait]
impl ToolFunction for TextAgentTool {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn parameters(&self) -> Option<serde_json::Value> { self.parameters.clone() }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        // Inject args into state
        if let Some(request) = args.get("request").and_then(|r| r.as_str()) {
            self.state.set("input", request);
        }
        self.state.set("agent_tool_args", &args);

        // Run the text agent
        let result = self.agent.run(&self.state).await
            .map_err(|e| ToolError::ExecutionFailed(format!("{e}")))?;

        Ok(json!({"result": result}))
    }
}
```

**Step 3: Write tests**

- Test basic dispatch (echo agent → verify tool response)
- Test state sharing (parent state visible to child)
- Test error propagation (agent failure → ToolError)
- Test custom parameters schema

**Step 4: Run tests**

```bash
cargo test -p gemini-adk-rs text_agent_tool
```

**Step 5: Commit**

```bash
git add crates/gemini-adk-rs/src/text_agent_tool.rs crates/gemini-adk-rs/src/lib.rs
git commit -m "feat(gemini-adk-rs): add TextAgentTool bridging text agent pipelines to tool dispatch"
```

---

### Task 2: Fluent `.agent_tool()` on Live builder (P0, ~40 LOC)

**Files:**
- Modify: `crates/gemini-adk-fluent-rs/src/live.rs`

**Step 1: Add `agent_tool` method**

```rust
/// Register a TextAgent as a tool the live model can call.
///
/// The agent runs in the session's shared State context, so it can
/// read extracted values and its mutations are visible to watchers.
pub fn agent_tool(
    mut self,
    name: impl Into<String>,
    description: impl Into<String>,
    agent: impl TextAgent + 'static,
) -> Self {
    // Store for deferred construction (State not available until connect)
    self.deferred_agent_tools.push(DeferredAgentTool {
        name: name.into(),
        description: description.into(),
        agent: Arc::new(agent),
    });
    self
}
```

Note: `TextAgentTool` needs a `State` reference, but `State` is created at
`connect()` time. Options:
1. **Deferred construction**: Store agent specs, build `TextAgentTool` at connect time
2. **Pre-shared State**: Create `State` in `builder()`, share with `TextAgentTool`

Recommendation: Option 2 is simpler. `Live::builder()` creates a `State` that's
passed to both `TextAgentTool` and `LiveSessionBuilder`.

**Step 2: Add `agent_tool_pipeline` convenience**

```rust
/// Register a Composable pipeline as a tool the live model can call.
pub fn agent_tool_pipeline(
    self,
    name: impl Into<String>,
    description: impl Into<String>,
    pipeline: Composable,
) -> Self {
    let agent = pipeline.compile();
    self.agent_tool(name, description, agent)
}
```

**Step 3: Write integration test**

Test that `.agent_tool()` registers the tool in the dispatcher and it appears
in tool declarations sent to the model.

**Step 4: Commit**

```bash
git commit -m "feat(gemini-adk-fluent-rs): add .agent_tool() for registering text agent pipelines as live tools"
```

---

### Task 3: `BackgroundAgentDispatcher` (P1, ~120 LOC)

**Files:**
- Create: `crates/gemini-adk-rs/src/live/background_agent.rs`
- Modify: `crates/gemini-adk-rs/src/live/mod.rs` (add module + re-export)

**Step 1: Core dispatcher struct**

```rust
pub struct BackgroundAgentDispatcher {
    budget: Arc<tokio::sync::Semaphore>,
    tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

impl BackgroundAgentDispatcher {
    pub fn new(max_concurrent: usize) -> Self { ... }
    pub fn dispatch(&self, name: impl Into<String>, agent: Arc<dyn TextAgent>, state: State) { ... }
    pub fn cancel_all(&self) { ... }
    pub fn is_running(&self, name: &str) -> bool { ... }
}
```

**Step 2: Integrate with `LiveHandle`**

Add `background_agents: BackgroundAgentDispatcher` to `LiveHandle` so callbacks
can dispatch via `handle.dispatch_agent(...)`.

**Step 3: Write tests**

- Test concurrent dispatch respects budget
- Test results land in state
- Test cancellation
- Test error handling

**Step 4: Commit**

```bash
git commit -m "feat(gemini-adk-rs): add BackgroundAgentDispatcher for async agent work from live callbacks"
```

---

### Task 4: `PromptComposite` → `String` bridge (P2, ~10 LOC)

**Files:**
- Modify: `crates/gemini-adk-fluent-rs/src/compose/prompt.rs`

**Step 1: Implement conversions**

```rust
impl From<PromptComposite> for String {
    fn from(p: PromptComposite) -> String {
        p.render()
    }
}

impl From<PromptSection> for String {
    fn from(s: PromptSection) -> String {
        s.render()
    }
}
```

Now `LlmTextAgent::new("x", llm).instruction(P::role("analyst") + P::task("analyze"))` works.

**Step 2: Test**

```rust
#[test]
fn prompt_composite_into_string() {
    let s: String = (P::role("analyst") + P::task("analyze")).into();
    assert!(s.contains("You are analyst."));
    assert!(s.contains("Your task: analyze"));
}
```

**Step 3: Commit**

```bash
git commit -m "feat(gemini-adk-fluent-rs): implement Into<String> for PromptComposite and PromptSection"
```

---

### Task 5: Integration test — debt collection with agent dispatch (P1)

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`

**Step 1: Add a verification agent tool**

Replace the mock `verify_identity` pure-function tool with a `TextAgentTool`
wrapping a `LlmTextAgent` that actually reasons about identity matching.

**Step 2: Add a payment calculation agent tool**

Replace the mock `calculate_payment_plan` pure-function tool with a pipeline:
`analyze_finances >> calculate_options >> format_presentation`.

**Step 3: Add background compliance agent**

Wire up an `on_turn_complete` callback that dispatches a compliance checker
agent every N turns, with results surfaced via `with_state`.

**Step 4: Verify end-to-end**

Run the Web UI and verify:
- Voice model calls agent tools
- Multi-step reasoning happens in background
- Results flow back naturally
- State mutations trigger watchers

---

## Open Questions

### Q1: State isolation vs. sharing

Should `TextAgentTool` share the parent's `State` directly (zero-copy, mutations
visible immediately) or receive a snapshot (isolated, explicit promotion)?

**Recommendation**: Share directly. The `State` type is already `Arc<DashMap>` —
concurrent access is safe. Sharing enables:
- Agent reads live-extracted state (emotional_state, risk_level)
- Agent mutations trigger watchers immediately
- No explicit promotion step needed

Risk: Agent pollutes parent state. Mitigate with prefix convention: agent writes
to `agent:{name}:` prefixed keys.

### Q2: Tool call timeout for agent tools

Text agent pipelines can take 5-30 seconds (multiple LLM calls). The existing
`ToolDispatcher` timeout (default 30s) may not be enough for complex pipelines.

**Recommendation**: Allow per-tool timeout override:
```rust
.agent_tool("complex_analysis", "...", analysis_pipeline)
    .timeout(Duration::from_secs(60))
```

### Q3: Should compiled `Composable` produce `Arc<dyn TextAgent>`?

Currently `Composable::compile()` returns `Box<dyn TextAgent>`. For sharing
across multiple tool registrations, `Arc<dyn TextAgent>` would be better.

**Recommendation**: Return `Arc<dyn TextAgent>`. It's `Clone`-able and shareable.

---

## Performance Considerations

| Concern | Mitigation |
|---|---|
| Tool call latency (1-5s) | Voice model waits silently. Use `BackgroundToolTracker` for long-running tools. |
| State contention | `DashMap` sharding handles concurrent reads/writes without lock contention. |
| Memory (multiple LLM contexts) | `TextAgent` contexts are small (single conversation). Freed after call. |
| Task explosion | `BackgroundAgentDispatcher` uses semaphore budget (default 5). |
| Session disconnect during agent work | Cancellation via `JoinHandle::abort()` on disconnect. |

---

## Relationship to Python ADK

| Python ADK Feature | Our Equivalent | Status |
|---|---|---|
| `AgentTool(agent=llm_agent)` | `TextAgentTool::new(name, desc, text_agent, state)` | **Task 1** |
| `dispatch(*agents)` | `BackgroundAgentDispatcher::dispatch()` | **Task 3** |
| `join()` | Watcher on `{task}:result` key | **Already exists** (watchers) |
| `Route("key").eq(...)` | `RouteTextAgent` (exists) + `S::eq()` (exists) | **Done** |
| `Fallback(a, b, c)` | `FallbackTextAgent` (exists) | **Done** |
| `Pipeline(a >> b >> c)` | `SequentialTextAgent` (exists) | **Done** |
| `FanOut(a \| b \| c)` | `ParallelTextAgent` (exists) | **Done** |
| `Loop(a * n)` | `LoopTextAgent` (exists) | **Done** |
| `Race(a, b)` | `RaceTextAgent` (exists) | **Done** |
| `MapOver("items", agent)` | `MapOverTextAgent` (exists) | **Done** |
| `E.suite(agent)` | Not implemented | **P3** |
| `StreamRunner` | Not implemented | **P3** |

The critical gap is **Task 1** — ~60 lines of code that unlock the entire
orchestrator pattern. Everything else is DX polish and advanced features.

---

## Summary

```
P0 (unblocks everything):
  Task 1: TextAgentTool           ~60 LOC   bridges text agents into live tool dispatch
  Task 2: .agent_tool() fluent    ~40 LOC   ergonomic registration on Live builder

P1 (enables async patterns):
  Task 3: BackgroundDispatcher    ~120 LOC  fire-and-forget agent work from callbacks
  Task 5: Integration test        ~100 LOC  prove it works end-to-end

P2 (polish):
  Task 4: P → String bridge       ~10 LOC   reuse prompt composition for text agents

P3 (future):
  Evaluation framework           ~500 LOC   test agent pipeline quality
  StreamRunner                   ~200 LOC   batch ingestion through pipelines
```

Total P0+P1: **~320 lines of new code** to turn Gemini Live from a monolithic
voice agent into a voice orchestrator dispatching to arbitrary agent pipelines.
