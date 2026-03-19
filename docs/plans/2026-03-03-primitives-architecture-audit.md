# The Five Primitives — S.C.T.P.M Architecture Audit & Redesign

**Date**: 2026-03-03
**Status**: Design RFC (Revised)
**Scope**: Cross-cutting audit of State, Context, Tools, Prompt, Middleware across L0/L1/L2
**Complements**: `voice-native-state-control-design.md` (state taxonomy & control flow),
`fluent-devex-redesign.md` (composition surface), `callback-mode-design.md` (execution semantics)
**Reference**: Python `adk-fluent` repo — expression IR and three-channel model

---

## Executive Summary

Every voice application — every LLM application — is built from five primitives:

```
┌──────────────────────────────────────────────────────────────────┐
│                    THE FIVE PRIMITIVES                            │
│                                                                  │
│   S — State        The memory of the conversation                │
│   C — Context      What the model sees right now                 │
│   T — Tools        What the model can do                         │
│   P — Prompt       What the model should be                      │
│   M — Middleware    What happens before/after every action        │
│                                                                  │
│   Every feature is a composition of these five.                  │
│   Nothing exists outside them.                                   │
└──────────────────────────────────────────────────────────────────┘
```

### The Three-Channel Model

Communication between developer code and the LLM flows through exactly three
channels. Every primitive operates on one or more of these channels:

```
┌─────────────────────────────────────────────────────────────────────┐
│  Channel 1: Conversation History    (Content[], turns, transcripts) │
│  Channel 2: Session State           (State / DashMap)               │
│  Channel 3: Instruction Templating  ({key} substitution → model)    │
└─────────────────────────────────────────────────────────────────────┘
         ↓                    ↓                    ↓
      C Module           S Module            P Module
   (what model sees)  (what persists)    (what model told)
```

| Channel | Written by | Read by | Primitive |
|---------|-----------|---------|-----------|
| 1 — Conversation | Transcripts, context injection, tool summaries | Context filters, extractors | **C** |
| 2 — State | Extractors, tools, watchers, developer code | Guards, computed, templates, watchers | **S** |
| 3 — Instruction | Phase machine, instruction_template, amendments | Model (system prompt) | **P** |

**T** (Tools) bridges Channels 1 and 2: tool calls appear in conversation history
(Channel 1), and tool results can be promoted to state (Channel 2) via interceptors.

**M** (Middleware) observes and controls all three channels: lifecycle hooks fire
at every layer and can inspect/modify request/response data.

### Voice-Native vs Text-Based: The Fundamental Constraint

Gemini Live operates over a **stateful WebSocket session** with critical constraints
that differ from standard LLM request-response:

| Constraint | Gemini Live | Standard LLM |
|------------|-------------|---------------|
| Tool declarations | **Immutable after setup** — cannot add/remove tools mid-session | Sent with each request |
| System instruction | **Updatable mid-session** via WebSocket command | Sent with each request |
| Conversation history | **Server-managed** — no full history resend | Client-managed, sent each time |
| Context window | **Server-side compression** with configurable thresholds | Client truncates before sending |
| Concurrency | **Two-lane** — fast lane (audio/text) + control lane (tools/lifecycle) | Sequential request-response |
| Interruption | **Barge-in** — user can interrupt mid-response | Not applicable |
| Session lifetime | **Continuous** — 15min audio, 2min video | Per-request |

These constraints dictate architecture:
- **Phase-scoped tool filtering** must be implemented as runtime rejection, not declaration changes
- **Instruction updates** are the primary lever for steering behavior mid-conversation
- **State** is the central coordination point — no request-level parameter overrides
- **Context injection** must use `send_client_content()`, not message history rebuilds

These primitives exist at every layer of the stack:

| Primitive | L0 (gemini-live) | L1 (gemini-adk) | L2 (gemini-adk-fluent) |
|-----------|---------------|-------------|---------------------|
| **S** | `SessionConfig`, `SessionPhase`, `Turn` | `State`, `PrefixedState`, `ComputedVar`, `WatcherRegistry` | `.watch()`, `.computed()`, `S::` module |
| **C** | `Content`, `Part`, `Role`, context compression | `TranscriptBuffer`, `InvocationContext` | `.extract_turns()`, `C::` module |
| **T** | `Tool`, `FunctionDeclaration`, `FunctionCall/Response`, `ToolProvider` | `ToolDispatcher`, `ToolFunction`, `BackgroundToolTracker` | `.tools()`, `.on_tool_call()`, `T::` module |
| **P** | `system_instruction`, `GenerationConfig` | `instruction.rs`, `PhaseInstruction`, `instruction_template` | `.instruction()`, `.phase()`, `P::` module |
| **M** | Traits: `Transport`, `Codec`, `AuthProvider` | `Middleware` trait, `MiddlewareChain`, `RetryMiddleware` | `.before_tool_response()`, `M::` module |

This document audits each primitive across all three layers, identifies gaps and
friction points, and proposes targeted improvements that preserve performance
while dramatically improving developer ergonomics.

**Design principle**: The five primitives are **the entire API surface**. If a
developer needs to step outside S.C.T.P.M to accomplish something, that is a bug
in the framework, not in the developer's code.

---

## 1. State (S) — The Memory of the Conversation

### 1.1 Current Architecture

**L0 (gemini-live)**: State is implicit. `SessionConfig` is consumed at connection
time and becomes immutable. Runtime state lives in `SessionState` (turn history,
phase FSM) behind `Arc<Mutex<>>`. The session phase (`SessionPhase`) is a
validated FSM with transitions like `Active → ToolCallPending → ToolCallExecuting`.

**L1 (gemini-adk)**: `State` is a `DashMap<String, Value>` with typed get/set and
prefix-based namespacing:

```rust
impl State {
    fn session(&self) -> PrefixedState<'_>          // "session:" — runtime signals
    fn app(&self) -> PrefixedState<'_>              // "app:" — application state
    fn user(&self) -> PrefixedState<'_>             // "user:" — user preferences
    fn temp(&self) -> PrefixedState<'_>             // "temp:" — scratch space
    fn turn(&self) -> PrefixedState<'_>             // "turn:" — reset each turn
    fn bg(&self) -> PrefixedState<'_>               // "bg:" — background tasks
    fn derived(&self) -> ReadOnlyPrefixedState<'_>  // "derived:" — computed (read-only)
}
```

Additional L1 state subsystems:
- `ComputedRegistry`: Topologically sorted derived variables, stored at `derived:{key}`
- `WatcherRegistry`: Reactive triggers on state diffs (snapshot → mutate → diff)
- `SessionSignals`: Auto-populates `session:*` keys from events
- `PhaseMachine`: Stores current phase at `session:phase`, tracks history

**L2 (gemini-adk-fluent)**: Fluent builders expose state through closures:

```rust
.computed("total", &["items"], |state| { ... })
.watch("score").crossed_above(0.9).then(|old, new, state| async { ... })
.instruction_template(|state| Some(format!("Phase: {}", state.get("session:phase")?)))
.phase("ordering").guard(|state| state.get::<bool>("verified").unwrap_or(false))
```

### 1.2 What Works

**Prefix namespacing is excellent.** The `session:`, `derived:`, `turn:`, `bg:`,
`app:` prefixes give developers clear mental models about who owns what, what
persists, and what auto-resets. The `ReadOnlyPrefixedState` for `derived:` is
particularly good — it makes it impossible to accidentally overwrite computed values.

**DashMap for concurrent access** is the right choice. Voice applications have
events on multiple lanes (fast audio lane, control lane) and DashMap provides
lock-free reads with shard-level write locking.

**Delta tracking** (`with_delta_tracking()`) enables transactional multi-key
updates. This is important for computed variables that need atomic state reads.

**Snapshot/diff for watchers** is efficient — only observed keys are captured,
and the diff is computed by the registry, not by individual watchers.

### 1.3 What's Broken

**1.3.1 Extractor results were opaque blobs (FIXED)**

When `extract_turns::<DebtorState>()` ran, the processor stored the entire struct
as a single JSON blob:

```rust
// BEFORE: One opaque blob, fields inaccessible to watchers/computed/guards
state.set("DebtorState", json!({"emotional_state": "calm", "willingness_to_pay": 0.5}));
```

This meant `watch("willingness_to_pay").crossed_above(0.7)` could never fire —
the key `"willingness_to_pay"` didn't exist in state. Developers had to navigate
the blob manually in every guard, computed var, and instruction template.

**Fix applied**: The L1 processor now auto-flattens JSON object fields to
individual state keys. The blob is preserved for struct-level access:

```rust
// AFTER: Blob preserved + fields individually addressable
state.set("DebtorState", blob);                    // struct access still works
state.set("emotional_state", "calm");              // watch/computed/guard just works
state.set("willingness_to_pay", 0.5);              // crossed_above(0.7) fires correctly
```

**1.3.2 Tool responses don't write state**

Tool results (e.g., `verify_identity` returning `{"verified": true}`) never
write to state. The `on_tool_call` callback doesn't receive `State`. This forces
developers to use `before_tool_response` (which does receive State) as a
workaround for state promotion:

```rust
// WORKAROUND: promoting tool results to state via interceptor
.before_tool_response(|responses, state| async move {
    for r in &responses {
        if r.name == "verify_identity" && r.response["verified"] == true {
            state.set("identity_verified", true);
        }
    }
    responses
})
```

**Proposed fix**: `on_tool_call` should receive `State`:

```rust
// PROPOSED: on_tool_call receives State
.on_tool_call(|calls, state| async move {
    let responses = dispatch_tools(&calls);
    // Promote results naturally
    if let Some(r) = responses.iter().find(|r| r.name == "verify_identity") {
        if r.response["verified"] == true {
            state.set("identity_verified", true);
        }
    }
    Some(responses)
})
```

**1.3.3 No atomic read-modify-write**

Common patterns like incrementing a counter require get + set, which isn't atomic:

```rust
// NOT ATOMIC: another task could read between get and set
let count: u32 = state.get("error_count").unwrap_or(0);
state.set("error_count", count + 1);
```

**Proposed fix**: Add `modify()` to State:

```rust
impl State {
    /// Atomically read-modify-write a value.
    fn modify<T, F>(&self, key: &str, default: T, f: F) -> T
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(T) -> T,
    {
        // Uses DashMap::entry() for atomicity
    }
}

// Usage:
state.modify("error_count", 0u32, |n| n + 1);
```

**1.3.4 No state change provenance**

When debugging, there's no way to know *who* set a value. Was it an extractor?
A tool callback? A watcher? A computed var?

**Proposed enhancement** (debug-only): State tracks the last writer per key:

```rust
#[cfg(debug_assertions)]
struct StateEntry {
    value: Value,
    written_by: &'static str,  // "extractor:DebtorState", "computed:sentiment", "tool:verify"
    written_at: Instant,
}
```

### 1.4 State Lifecycle — Who Writes What, When

The evaluation order on TurnComplete establishes a clear ownership chain:

```
TurnComplete
  │
  ├─ 1. TranscriptBuffer.end_turn()      ─── turn-scoped state cleared
  ├─ 2. SessionSignals.process()          ─── session:* keys updated
  ├─ 3. Extractors run                    ─── extractor blobs + flattened fields written
  ├─ 4. ComputedRegistry.recompute()      ─── derived:* keys updated
  ├─ 5. PhaseMachine.evaluate()           ─── session:phase updated, on_enter/on_exit fire
  ├─ 6. WatcherRegistry.evaluate()        ─── watchers fire (may write arbitrary keys)
  ├─ 7. TemporalRegistry.check()          ─── temporal patterns fire
  ├─ 8. instruction_template()            ─── reads state, produces instruction
  ├─ 9. on_turn_boundary()                ─── may inject content
  └─ 10. on_turn_complete()               ─── user callback
```

Each stage has clear read/write boundaries:

| Stage | Reads | Writes | Owns |
|-------|-------|--------|------|
| SessionSignals | Events | `session:*` | turn_count, interrupt_count, VAD state |
| Extractors | Transcript | Extractor name + flattened fields | Domain state |
| Computed | Any key | `derived:*` | Derived scores, aggregates |
| PhaseMachine | Any key | `session:phase` | Current conversation phase |
| Watchers | Diffs + State | Arbitrary | Side-effects, alerts |
| Temporal | State + Events | Arbitrary | Time-based patterns |
| instruction_template | Any key | None (read-only) | Model instructions |

**Key insight**: The lifecycle is a DAG, not a flat sequence. Computed vars
depend on extractor output. Phase guards depend on both. Watchers see the
final state after all upstream mutations. This ordering is correct and should
be preserved.

### 1.5 State ↔ Channel Mapping

State operates exclusively on **Channel 2** (session state). It does not directly
read or write conversation history (Channel 1) or model instructions (Channel 3).
However, it **bridges** to both:

```
Channel 1 (History) ──transcripts──→ Extractors ──auto-flatten──→ Channel 2 (State)
Channel 2 (State) ──instruction_template──→ Channel 3 (Instruction)
Channel 2 (State) ──{key} substitution──→ Channel 3 (Instruction)
```

This separation is deliberate. State transforms (`S::pick()`, `S::rename()`,
`S::flatten()`) are zero-cost operations on Channel 2 that never invoke an LLM.
When state values need to reach the model, they flow through Channel 3 via
explicit bridges (instruction templates, phase instructions with `{key}` interpolation).

**S.capture()** (from Python adk-fluent) bridges Channel 1 → Channel 2 by
snapshotting the latest user message into state. Our Rust equivalent is the
`TranscriptBuffer` → extractor pipeline, which is more powerful (structured
extraction with typed schemas) but less immediate for simple cases.

**Proposed addition**: `S::capture(key)` for simple text capture:
```rust
// Capture latest user utterance into state without an LLM call
S::capture("last_user_message")
// Equivalent to: state.set("last_user_message", transcript.last_input())
```

### 1.6 Computed State — Integrated, Not Bolted On

In Python adk-fluent, `S.compute(**fns)` derives new keys as a regular state
transform — same namespace, same evaluation model. In our Rust implementation,
`ComputedRegistry` stores derived values at `derived:*` with a separate
`ReadOnlyPrefixedState` accessor.

This isolation is **correct for safety** (prevents accidental overwrites of
derived values) but creates a **cognitive split**: developers must remember that
`derived:risk` is different from `app:risk`.

**Resolution**: Keep the `derived:` prefix and read-only access, but make computed
vars accessible via the normal `state.get()` path. When `state.get("risk")` is
called and `"risk"` doesn't exist, automatically check `"derived:risk"` as
fallback. This preserves safety while removing the prefix tax:

```rust
// CURRENT: must know the prefix
let risk: f64 = state.derived().get("risk").unwrap_or(0.0);

// PROPOSED: transparent fallback (get checks derived: automatically)
let risk: f64 = state.get("risk").unwrap_or(0.0);
// Still works:
let risk: f64 = state.derived().get("risk").unwrap_or(0.0);
// Direct write still blocked:
state.set("derived:risk", 0.5);  // ERROR: derived: is read-only
```

### 1.7 Gemini Live vs Other LLMs

The State architecture is **entirely LLM-agnostic**. Nothing in `State`,
`PrefixedState`, `ComputedRegistry`, or `WatcherRegistry` knows about Gemini.
The only Gemini-specific aspect is `SessionSignals`, which translates
Gemini-specific `SessionEvent` variants to `session:*` keys. For other LLMs,
a different `SignalProvider` trait could map their events to the same keys.

**No changes needed** for multi-LLM support — State is already portable.

---

## 2. Context (C) — What the Model Sees Right Now

### 2.1 Current Architecture

**L0 (gemini-live)**: Context is modeled as `Content` (role + parts) and `Part`
(text, inline data, function calls, etc.). The session maintains a turn history
in `SessionState`. Context window management is handled via
`ContextWindowCompressionConfig` (trigger/target token thresholds) and session
resumption via `SessionResumptionConfig`.

Key types:
```rust
Content { role: Option<Role>, parts: Vec<Part> }
Part { text, inline_data, function_call, function_response, ... }
Role { User, Model, System }
```

Content injection methods:
- `send_client_content(turns, turn_complete)` — inject multi-turn context
- `update_instruction(text)` — update system instruction mid-session
- `send_text(text)` — send user text

**L1 (gemini-adk)**: `TranscriptBuffer` accumulates input/output transcripts and
segments them into turns. It provides windowed access for extractors:

```rust
TranscriptBuffer {
    current_user_text: String,
    current_model_text: String,
    turns: Vec<TranscriptTurn>,
}

fn window(&self, n: usize) -> &[TranscriptTurn]  // last N completed turns
fn format_window(&self, n: usize) -> String       // formatted for LLM prompts
```

`InvocationContext` wraps session, state, and event data for agent execution.

**L2 (gemini-adk-fluent)**: The `C::` composition module provides context engineering:

```rust
C::window(10)          // Last N messages
C::user_only()         // Filter to user messages
C::model_only()        // Filter to model messages
C::exclude_tools()     // Remove tool-related parts
C::truncate(max_chars) // Limit by character count
C::dedup()             // Remove adjacent duplicates
```

### 2.2 What Works

**Content/Part is the right abstraction.** Multi-modal content with typed parts
(text, audio, images, function calls) is flexible and well-designed. The builder
methods (`Content::user()`, `Part::text()`) are ergonomic.

**TranscriptBuffer** correctly handles the complexities of speech-to-speech:
server transcripts overriding accumulated partial transcripts, barge-in
truncation, turn segmentation from continuous audio streams.

**Context compression** at L0 is Gemini's built-in sliding window, which is the
right place for it — the model handles token counting, not the framework.

### 2.3 Context ↔ Channel Mapping

Context operates on **Channel 1** (conversation history) and bridges to
**Channel 3** (instruction). The `C::` module filters and transforms Channel 1
content before the model sees it.

```
Channel 1 (History)                     Channel 3 (Instruction)
─────────────────                       ────────────────────────
  All turns           C::window(10)     Phase instruction
  All roles      →    C::user_only()  →  + instruction_template
  All modalities       C::exclude_tools  + {key} interpolation
```

**Missing bridge**: `C::from_state(*keys)` — In Python adk-fluent, this
explicitly bridges Channel 2 → Channel 3 by injecting state values into the
context. Our Rust equivalent uses `instruction_template()` for this, which works
but conflates context assembly with instruction composition.

**Proposed addition**: `C::from_state()` context policy:
```rust
// Inject state values as context preamble (Channel 2 → Channel 1 → Channel 3)
C::from_state(&["user:name", "app:account_balance", "derived:risk"])
// Compiles to: prepend state values as formatted context before model sees history
// Result: "[Context: name=John, balance=$5,230, risk=0.72]\n{conversation}"
```

This is different from `instruction_template()` because:
- `C::from_state()` injects into conversation history (Channel 1) — visible as context
- `instruction_template()` modifies system instruction (Channel 3) — shapes behavior

Both are useful. C bridges data; P bridges intent.

### 2.4 What's Missing

**2.4.1 No context budget visibility**

Developers can't see how much context they've consumed. In voice applications
with 30+ turns, context pressure is a constant concern. The framework should
expose estimated context usage:

```rust
// PROPOSED: context budget as session signal
state.session().get::<u32>("context_tokens_est")   // Estimated tokens used
state.session().get::<f64>("context_utilization")   // 0.0 - 1.0
```

This would enable computed vars like:
```rust
.computed("should_summarize", &["session:context_utilization"], |state| {
    let util: f64 = state.session().get("context_utilization").unwrap_or(0.0);
    Some(json!(util > 0.7))  // Start summarizing at 70% capacity
})
```

**Implementation**: Gemini's `UsageMetadata` in server messages contains
`total_token_count`. The processor already receives these — pipe them to
`session:context_tokens_est`.

**2.4.2 No structured context injection**

Injecting context requires manually building `Content::user(text)` strings.
For common patterns (RAG results, tool outputs, conversation summaries), the
framework should provide structured injection:

```rust
// PROPOSED: structured context injection via writer
writer.inject_context(ContextEntry {
    source: "knowledge_base",
    content: "Product pricing: Basic $10/mo, Pro $25/mo",
    priority: Priority::High,    // Survives context compression
    ttl: Some(Duration::from_secs(300)),  // Auto-expires
});
```

**For voice**: This is especially important because context injection in
speech-to-speech must be invisible to the user. The framework wraps injections
in `[system: ...]` markers that the model reads but doesn't speak.

**2.4.3 Transcript buffer doesn't track tool context**

The `TranscriptBuffer` tracks user/model text turns but not tool calls and
responses. Extractors that analyze the conversation miss tool interactions
entirely. A tool call that resolved the user's question is invisible to the
extraction LLM.

**Proposed fix**: Include tool summaries in transcript turns:

```rust
struct TranscriptTurn {
    user_text: String,
    model_text: String,
    tool_calls: Vec<ToolCallSummary>,  // NEW: what tools were called and returned
}
```

### 2.5 Gemini Live vs Other LLMs

**Gemini-specific**: Context compression (`ContextWindowCompressionConfig`),
session resumption, `send_client_content` for multi-turn injection, server-side
VAD. These are protocol-level features.

**LLM-agnostic**: The concept of Content/Part, transcripts, windowed history.
For non-Gemini LLMs, context injection would use their native mechanisms
(e.g., OpenAI's messages array, Anthropic's system/user/assistant turns).

**Required abstraction**: A `ContextManager` trait that L0 implements per-LLM:

```rust
trait ContextManager {
    fn inject(&self, content: Content) -> Result<()>;
    fn update_instruction(&self, text: &str) -> Result<()>;
    fn estimated_tokens(&self) -> Option<u32>;
    fn compress(&self) -> Result<()>;
}
```

This keeps the abstraction minimal while allowing LLM-specific optimizations.

---

## 3. Tools (T) — What the Model Can Do

### 3.1 Current Architecture

**L0 (gemini-live)**: Tools are declared via `FunctionDeclaration` (name, description,
JSON Schema parameters) and grouped in `Tool` containers. The model emits
`FunctionCall` events; the user responds with `FunctionResponse`.

```rust
Tool::functions(vec![
    FunctionDeclaration { name, description, parameters: Some(json_schema) },
])
```

Control: `ToolConfig` with `FunctionCallingMode` (Auto/Any/None) and
`FunctionCallingBehavior` (Blocking/NonBlocking).

**L1 (gemini-adk)**: `ToolDispatcher` routes `FunctionCall`s to `ToolFunction`
implementations. Three tool types:

```rust
trait ToolFunction: Send + Sync {
    fn name(&self) -> &str;
    fn declaration(&self) -> FunctionDeclaration;
    async fn call(&self, args: Value, context: ToolContext) -> Result<Value>;
}

trait StreamingTool: Send + Sync { ... }      // Background execution
trait InputStreamingTool: Send + Sync { ... } // Receives live input
```

`BackgroundToolTracker` manages in-flight background tool executions with
cancellation support.

**L2 (gemini-adk-fluent)**: Tool registration through builder methods:

```rust
.tools(dispatcher)                              // Full ToolDispatcher
.on_tool_call(|calls| async { Some(responses) }) // Manual dispatch
.before_tool_response(|responses, state| async { responses }) // Interceptor
```

Composition module: `T::simple("name", "desc", |args| async { ... })`

### 3.2 What Works

**Three-tier tool types** (regular, streaming, input-streaming) is the right
design. Voice applications need all three: synchronous lookups, background
enrichment, and live input processing (e.g., real-time audio analysis).

**ToolDispatcher** with timeout management and automatic FunctionResponse
construction is clean. The developer implements the logic; the framework handles
the plumbing.

**`before_tool_response` interceptor** is powerful — it enables PII redaction,
response enrichment, and state promotion without modifying tool implementations.

### 3.3 What's Broken

**3.3.1 `on_tool_call` doesn't receive State**

This is the biggest ergonomic gap in the tool system. The callback signature:

```rust
on_tool_call: |Vec<FunctionCall>| -> Option<Vec<FunctionResponse>>
```

Cannot access `State`. This means:
- Can't set `identity_verified` when a tool returns `verified: true`
- Can't read state to conditionally modify tool behavior
- Forces use of `before_tool_response` as a workaround

**Proposed fix**:

```rust
// PROPOSED: on_tool_call receives State
.on_tool_call(|calls, state| async move {
    let mut responses = Vec::new();
    for call in &calls {
        let result = execute(&call.name, &call.args);
        // Natural state promotion
        if call.name == "verify_identity" {
            if result["verified"] == true {
                state.set("identity_verified", true);
            }
        }
        responses.push(FunctionResponse { name: call.name.clone(), response: result, id: call.id.clone() });
    }
    Some(responses)
})
```

This is a **breaking change** to the callback signature. Migration path: add
`on_tool_call_with_state()` alongside the existing `on_tool_call()`, deprecate
the old one.

**3.3.2 Tool declarations are not type-safe**

Tool parameters are raw `serde_json::Value` (JSON Schema). This means schema
errors are runtime failures, not compile-time errors:

```rust
// CURRENT: raw JSON, no validation
FunctionDeclaration {
    name: "search".into(),
    parameters: Some(json!({"type": "object", "properties": {"query": {"type": "string"}}})),
    ..
}
```

**Proposed improvement**: Derive tool schemas from Rust types:

```rust
// PROPOSED: derive schemas from types
#[derive(ToolArgs)]
struct SearchArgs {
    /// The search query
    query: String,
    /// Maximum number of results
    #[tool(default = 10)]
    max_results: Option<u32>,
}

// Auto-generates FunctionDeclaration with correct JSON Schema
T::typed::<SearchArgs>("search", "Search the knowledge base", |args: SearchArgs| async {
    // args is already deserialized and validated
    Ok(json!({"results": search(args.query, args.max_results.unwrap_or(10))}))
})
```

This would use `schemars::JsonSchema` (already in workspace) to generate schemas
at compile time.

**3.3.3 No phase-scoped tool activation**

The design doc describes phase-scoped tools:
```rust
.phase("ordering").tools_enabled(&["search_menu", "add_item"])
```

But since Gemini Live doesn't support changing tools mid-session, this must be
implemented as **tool call filtering**: declare all tools at setup, reject calls
to disabled tools at runtime. This is described in the design doc but not
implemented.

**Proposed implementation**: Phase stores allowed tools. The processor checks the
phase's tool list before dispatching. Rejected calls get an error response:

```rust
if !current_phase.tools.is_empty() && !current_phase.tools.contains(&call.name) {
    return FunctionResponse {
        name: call.name,
        response: json!({"error": "This action is not available in the current phase."}),
        id: call.id,
    };
}
```

### 3.4 Gemini Live vs Other LLMs

**Gemini-specific**: `FunctionCallingBehavior::NonBlocking` (parallel tool
execution), `google_search`/`code_execution`/`url_context` built-in tools,
tool call IDs.

**LLM-agnostic**: Tool declaration (name + description + schema), call/response
cycle, tool dispatching, timeout management.

**Abstraction needed**: The `FunctionDeclaration` → `FunctionCall` →
`FunctionResponse` pattern is universal. The framework's tool types
(`ToolFunction`, `StreamingTool`) are already LLM-agnostic.

---

## 4. Prompt (P) — What the Model Should Be

### 4.1 Current Architecture

**L0 (gemini-live)**: System instruction set once via `SessionConfig::system_instruction()`.
Can be updated mid-session via `SessionCommand::UpdateInstruction`. Generation
parameters (temperature, top_p, etc.) set at config time.

**L1 (gemini-adk)**: Three instruction mechanisms:

1. **Static instruction**: Set at session config time
2. **`instruction_template`**: Closure evaluated on every turn, receives `&State`,
   returns `Option<String>`. If `Some`, replaces the system instruction.
3. **Phase instructions**: Each phase has a static or dynamic instruction. On
   phase transition, the processor calls `writer.update_instruction()`.

Template interpolation via `inject_session_state()`:
```rust
"Hello {user:name}, you have {app:items} items."  // Required keys
"Your score: {derived:score?}"                     // Optional keys (omitted if missing)
```

**L2 (gemini-adk-fluent)**: Builder methods:

```rust
.instruction("You are a helpful assistant")
.instruction_template(|state| Some(format!("Phase: {}", state.get("session:phase")?)))
.phase("ordering")
    .instruction("Help with ordering.")
    .dynamic_instruction(|state| format!("Items: {}", state.get("items")?))
```

### 4.2 What Works

**Phase-based instruction switching** is the killer feature. Instead of one
monolithic system prompt with `if phase == X then Y`, each phase has its own
focused instruction. The PhaseMachine updates the instruction atomically on
transition. This produces dramatically better model behavior.

**`instruction_template` as a reactive function** is elegant. It runs after
extractors, computed vars, and phase transitions — so it sees fully consistent
state. It can augment the phase instruction with runtime context.

**Template interpolation** (`{key}` and `{key?}`) is simple and effective for
common cases where the instruction needs to include state values.

### 4.3 What's Missing

**4.3.1 Instruction layering / composition**

Currently, `instruction_template` replaces the entire instruction. This means
the developer must reconstruct the phase instruction inside the template:

```rust
// CURRENT: must repeat phase instruction in template
.instruction_template(|state| {
    let phase: String = state.get("session:phase").unwrap_or_default();
    let base = match phase.as_str() {
        "ordering" => ORDERING_INSTRUCTION,
        "confirming" => CONFIRMING_INSTRUCTION,
        _ => DEFAULT_INSTRUCTION,
    };
    let risk: String = state.get("derived:risk").unwrap_or_default();
    Some(format!("{base}\n\n[Risk level: {risk}]"))  // Must include base!
})
```

This is redundant — the phase machine already manages base instructions.

**Proposed fix**: Instruction layering. The template returns an *amendment*, not
a replacement. The framework composes: `phase_instruction + template_amendment`:

```rust
// PROPOSED: instruction_amendment only adds to the phase instruction
.instruction_amendment(|state| {
    let risk: String = state.get("derived:risk").unwrap_or_default();
    if risk == "high" {
        Some("[IMPORTANT: Use empathetic language. Do not threaten.]".into())
    } else {
        None  // No amendment needed
    }
})
```

The processor composes: `phase.instruction + "\n\n" + amendment`. The developer
never needs to know or repeat the base instruction.

**Backward compatibility**: `instruction_template` remains as the full-replacement
escape hatch. `instruction_amendment` is the new, preferred API.

**4.3.2 No instruction history / diff**

When debugging, developers can't see what instruction the model is currently
operating under, or how it changed. Every `update_instruction` call is a black box.

**Proposed fix**: Track instruction history in state:

```rust
state.session().set("instruction_hash", hash(&current_instruction));
state.session().set("instruction_updated_at_turn", turn_count);
// In debug mode:
#[cfg(debug_assertions)]
state.session().set("instruction_text", current_instruction);
```

**4.3.3 No prompt versioning**

In production, operators want to A/B test different instructions for the same
phase. Currently this requires code changes.

**Proposed enhancement** (future): Instruction variants:

```rust
.phase("ordering")
    .instruction_variant("control", "Help with ordering.")
    .instruction_variant("concise", "Take orders. Be brief.")
    .instruction_variant("upsell", "Help with ordering. Suggest drinks and desserts.")
```

Selection via state: `state.set("instruction_variant", "upsell")`. This enables
runtime experimentation without redeployment.

### 4.4 Gemini Live vs Other LLMs

**Gemini-specific**: Mid-session instruction updates via WebSocket command. Most
other LLMs don't support changing the system prompt mid-conversation without
re-sending the entire history.

**LLM-agnostic**: The concept of layered instructions (base + phase + amendment)
is universal. For LLMs without mid-session updates, the framework would prepend
the instruction to the next user turn instead.

---

## 5. Middleware (M) — What Happens Before/After Every Action

### 5.1 Current Architecture

**L0 (gemini-live)**: No middleware system. Extension via traits:
- `Transport` — custom WebSocket implementations
- `Codec` — custom serialization
- `AuthProvider` — custom authentication
- `SessionWriter`/`SessionReader` — wrappable session interfaces

These are correct for L0 — the wire layer should expose extension points, not
a middleware chain.

**L1 (gemini-adk)**: `Middleware` trait for agent execution lifecycle:

```rust
trait Middleware: Send + Sync {
    async fn before_agent(&self, context: &mut InvocationContext) -> Result<()> { Ok(()) }
    async fn after_agent(&self, context: &mut InvocationContext, result: &Result<Value>) -> Result<()> { Ok(()) }
    async fn before_tool(&self, name: &str, args: &Value, context: &ToolContext) -> Result<()> { Ok(()) }
    async fn after_tool(&self, name: &str, result: &Result<Value>, context: &ToolContext) -> Result<()> { Ok(()) }
    async fn on_error(&self, error: &Error, context: &InvocationContext) -> Result<ErrorAction> { Ok(ErrorAction::Propagate) }
}
```

`MiddlewareChain` composes multiple middleware in order. Built-ins: `LogMiddleware`,
`RetryMiddleware`.

**But**: This middleware system is for the **agent** execution loop (text agents,
REST API calls), **not** for the **Live session** event loop. The Live processor
has its own interception points that are completely separate:

```rust
// Live interceptors (in callbacks, not middleware):
before_tool_response: |responses, state| async { ... }
on_turn_boundary: |state, writer| async { ... }
instruction_template: |state| -> Option<String>
on_extracted: |name, value| async { ... }
```

This creates a **split brain**: middleware for agents, interceptors for Live
sessions. Same concepts, different APIs.

**L2 (gemini-adk-fluent)**: The `M::` composition module provides middleware primitives:

```rust
M::log()
M::retry(3)
M::timeout(Duration::from_secs(10))
M::circuit_breaker(5)
M::validate(|call| Ok(()))
M::before_tool(|call| Ok(()))
```

### 5.2 What Works

**The Live interceptors are well-positioned in the lifecycle.** `before_tool_response`
fires at exactly the right moment (after tool execution, before model sees
results). `on_turn_boundary` fires at the right moment (after all state updates,
before next turn). These are the correct extension points for voice applications.

**Middleware composition** (`M::log() | M::retry(3)`) is clean and provides
the right building blocks for production deployments.

### 5.3 The Callback vs Middleware Distinction

Before listing what's broken, we must clarify the fundamental distinction that
Python adk-fluent makes explicit and our Rust code conflates:

```
┌─────────────────────────────────────────────────────────────────┐
│  CALLBACKS = per-agent/per-session lifecycle hooks              │
│  • Attached to a specific agent or session                     │
│  • Run in the agent/session's context                          │
│  • Purpose: domain logic (state promotion, context injection)  │
│  • Examples: on_tool_call, on_enter, instruction_template      │
│                                                                 │
│  MIDDLEWARE = app-global cross-cutting concerns                 │
│  • Wraps ALL agents/sessions uniformly                         │
│  • Independent of specific agent logic                          │
│  • Purpose: observability, resilience, policy                  │
│  • Examples: logging, retry, cost tracking, rate limiting       │
└─────────────────────────────────────────────────────────────────┘
```

Our Rust code has both, but the naming obscures the distinction:
- **Callbacks** (correctly scoped): `on_tool_call`, `on_turn_boundary`,
  `before_tool_response`, `instruction_template`, phase `on_enter`/`on_exit`
- **Middleware** (correctly scoped): `M::log()`, `M::retry()`, `M::timeout()`,
  `MiddlewareChain`

The "split brain" is actually **correct architecture** — it just needs clearer
documentation. Callbacks instrument specific behavior. Middleware instruments
the entire execution.

### 5.4 What's Broken

**5.4.1 Two unrelated interception systems**

The `Middleware` trait (for agents) and the Live interceptors (for sessions) solve
related problems but share no code, no types, and no mental model. A developer
who learns `Middleware` can't apply that knowledge to Live sessions, and vice versa.

**Resolution**: These are intentionally different (per-agent vs per-session), but
the documentation should present them as two instances of the same pattern:

```
Agent Middleware        Live Callbacks            Unified Concept
─────────────────      ──────────────────        ────────────────
before_agent           on_connected              Lifecycle.before
after_agent            on_disconnected           Lifecycle.after
before_tool            on_tool_call              Tool.before
after_tool             before_tool_response      Tool.after
on_error               on_error                  Error.handle
(none)                 on_turn_boundary          Turn.boundary
(none)                 instruction_template      Turn.instruction
(none)                 on_extracted              Extraction.after
```

The naming doesn't need to change — but the **documentation and mental model**
should present them as instances of the same pattern. And the `M::` composition
module should provide Live-specific middleware factories alongside agent ones.

**5.4.2 No middleware for Live event processing**

The Live processor has no way to add cross-cutting behavior to event processing
itself. For example:
- Logging every event with timing
- Metrics collection (events/sec, processing latency)
- Event filtering (drop certain events)
- Event transformation (modify events before callbacks see them)

Currently, these require modifying the processor itself.

**Proposed addition**: `EventMiddleware` for the Live processor:

```rust
trait EventMiddleware: Send + Sync {
    /// Called before event processing. Return false to drop the event.
    fn before_event(&self, event: &SessionEvent, state: &State) -> bool { true }
    /// Called after event processing.
    fn after_event(&self, event: &SessionEvent, state: &State, elapsed: Duration) {}
}
```

This enables:
```rust
.event_middleware(MetricsMiddleware::new())
.event_middleware(EventFilter::drop(|e| matches!(e, SessionEvent::AudioData(_))))
```

**5.4.3 No middleware ordering guarantees**

Multiple middleware instances have no explicit ordering. In practice, order
matters: a retry middleware must wrap a timeout middleware, not the other way
around.

**Proposed fix**: Named layers with explicit ordering:

```rust
.middleware_layer("auth", AuthMiddleware::new())
.middleware_layer("retry", RetryMiddleware::new(3))
    .after("auth")
.middleware_layer("logging", LogMiddleware::new())
    .wrap_all()  // outermost layer
```

### 5.5 Gemini Live vs Other LLMs

**Gemini-specific**: The Live interceptor positions (before_tool_response,
on_turn_boundary) are tied to Gemini's event model. Other real-time LLMs
(OpenAI Realtime API) have similar but not identical event lifecycles.

**LLM-agnostic**: The concept of before/after hooks, event filtering, and
cross-cutting concerns is universal.

---

## 6. Phase — The Sixth Primitive (First-Class Conversation Stage)

Phase is not merely a state variable — it is a **first-class conversation primitive**
that composes all five S.C.T.P.M primitives into coherent, bounded stages.

### 6.1 Why Phase Deserves First-Class Status

Each phase represents a complete configuration slice:

```
Phase "verify_identity"
  ├── S: guard(|s| s.get("disclosure_given") == true)     // state precondition
  ├── C: instruction includes identity context             // context shaping
  ├── T: tools_enabled(["verify_identity", "lookup_account"]) // tool filter
  ├── P: instruction("Verify the debtor's identity...")    // model prompt
  └── M: on_enter/on_exit callbacks                        // lifecycle hooks
```

A phase transition atomically reconfigures the model's behavior by changing
prompt, tools, and constraints in a single operation. This is fundamentally
different from just setting `state["phase"] = "verify"`.

### 6.2 Current Implementation

`PhaseMachine` (L1) already provides:
- Declarative phase definitions with guards, transitions, instructions
- Transition history via `Vec<PhaseTransition>` recording `{from, to, turn, timestamp}`
- Guard evaluation: transition guard fires → target phase guard checked → enter/exit callbacks
- Phase-specific tool filtering (declared but not enforced at runtime — see §3.3.3)

### 6.3 Enriched Phase Transition History

The current transition record (`from, to, turn, timestamp`) is insufficient for
debugging and analytics. Phase transitions should record *why* they happened:

```rust
// CURRENT
struct PhaseTransition {
    from: String,
    to: String,
    turn: u32,
    timestamp: Instant,
}

// PROPOSED: enriched transition with causality
struct PhaseTransition {
    from: String,
    to: String,
    turn: u32,
    timestamp: Instant,
    trigger: TransitionTrigger,       // WHY did this transition fire?
    duration_in_phase: Duration,       // How long were we in the previous phase?
    state_snapshot: Option<Value>,     // Key state at transition time (debug only)
}

enum TransitionTrigger {
    /// Guard predicate on a named transition returned true
    Guard { transition_name: String },
    /// Explicit programmatic transition via writer
    Programmatic { source: &'static str },
    /// Temporal pattern fired
    Temporal { pattern_name: String },
    /// Watcher action triggered transition
    Watcher { key: String },
}
```

This enables:
```rust
// Analytics: "How long do users spend in each phase?"
let history = phase_machine.history();
let avg_verify_time = history.iter()
    .filter(|t| t.from == "verify_identity")
    .map(|t| t.duration_in_phase)
    .sum::<Duration>() / count;

// Debugging: "Why did we skip to closing?"
let last = history.last().unwrap();
match &last.trigger {
    TransitionTrigger::Guard { transition_name } =>
        println!("Guard '{}' fired at turn {}", transition_name, last.turn),
    TransitionTrigger::Watcher { key } =>
        println!("Watcher on '{}' triggered phase change", key),
    _ => {}
}
```

### 6.4 Phase Timeline Visualization

With enriched history, the framework can render a phase timeline:

```
Turn 1-3:  [greeting     ] ──guard:disclosure_given──→
Turn 4-7:  [verify_identity] ──guard:identity_verified──→
Turn 8-15: [inform_debt   ] ──watcher:cease_desist──→
Turn 16:   [close         ] (terminal)

Total: 16 turns, 4 phases, 3 transitions
Avg phase duration: 4.0 turns
```

This is invaluable for conversation flow optimization.

### 6.5 Phase Composition with Other Primitives

Phase is the **orchestrator** — it doesn't replace S.C.T.P.M, it composes them:

| Phase Method | Primitive | Effect |
|-------------|-----------|--------|
| `.guard(predicate)` | S (State) | Reads state to decide entry |
| `.instruction(text)` | P (Prompt) | Sets model behavior for this phase |
| `.tools_enabled(list)` | T (Tools) | Filters available tools |
| `.on_enter(callback)` | M (Middleware) | Lifecycle hook on entry |
| `.on_exit(callback)` | M (Middleware) | Lifecycle hook on exit |
| `.transition(target, guard)` | S (State) | State-driven transition |
| `.dynamic_instruction(fn)` | P + S | State-reactive prompt |

---

## 7. The Expression IR — Frozen Descriptors That Compile

The L2 composition modules (S, C, P, T, M, A) follow a pattern borrowed from
compiler design: they produce **frozen descriptors** that describe *what* should
happen, not *how*. These descriptors are compiled to runtime behavior at build time.

```
Developer writes    →  Frozen Descriptor      →  Compiled to Runtime
─────────────────      ──────────────────        ───────────────────
S::pick("a","b")   →  StateTransform(name,fn)  →  FnAgent (zero-cost, no LLM)
C::window(10)      →  ContextPolicy(name,fn)   →  Filter function
P::role("expert")  →  PromptSection(Role,text)  →  Instruction string
M::retry(3)        →  RetryMiddleware           →  Middleware chain
T::simple(...)     →  ToolCompositeEntry        →  ToolDispatcher entry
```

### Why This Matters

1. **Introspection**: Frozen descriptors can be inspected before execution.
   `check_contracts()` validates state read/write contracts at build time.

2. **Composition**: Descriptors combine via operators (`>>`, `+`, `|`, `*`, `/`).
   The algebra is closed — combining two descriptors produces another descriptor.

3. **Diagnostics**: The framework can generate a "what will happen" report
   without executing anything.

4. **Reuse**: A `P::role("expert") + P::task("analyze data")` can be shared
   across multiple agents — it's data, not code.

### Live Session vs Text Agent Compilation

For **text agents** (request-response), compilation produces an agent tree:
```
researcher >> (writer | reviewer) >> feedback * 3
  → Pipeline([Agent, FanOut([Agent, Agent]), Loop(Agent, max=3)])
  → compile(llm) → Arc<dyn TextAgent>
```

For **Live sessions** (stateful WebSocket), compilation configures the processor:
```
Live::builder()
  .phase("greeting").instruction("...").guard(|s| ...).done()
  .watch("score").crossed_above(0.9).then(|old,new,state| async { ... })
  .computed("risk", &["score","intent"], |s| Some(json!(...)))
  → LiveSessionBuilder with PhaseMachine, WatcherRegistry, ComputedRegistry
  → connect() → LiveHandle (running processor)
```

The key difference: text agents compile to a *tree of agents*; Live sessions
compile to a *configured event processor*. Both start as frozen descriptors.

---

## 8. Cross-Cutting Concerns

### 8.1 The Interplay Between Primitives

The five primitives are not independent. They interact in specific, predictable
ways that the framework should make explicit:

```
                    ┌──────────┐
                    │  Prompt  │ ←── instruction_template reads State
                    │  (P)     │ ←── phase instructions from PhaseMachine
                    └────┬─────┘
                         │ shapes model behavior
                         ▼
                    ┌──────────┐
                    │  Model   │
                    │ (Gemini) │
                    └────┬─────┘
                    ╱    │    ╲
              text ╱     │     ╲ tool calls
                  ╱      │      ╲
      ┌──────────┐  ┌────┴─────┐  ┌──────────┐
      │ Context  │  │  State   │  │  Tools   │
      │  (C)     │  │  (S)     │  │  (T)     │
      └────┬─────┘  └────┬─────┘  └────┬─────┘
           │              │              │
           │         extractors    tool responses
           │         computed      write state
           │         watchers      via before_tool_response
           │              │              │
           └──────────────┴──────────────┘
                         │
                    ┌────┴─────┐
                    │Middleware│ ←── wraps everything
                    │  (M)     │
                    └──────────┘
```

### 8.2 The Reactive State Graph

The most powerful pattern in this architecture is the **reactive state graph**:

```
User says "I dispute this debt"
  → Extractor: DebtorState.negotiation_intent = "dispute" (flattened to top-level)
    → Computed: call_risk_level recalculates (depends on negotiation_intent)
      → Watcher: negotiation_intent.changed_to("dispute") fires → send alert
        → Phase guard: inform_debt → close transition fires (dispute detected)
          → Phase.on_exit: log compliance event
            → Phase.on_enter(close): deliver closing instruction
              → instruction_template: includes risk level and dispute status
                → Model: "I understand you're disputing this debt. A validation
                          notice will be sent within 5 business days..."
```

One user utterance triggers a cascade through extractors → computed → watchers →
phases → instructions → model response. This is the architecture working as
designed — reactive state propagation through declared relationships.

### 8.3 Performance Budget

For a voice application at 25 events/second:

| Component | Budget | Actual | Status |
|-----------|--------|--------|--------|
| Fast lane callbacks (audio, text) | < 100μs | ~10μs | ✅ |
| State read (DashMap) | < 1μs | ~50ns | ✅ |
| State write (DashMap) | < 1μs | ~100ns | ✅ |
| Computed recompute (5 vars) | < 1ms | ~100μs | ✅ |
| Watcher evaluate (5 watchers) | < 1ms | ~50μs | ✅ |
| Phase transition | < 1ms | ~200μs | ✅ |
| Extractor LLM call | < 5s | 1-3s | ⚠️ (async, non-blocking) |
| instruction_template | < 1ms | ~100μs | ✅ |
| Auto-flatten (5 fields) | < 100μs | ~5μs | ✅ |

The only expensive operation is LLM extraction, which is inherently async and
runs on the control lane without blocking audio. Everything else is well within
budget. **No performance changes needed.**

---

## 9. Proposed Changes — Priority Ordered

### Tier 1: Do Now (high impact, low effort)

| # | Change | Layer | Effort | Impact |
|---|--------|-------|--------|--------|
| 1 | ~~Auto-flatten extractor results~~ | L1 | ✅ Done | Watchers/computed/guards work with flat keys |
| 2 | `on_tool_call` receives State | L1+L2 | ~30 lines | Natural state promotion from tool results |
| 3 | `instruction_amendment` | L1+L2 | ~40 lines | Instruction layering without repetition |
| 4 | `State::modify()` | L1 | ~20 lines | Atomic read-modify-write |
| 5 | `State::get()` derived fallback | L1 | ~10 lines | Transparent access to computed vars |

### Tier 2: Do Next (high impact, medium effort)

| # | Change | Layer | Effort | Impact |
|---|--------|-------|--------|--------|
| 6 | Phase-scoped tool filtering | L1 | ~50 lines | Tools restricted per phase |
| 7 | Enriched phase transition history | L1 | ~60 lines | TransitionTrigger, duration, debugging |
| 8 | Tool call summaries in transcript | L1 | ~30 lines | Extractors see full context |
| 9 | Context token estimation in session signals | L1 | ~20 lines | Context budget visibility |
| 10 | `C::from_state()` context policy | L2 | ~40 lines | Channel 2 → Channel 1 bridge |

### Tier 3: Do Later (medium impact, higher effort)

| # | Change | Layer | Effort | Impact |
|---|--------|-------|--------|--------|
| 11 | `EventMiddleware` for Live processor | L1+L2 | ~100 lines | Cross-cutting event processing |
| 12 | `S::capture()` — Channel 1 → Channel 2 | L2 | ~30 lines | Simple text capture without extractor |
| 13 | State provenance tracking (debug mode) | L1 | ~60 lines | Debugging state mutations |
| 14 | Structured context injection | L1+L2 | ~100 lines | Priority-based, TTL-aware context |
| 15 | Instruction variant system | L1+L2 | ~80 lines | A/B testing instructions |
| 16 | Typed tool args via `#[derive(ToolArgs)]` | L1+L2 | ~150 lines | Compile-time schema validation |

### Tier 4: Future (requires design iteration)

| # | Change | Layer | Effort | Impact |
|---|--------|-------|--------|--------|
| 17 | Multi-LLM `ContextManager` trait | L0 | ~200 lines | LLM-agnostic context management |
| 18 | Multi-LLM `SignalProvider` trait | L1 | ~100 lines | LLM-agnostic session signals |
| 19 | Phase timeline visualization helper | L1 | ~80 lines | Debug/analytics for conversation flow |
| 20 | Unified middleware documentation | Docs | ~2 hours | Single mental model for M |

---

## 10. What Stays Unchanged

The following are well-designed and should not be modified:

- **DashMap-based State**: Correct concurrency model for voice
- **Two-lane processor**: Fast lane / control lane separation is essential
- **Prefix namespacing**: `session:`, `derived:`, `turn:`, etc.
- **ComputedRegistry topology sort**: Correct dependency evaluation
- **WatchPredicate enum**: Covers all common patterns
- **TranscriptBuffer**: Handles speech-to-speech complexities well
- **Phase machine with guards**: Declarative, correct evaluation order
- **L0 wire types** (Content, Part, Role): Clean, multi-modal, extensible
- **Builder pattern at L2**: Ergonomic, discoverable, type-safe
- **Operator algebra** (>>, |, *, /): Powerful composition for text agents
- **Frozen descriptor pattern**: S/C/P/T/M/A modules as expression IR
- **Callback vs Middleware split**: Per-session callbacks + global middleware
- **Three-channel separation**: History, State, Instruction as independent concerns
- **Copy-on-write AgentBuilder**: Thread-safe, cloneable agent templates

---

## Appendix A: State Key Convention Reference

```
Prefix         Owner              Lifecycle          Access
─────────────  ──────────────     ─────────────      ──────────────
session:       Processor          Session            Read-only to user
turn:          Processor          Cleared each turn  Read-write
app:           Developer          Session            Read-write
user:          Developer          Session            Read-write
temp:          Developer          Session            Read-write
bg:            Background tasks   Session            Read-write
derived:       ComputedRegistry   Recomputed/turn    Read-only to user
(no prefix)    Extractors         Session            Read-write (auto-flattened)
```

## Appendix B: Callback Signature Reference

```
FAST LANE (sync, < 100μs, no State access):
  on_audio:              |&Bytes|
  on_text:               |&str|
  on_text_complete:      |&str|
  on_input_transcript:   |&str, bool|
  on_output_transcript:  |&str, bool|
  on_vad_start:          ||
  on_vad_end:            ||

CONTROL LANE (async, can block):
  on_tool_call:          |Vec<FunctionCall>| async → Option<Vec<FunctionResponse>>
  on_interrupted:        || async {}
  on_turn_complete:      || async {}
  on_go_away:            |Duration| async {}
  on_connected:          || async {}
  on_disconnected:       |Option<String>| async {}
  on_error:              |String| async {}

INTERCEPTORS (async, receive State and/or Writer):
  before_tool_response:  |Vec<FunctionResponse>, State| async → Vec<FunctionResponse>
  on_turn_boundary:      |State, Arc<dyn SessionWriter>| async {}
  instruction_template:  |&State| → Option<String>
  on_extracted:          |String, Value| async {}

PHASE CALLBACKS (async, receive State + Writer):
  on_enter:              |State, Arc<dyn SessionWriter>| async {}
  on_exit:               |State, Arc<dyn SessionWriter>| async {}

WATCHERS (async, receive old/new/state):
  then:                  |Value, Value, State| async {}

TEMPORAL (async, receive State + Writer):
  action:                |State, Arc<dyn SessionWriter>| async {}
```

## Appendix C: Three-Channel Reference

```
Channel 1: Conversation History
  Written by:  Transcripts, context injection, tool call/response events
  Read by:     Context filters (C::), extractors, model
  Rust types:  Content, Part, TranscriptBuffer, TranscriptTurn
  Operations:  C::window(), C::user_only(), C::from_state() (proposed)

Channel 2: Session State
  Written by:  Extractors (auto-flatten), tools (via interceptors), computed vars,
               session signals, developer code
  Read by:     Guards, computed vars, watchers, instruction templates, developers
  Rust types:  State, PrefixedState, ReadOnlyPrefixedState
  Operations:  S::pick(), S::rename(), S::flatten(), state.get/set()

Channel 3: Instruction Templating
  Written by:  Phase machine, instruction_template, instruction_amendment (proposed)
  Read by:     Model (system prompt)
  Rust types:  PhaseInstruction, String
  Operations:  P::role(), P::task(), {key} interpolation, update_instruction()
```

## Appendix D: Phase Transition Record (Proposed)

```rust
struct PhaseTransition {
    from: String,                      // Source phase
    to: String,                        // Target phase
    turn: u32,                         // Turn number at transition
    timestamp: Instant,                // Wall clock time
    trigger: TransitionTrigger,        // What caused this transition
    duration_in_phase: Duration,       // Time spent in source phase
}

enum TransitionTrigger {
    Guard { transition_name: String }, // Named transition guard fired
    Programmatic { source: &'static str }, // Explicit code-driven transition
    Temporal { pattern_name: String }, // Temporal pattern triggered
    Watcher { key: String },           // Watcher action triggered
}
```
