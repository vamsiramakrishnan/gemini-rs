# Fluent DevEx Redesign — Bridging adk-fluent Patterns to Live API Middleware

**Date**: 2026-03-02
**Status**: Design RFC
**Scope**: Foundational changes across `gemini-adk-rs` (L1) and `gemini-adk-fluent-rs` (L2)
**Reference**: `/tmp/adk-fluent` (Python ADK Fluent — the gold standard for text agent DX)

---

## Executive Summary

**Gemini Live is fundamentally different from text LLM APIs.** It is a persistent,
stateful WebSocket session where the model drives the conversation continuously.
There is no request-response cycle. There is no "between turns" gap where application
code controls execution. The developer's ONLY interfaces are: **(1) event callbacks**
— reacting to what the model produces (audio, text, tool calls, turn completions),
and **(2) tool calls** — the sole mechanism for the model to reach external systems,
and our richest channel for injecting structured data back into the conversation.

This means the fluent composition patterns from text agent frameworks (like Python's
adk-fluent) **cannot work the same way for Gemini Live.** A text agent's
`a >> b >> c` pipeline runs when the developer calls `pipeline.run()`. In a live
session, there is no `run()` — there is only "configure callbacks, connect, and
react." Agent pipelines, operator algebra, middleware, and state transforms must
be re-imagined as **event-triggered reactions** rather than developer-driven
execution sequences.

The Python `adk-fluent` library provides an excellent DX for text agents: operator
algebra, dispatch/join primitives, per-agent callback stacks, rich middleware with
topology hooks, and conditional control flow. Our Rust `gemini-adk-fluent-rs` crate has
adopted the operator algebra and composition modules (P, C, S, M, T, A), but is
missing the **runtime primitives** that bridge these patterns into the live session
event loop.

This document:
1. Establishes the fundamental constraint (Section 2): the WebSocket session model,
   the two interfaces (callbacks + tools), and why text agent patterns don't map
   directly
2. Identifies gaps vs adk-fluent (Section 1): what we're missing from the text agent
   world that still applies
3. Proposes foundational primitives (Section 3): FnStep, dispatch/join, callback
   stacks, enhanced middleware, Route/Gate
4. Defines the Live Bridge (Section 4): four specific mechanisms connecting events
   to agent pipelines — the novel contribution beyond what adk-fluent offers
5. Lists every verb available to developers (Section 6): the complete fluent
   vocabulary after the redesign

---

## 1. What adk-fluent Gets Right (And We Don't Have Yet)

### 1.1 dispatch() / join() — Non-Blocking Background Agents

The single most powerful primitive in adk-fluent. Fire agents in background, continue
pipeline, collect results later:

```python
# adk-fluent (Python)
pipeline = (
    classifier
    >> dispatch(email_sender, seo_optimizer, names=["email", "seo"])
    >> formatter                      # runs immediately, doesn't wait
    >> join("seo", timeout=30)        # wait only for SEO
    >> publisher
    >> join("email")                  # collect email result at the end
)
```

**Our gap**: We have no `dispatch()` or `join()` primitives. The operator algebra
(`>>`, `|`, `*`, `/`) only supports static, synchronous composition. There is no way
to express "fire this agent in background, continue immediately, collect later."

**Why this matters for live**: When `on_extracted` fires with structured data, the user
often wants to dispatch a text agent (summarizer, classifier, formatter) without
blocking the live conversation. Today, the only option is `tokio::spawn` with manual
state coordination. `dispatch()` makes this a first-class verb.

### 1.2 Per-Agent Callback Stacks

adk-fluent provides **eight hook points** per agent, with additive accumulation:

```python
# adk-fluent (Python)
agent = (
    Agent("service")
    .before_model(log_fn)        # before LLM call
    .before_model(metrics_fn)    # both run (additive)
    .after_model(audit_fn)       # after LLM response
    .before_tool(validate_fn)    # before tool dispatch
    .after_tool(enrich_fn)       # after tool result
    .on_model_error(fallback_fn) # error recovery
    .on_tool_error(retry_fn)     # tool error recovery
    .guard(safety_check)         # runs both before + after
)
```

**Our gap**: `AgentBuilder` has zero callback hooks. Agents are opaque — you set
instruction, model, tools, and compile. No way to intercept the agent's execution
at any point.

**Why this matters for live**: When an agent runs as a tool callback or background
task in a live session, you need per-agent observation (latency tracking, error
recovery, output validation) without wrapping everything in custom code.

### 1.3 Rich Middleware Protocol with Topology Hooks

adk-fluent's middleware protocol has **25+ hook points** covering:

```python
# adk-fluent (Python)
class Middleware(Protocol):
    # Agent lifecycle
    async def before_agent(self, ctx, agent_name): ...
    async def after_agent(self, ctx, agent_name): ...

    # Model lifecycle
    async def before_model(self, ctx, request): ...
    async def after_model(self, ctx, response): ...

    # Tool lifecycle
    async def before_tool(self, ctx, tool_name, args): ...
    async def after_tool(self, ctx, tool_name, args, result): ...

    # Dispatch lifecycle
    async def on_dispatch(self, ctx, task_name, agent_name) -> DispatchDirective: ...
    async def on_task_complete(self, ctx, task_name, result): ...
    async def on_join(self, ctx, joined, timed_out): ...

    # Topology (loop, fanout, route, fallback, timeout)
    async def on_loop_iteration(self, ctx, loop_name, i) -> LoopDirective: ...
    async def on_fanout_start(self, ctx, fanout_name, branches): ...
    async def on_route_selected(self, ctx, route_name, selected): ...
    async def on_fallback_attempt(self, ctx, name, agent, attempt, err): ...
```

**Our gap**: Our `Middleware` trait only has 4 methods: `on_event`, `before_tool`,
`after_tool`, `on_tool_error`. No agent lifecycle hooks, no topology hooks, no
dispatch hooks, no control directives (LoopDirective, DispatchDirective).

### 1.4 Inline Code Steps (FnAgent)

adk-fluent lets you put arbitrary code anywhere in a pipeline:

```python
# adk-fluent (Python)
def merge_results(state):
    return {"merged": state["web"] + state["scholar"]}

pipeline = web_agent >> merge_results >> writer_agent
```

**Our gap**: Pipelines only contain `AgentBuilder` nodes. No `FnStep` that runs a
closure with state access. Users must create a dummy agent to run code between
pipeline stages.

### 1.5 Conditional Branching & Route

```python
# adk-fluent (Python)
Route("category")
    .eq("technical", tech_agent)
    .eq("billing", billing_agent)
    .default(general_agent)
```

**Our gap**: No `Route` operator, no `conditional()`, no `Gate` agent. The only
branching is `Fallback` (try-until-success), which is error-driven, not state-driven.

### 1.6 Presets — Reusable Callback/Middleware Bundles

```python
# adk-fluent (Python)
logging_preset = Preset(before_model=log_fn, after_model=log_response_fn)
security_preset = Preset(before_model=safety_check, after_model=audit_fn)

agent.use(logging_preset).use(security_preset)
```

**Our gap**: No `Preset` concept. Callback stacks must be configured per-agent
manually. No reusable bundles.

### 1.7 Scoped & Conditional Middleware

```python
# adk-fluent (Python)
M.scope("writer", M.cost())              # Only applies to "writer" agent
M.when("stream", M.latency())            # Only in streaming mode
M.when(lambda: is_debug(), M.log())      # Conditional on runtime flag
```

**Our gap**: Our `M::` module creates middleware, but no scoping or conditional
application. All middleware applies to everything equally.

---

## 2. The Fundamental Constraint: Gemini Live Is a Stateful WebSocket Session

This section is critical. Everything in this document must be understood through this
lens. **Gemini Live is not a request-response API. It is a persistent, stateful,
full-duplex WebSocket session.** This changes everything about how composition,
middleware, and agent dispatch work compared to text agent frameworks like adk-fluent.

### 2.1 The Two Interfaces — And Only Two

Once a Live session is established, there are exactly **two interfaces** between our
application and the outside world:

```
┌─────────────────────────────────────────────────────────────────┐
│                    Gemini Live WebSocket Session                 │
│                  (stateful, persistent, full-duplex)            │
│                                                                 │
│  ┌─────────────────────┐        ┌────────────────────────────┐ │
│  │  INTERFACE 1:       │        │  INTERFACE 2:              │ │
│  │  Event Callbacks    │        │  Tool Calls                │ │
│  │  (Server → Client)  │        │  (Model → Client → Model)  │ │
│  │                     │        │                            │ │
│  │  on_audio           │        │  Model decides to call     │ │
│  │  on_text            │        │  a function. We execute    │ │
│  │  on_turn_complete   │        │  it and return results.    │ │
│  │  on_extracted       │        │                            │ │
│  │  on_interrupted     │        │  This is the ONLY way      │ │
│  │  on_vad_start/end   │        │  external systems are      │ │
│  │  on_connected       │        │  accessible to the model.  │ │
│  │  on_disconnected    │        │                            │ │
│  │  on_tool_call       │        │  Tool responses are the    │ │
│  │  on_error           │        │  ONLY structured data      │ │
│  │  on_go_away         │        │  we can feed back.         │ │
│  └─────────────────────┘        └────────────────────────────┘ │
│                                                                 │
│  Plus three narrow outbound channels for injection:             │
│  • send_tool_response()    — return tool results                │
│  • send_client_content()   — inject context (turns, summaries)  │
│  • update_instruction()    — change system instruction          │
│                                                                 │
│  Everything else (audio, text, turn management, context window) │
│  is SERVER-MANAGED. We cannot control it. We can only react.    │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 Why This Changes Everything vs Text Agents

In adk-fluent's text agent world, **the developer controls execution flow**:

```python
# Text agent: YOU drive the conversation
response = agent.ask("What is 2+2?")     # YOU call the LLM
result = pipeline.run(state)              # YOU run the pipeline
# Between calls, YOU decide what happens next
# YOU compose agents in sequence: a >> b >> c
# Each agent runs when YOU tell it to
```

In a Live session, **the model drives the conversation**:

```
# Live session: THE MODEL drives
# You set up callbacks ONCE. Then the model talks.
# The model decides when to call tools.
# The model decides when a turn is complete.
# The model decides when to interrupt itself.
# You can only REACT to events as they happen.
# You CANNOT pause the model to run a pipeline.
# You CANNOT "sequence" agents — there is no "between turns" gap you control.
```

**The fundamental inversion**: Text agent frameworks compose agents into pipelines
that the developer executes. Live session frameworks compose **reactions** that fire
when the model produces events. The operator algebra (`>>`, `|`, `*`, `/`) works
when you control execution. In a live session, you must map these operators onto
**event-driven triggers**.

### 2.3 The Two Interfaces in Detail

#### Interface 1: Event Callbacks

These are the hooks defined in the callback-mode-design document. They are the ONLY
way to observe what the model is doing:

| Event | What Fires It | What You Can Do |
|-------|---------------|-----------------|
| `on_audio` | Model produces audio chunk | Play it, relay it, record it |
| `on_text` | Model produces text delta | Display it, log it |
| `on_turn_complete` | Model finishes a turn | Run extractors, update state, inject context |
| `on_extracted` | Extractor produces structured data | Update state, trigger background agents |
| `on_interrupted` | User barged in | Flush audio buffer, cancel pending work |
| `on_tool_call` | Model wants to call a function | Execute it, override it, deny it |
| `on_vad_start/end` | Server detects speech | UI indicators, recording control |
| `on_connected` | Session established | Load profile, init state |
| `on_disconnected` | Session ended | Cleanup, save state |
| `on_go_away` | Session expiring soon | Save state, prepare reconnection |
| `on_error` | Non-fatal error | Log, alert, recover |

Each callback runs with a `CallbackMode` — Blocking (pipeline waits) or Concurrent
(fire-and-forget). See the callback-mode-design document for full analysis.

**Critical constraint**: These callbacks are the ONLY place where application code
runs during a live session. There is no "main loop" the developer controls. There
is no "between turns" gap. The model talks continuously. Your code runs ONLY when
an event fires.

#### Interface 2: Tool Calls

Tools are the ONLY mechanism for the model to interact with external systems.
Everything the model needs to "do" — query a database, call an API, check a status,
perform a calculation — must be exposed as a tool.

Tools are also the primary structured data channel back to the model:

```
Model sees:   System instruction (text, updatable)
              Conversation history (audio/text, server-managed)
              Tool declarations (fixed at setup)
              Tool responses (structured JSON, our main data channel)
              Client content (injected turns, secondary channel)
```

**Tool responses are our richest injection point.** When the model calls `get_order_status`,
our response isn't just the order status — it's an opportunity to inject context,
state, instructions, and guidance into the model's reasoning. The `ResultFormatter`
trait and `before_tool_response` interceptor exist for exactly this reason.

**Non-blocking tools** (see callback-mode-design Section 8) extend this further:
the model calls a tool, gets an immediate ack, keeps talking, and receives the real
result later. This means tool responses can arrive **at any point** during the
model's speech — making them the most flexible injection mechanism we have.

### 2.4 The Execution Model Inversion

In a text agent pipeline, execution flows like this:

```
Developer code                    LLM
─────────────                     ───
pipeline.run(state) ──────────→  agent_a.call()
                    ←──────────  response_a
state.set("a", response_a)
                    ──────────→  agent_b.call()
                    ←──────────  response_b
                                 ...
```

The developer controls the arrow. In a Live session:

```
Developer code                    Gemini Live (continuous)
─────────────                     ─────────────────────────
Live::builder()
  .on_X(callback)
  .connect()       ──────────→   WebSocket established
                                  Model starts talking...
  [waiting]        ←──────────   on_audio(chunk1)
  [waiting]        ←──────────   on_audio(chunk2)
  [waiting]        ←──────────   on_text("Hello")
  [waiting]        ←──────────   on_turn_complete
                                  User speaks...
  [waiting]        ←──────────   on_input_transcript("Hi")
                                  Model responds...
  [waiting]        ←──────────   on_tool_call(get_weather)
  execute tool     ──────────→   tool_response({temp: 22})
  [waiting]        ←──────────   on_audio(chunk_with_weather)
  [waiting]        ←──────────   on_turn_complete
                                  ...
```

The model drives. We react. Our code runs ONLY inside callbacks. We send data back
ONLY through tool responses, client content, and instruction updates.

### 2.5 What This Means for Composition

Given these constraints, the operator algebra and agent pipelines can only execute
in **three contexts**:

#### Context A: Inside a Callback (Event-Triggered)

A pipeline runs when an event fires. The pipeline's output affects state, which
affects future callbacks (instruction_template, on_turn_boundary):

```rust
.on_extracted(|name, value| async move {
    // THIS is where agent pipelines can run
    let result = (classifier >> summarizer).compile(llm).run(&state).await;
    state.set("summary", result);
    // This state is visible to instruction_template on next turn
})
```

The pipeline runs **in reaction to** an event. It cannot initiate its own
conversation with the model. It can only affect state that influences future
model behavior.

#### Context B: Inside a Tool Call (Model-Triggered)

An agent runs as part of tool execution. The agent's output becomes the tool
response that the model sees:

```rust
.agent_tool(deep_analyzer, llm)
// When model calls deep_analyzer(query: "..."), the agent runs
// Agent output IS the tool response — the model reads it directly
```

This is the most powerful integration point: the model explicitly asked for
information, and we can run an entire agent pipeline to generate the answer.
For non-blocking tools, this pipeline runs in the background while the model
keeps talking.

#### Context C: Background (Concurrent, State-Eventual)

A pipeline runs in the background, writing results to state. The live session
picks up these results at the next turn boundary or instruction template check:

```rust
.on_extracted_dispatch(("summarize", summarizer))
// Dispatched agent runs in background
// Writes to state when done
// on_turn_boundary reads state on next turn — eventual consistency
```

The pipeline is decoupled from the live event stream. It cannot inject content
directly into the conversation (no access to `SessionWriter`). It can only
write to shared state.

### 2.6 The Three Injection Points Back Into the Session

When a callback or background agent produces a result, there are exactly three
ways to feed it back into the live conversation:

```rust
// 1. Tool Response — structured data the model explicitly asked for
writer.send_tool_response(vec![FunctionResponse {
    name: "search".into(),
    response: json!({"results": [...]}),
    id: Some("call-1".into()),
}]).await;

// 2. Client Content — inject a turn into the conversation history
// The model sees this as "the user said X" or "context: X"
writer.send_client_content(
    vec![Content::user().text("[Context: Order has 3 items, total $45]")],
    false,  // turn_complete = false (don't end user's turn)
).await;

// 3. Update Instruction — change the system instruction mid-session
writer.update_instruction(
    "You are now in the order confirmation phase. Read back the order."
).await;
```

**Tool responses** are the richest: structured JSON, the model knows it asked for
this data, and it will act on it directly.

**Client content** is contextual: the model sees it in the conversation history, but
it didn't explicitly ask for it. Good for background context injection.

**Instruction updates** are behavioral: they change HOW the model behaves, not WHAT
data it has. Used for phase transitions, persona changes, and constraint updates.

### 2.7 Why adk-fluent's Patterns Don't Map Directly

| adk-fluent Pattern | Live Session Reality |
|-------------------|----------------------|
| `a >> b >> c` (sequential pipeline) | Works INSIDE a callback or tool call. Cannot span across turns — there is no "between turns" where you control execution. |
| `a \| b` (parallel fanout) | Works for background dispatch. But results can only affect the NEXT turn via state — they can't be injected into the CURRENT model response (unless via tool response). |
| `dispatch()` / `join()` | `dispatch()` maps naturally to `CallbackMode::Concurrent`. But `join()` has no natural equivalent — there's no pipeline "step" that waits. Results arrive via state, and the session reads them when it reads them. |
| `Route("key").eq(...)` | Works inside callbacks. But routing decisions happen in event handlers, not between pipeline stages. The "pipeline" is the stream of events, and routing is "which callback to fire." |
| `.before_model()` / `.after_model()` | No direct equivalent. The model runs continuously. There is no "before model call" — the model is always calling. The closest analogy is `instruction_template` (affects what model does) and `on_turn_complete` (after model finishes a turn). |
| `pipeline.run(state)` | No equivalent. You don't "run" a live session. You configure it, connect it, and react to events. The session runs itself. |

### 2.8 The Design Principle

**Everything in a live session is either a reaction to an event or an injection
into the conversation.** The fluent API must make it trivial to:

1. **React**: Attach any code (closure, agent, pipeline) to any event
2. **Inject**: Send results back through the appropriate channel (tool response, client content, instruction update)
3. **Choose timing**: Blocking (must complete before next event) vs Concurrent (fire-and-forget)
4. **Choose consistency**: Strong (blocking callbacks, state always current) vs Eventual (background agents, state updates when ready)

The operator algebra is not the primary composition mechanism for live sessions.
**The primary composition mechanism is the callback registry** — which events
trigger which handlers, in which execution mode, writing to which state keys,
injecting through which channels. The operator algebra is useful WITHIN handlers
for orchestrating complex multi-agent logic.

### 2.9 The Missing Bridges

Given this understanding, the specific bridges we need:

#### Bridge 1: Event → Agent Pipeline

Attach a compiled agent pipeline to a live session event. The pipeline runs inside
the callback, has access to state, and can inject results:

```rust
// "When extraction fires, run this pipeline"
.on_extracted_pipeline(classifier >> summarizer, llm)
```

#### Bridge 2: Tool Call → Agent Execution

Wrap an agent (or pipeline) as a tool. When the model calls the tool, the agent
runs. The agent's output becomes the tool response:

```rust
// "When model calls 'analyze', run this agent"
.agent_tool(analyzer_agent, llm)
```

#### Bridge 3: Background Agent → State → Instruction/Context

Dispatch an agent in background. Its output writes to state. State is read by
`instruction_template` or `on_turn_boundary` on the next turn:

```rust
// "After extraction, run summarizer in background. Use result next turn."
.on_extracted_dispatch(("summarize", summarizer))
.instruction_template(|state| {
    let summary = state.get::<String>("summarize")?;
    Some(format!("Context: {summary}\n\nBe helpful."))
})
```

#### Bridge 4: Non-Blocking Tool → Agent Pipeline → Tool Response

When a non-blocking tool completes, run an agent pipeline on its result. The
pipeline's output is formatted and sent as the tool response:

```rust
// "Search completes → rank results → format → send to model"
.tool_background_with_pipeline("search_menu", ranker >> formatter, llm)
```

---

## 3. Foundational Primitives to Build

### 3.1 `FnStep` — Inline Code in Pipelines

**Layer**: `gemini-adk-fluent-rs` (L2)

The simplest missing primitive. A closure that runs between pipeline stages with
full state access:

```rust
pub struct FnStep {
    name: String,
    handler: Arc<dyn Fn(&mut State) -> BoxFuture<Result<(), AgentError>> + Send + Sync>,
}
```

**Fluent API**:
```rust
let pipeline = researcher
    >> fn_step("merge", |state| async move {
        let web = state.get::<String>("web_results").unwrap_or_default();
        let docs = state.get::<String>("doc_results").unwrap_or_default();
        state.set("combined", format!("{web}\n\n{docs}"));
        Ok(())
    })
    >> writer;
```

**Also accepts bare closures** via `From` impl:
```rust
let pipeline = researcher
    >> |state: &mut State| async move {
        state.set("ready", true);
        Ok(())
    }
    >> writer;
```

**Integration**: Add `FnStep` as a `Composable` variant:
```rust
pub enum Composable {
    Agent(AgentBuilder),
    Pipeline(Pipeline),
    FanOut(FanOut),
    Loop(Loop),
    Fallback(Fallback),
    Step(FnStep),          // NEW
    Route(RouteBuilder),   // NEW (see 3.5)
    Dispatch(DispatchNode), // NEW (see 3.2)
    Join(JoinNode),        // NEW (see 3.2)
}
```

### 3.2 `dispatch()` / `join()` — Background Agent Execution

**Layer**: `gemini-adk-fluent-rs` (L2) with runtime support in `gemini-adk-rs` (L1)

The core non-blocking primitive. Fire agents in background, continue pipeline, collect
results at a later point:

```rust
/// A background task spawned by dispatch().
pub struct DispatchNode {
    agents: Vec<(String, Composable)>,  // (task_name, agent/pipeline)
    on_complete: Option<AsyncCallback<(String, serde_json::Value)>>,
    on_error: Option<AsyncCallback<(String, AgentError)>>,
    max_concurrent: usize,
}

/// A barrier that collects dispatched task results.
pub struct JoinNode {
    names: Vec<String>,       // which tasks to wait for (empty = all)
    timeout: Option<Duration>,
}
```

**Fluent API**:
```rust
use gemini_adk_fluent_rs::prelude::*;

let pipeline = classifier
    >> dispatch(
        ("email", email_sender),
        ("seo", seo_optimizer),
    )
    >> formatter                      // runs immediately
    >> join("seo").timeout(30)        // wait only for SEO
    >> publisher
    >> join("email");                 // collect email at end

// With callbacks:
let pipeline = classifier
    >> dispatch(("analysis", deep_analyzer))
        .on_complete(|name, result| async move {
            println!("Task {name} done: {result}");
        })
        .on_error(|name, err| async move {
            eprintln!("Task {name} failed: {err}");
        })
        .max_concurrent(4)
    >> formatter
    >> join_all();
```

**Runtime support in gemini-adk-rs (L1)**:
```rust
/// Task handle for a dispatched background agent.
pub struct DispatchedTask {
    pub name: String,
    pub handle: JoinHandle<Result<serde_json::Value, AgentError>>,
    pub cancel: CancellationToken,
}

/// Task registry for dispatch/join coordination.
pub struct TaskRegistry {
    tasks: DashMap<String, DispatchedTask>,
    semaphore: Arc<Semaphore>,  // max_concurrent
}
```

### 3.3 Per-Agent Callback Stacks

**Layer**: `gemini-adk-fluent-rs` (L2)

Add callback hooks to `AgentBuilder`. Callbacks are additive — multiple registrations
accumulate:

```rust
impl AgentBuilder {
    // Before/after the agent's LLM call
    fn before_run(self, f: impl AsyncFn(&State)) -> Self
    fn after_run(self, f: impl AsyncFn(&State, &str)) -> Self  // state + output

    // Before/after individual tool calls within the agent
    fn before_tool(self, f: impl AsyncFn(&FunctionCall, &State)) -> Self
    fn after_tool(self, f: impl AsyncFn(&FunctionCall, &Value, &State)) -> Self

    // Error recovery
    fn on_error(self, f: impl AsyncFn(&AgentError, &State) -> Option<String>) -> Self
    fn on_tool_error(self, f: impl AsyncFn(&FunctionCall, &ToolError)) -> Self

    // Guard (runs before, can abort)
    fn guard(self, f: impl Fn(&State) -> bool) -> Self

    // Preset bundles
    fn preset(self, preset: Preset) -> Self
}

/// Reusable callback bundle.
pub struct Preset {
    pub before_run: Vec<AsyncCallback<State>>,
    pub after_run: Vec<AsyncCallback<(State, String)>>,
    pub before_tool: Vec<AsyncCallback<(FunctionCall, State)>>,
    pub after_tool: Vec<AsyncCallback<(FunctionCall, Value, State)>>,
}
```

**Usage**:
```rust
let logging = Preset::new()
    .before_run(|state| async move { info!("Starting agent"); })
    .after_run(|state, output| async move { info!("Output: {output}"); });

let agent = AgentBuilder::new("writer")
    .instruction("Write an essay.")
    .preset(logging)
    .guard(|state| state.get::<bool>("approved").unwrap_or(false))
    .before_tool(|call, state| async move {
        info!("Tool call: {}", call.name);
    })
    .after_tool(|call, result, state| async move {
        if call.name == "search" {
            state.set("search_done", true);
        }
    });
```

### 3.4 Enhanced Middleware Protocol

**Layer**: `gemini-adk-rs` (L1)

Expand the `Middleware` trait with agent lifecycle and topology hooks:

```rust
#[async_trait]
pub trait Middleware: Send + Sync {
    fn name(&self) -> &str;

    // ---- Agent lifecycle ----
    async fn before_agent(&self, _ctx: &TraceContext, _name: &str) {}
    async fn after_agent(&self, _ctx: &TraceContext, _name: &str, _output: &str) {}

    // ---- Tool lifecycle (existing, expanded) ----
    async fn before_tool(&self, _ctx: &TraceContext, _call: &FunctionCall) -> ToolDirective {
        ToolDirective::Continue
    }
    async fn after_tool(
        &self, _ctx: &TraceContext, _call: &FunctionCall, _result: &Value
    ) {}
    async fn on_tool_error(
        &self, _ctx: &TraceContext, _call: &FunctionCall, _err: &ToolError
    ) {}

    // ---- Dispatch lifecycle (NEW) ----
    async fn on_dispatch(
        &self, _ctx: &TraceContext, _task: &str, _agent: &str
    ) -> DispatchDirective {
        DispatchDirective::Continue
    }
    async fn on_task_complete(&self, _ctx: &TraceContext, _task: &str, _result: &Value) {}
    async fn on_task_error(&self, _ctx: &TraceContext, _task: &str, _err: &AgentError) {}
    async fn on_join(&self, _ctx: &TraceContext, _joined: &[String], _timed_out: &[String]) {}

    // ---- Topology hooks (NEW) ----
    async fn on_loop_iteration(
        &self, _ctx: &TraceContext, _name: &str, _iteration: u32
    ) -> LoopDirective {
        LoopDirective::Continue
    }
    async fn on_fanout_start(&self, _ctx: &TraceContext, _name: &str, _branches: &[String]) {}
    async fn on_fanout_complete(&self, _ctx: &TraceContext, _name: &str) {}
    async fn on_route_selected(&self, _ctx: &TraceContext, _name: &str, _selected: &str) {}
    async fn on_fallback_attempt(
        &self, _ctx: &TraceContext, _name: &str, _agent: &str, _attempt: u32
    ) {}

    // ---- Live session hooks (OUR UNIQUE EXTENSION) ----
    async fn on_session_event(&self, _ctx: &TraceContext, _event: &SessionEvent) {}
    async fn on_extraction(&self, _ctx: &TraceContext, _name: &str, _value: &Value) {}
    async fn on_tool_dispatch_background(
        &self, _ctx: &TraceContext, _call: &FunctionCall
    ) -> DispatchDirective {
        DispatchDirective::Continue
    }
}

/// Control directive returned from before_tool.
pub enum ToolDirective {
    Continue,
    Skip(serde_json::Value),  // skip execution, use this result
    Deny(String),              // deny with reason
}

/// Control directive returned from on_dispatch.
pub enum DispatchDirective {
    Continue,
    Cancel,                          // skip this dispatch
    InjectState(serde_json::Value),  // inject additional state
}

/// Control directive returned from on_loop_iteration.
pub enum LoopDirective {
    Continue,
    Break,   // exit the loop early
}
```

### 3.5 Route — State-Driven Branching

**Layer**: `gemini-adk-fluent-rs` (L2)

Deterministic branching based on state key values:

```rust
pub struct RouteBuilder {
    key: String,
    branches: Vec<(RoutePredicate, Composable)>,
    default: Option<Box<Composable>>,
}

pub enum RoutePredicate {
    Eq(serde_json::Value),
    In(Vec<serde_json::Value>),
    Custom(Arc<dyn Fn(&serde_json::Value) -> bool + Send + Sync>),
}
```

**Fluent API**:
```rust
let workflow = classifier
    >> route("category")
        .eq("technical", tech_agent)
        .eq("billing", billing_agent)
        .eq("sales", sales_agent)
        .default(general_agent)
    >> formatter;

// Or with custom predicates:
let workflow = analyzer
    >> route_fn(|state| {
        let score = state.get::<f64>("confidence").unwrap_or(0.0);
        if score > 0.8 { "high" } else { "low" }
    })
    .when("high", fast_agent)
    .when("low", thorough_agent);
```

### 3.6 Conditional Gate

```rust
let workflow = input_parser
    >> gate(|state| state.get::<bool>("needs_review").unwrap_or(false))
        .then(reviewer)
        .otherwise(auto_approve)
    >> publisher;
```

---

## 4. The Live Bridge — Connecting Events to Pipelines

This is the section that goes beyond adk-fluent into territory no existing framework
covers. The core insight: **live session events should be able to trigger compiled
agent pipelines**, not just isolated closures.

### 4.1 `on_extracted` → Agent Pipeline

Today:
```rust
.on_extracted(|name, value| async move {
    // Can only run inline code here. No access to compiled agents.
    println!("Extracted: {value}");
})
```

Proposed:
```rust
let summarizer = AgentBuilder::new("summarizer")
    .instruction("Summarize the conversation state into 3 bullet points.");

let classifier = AgentBuilder::new("classifier")
    .instruction("Classify the order phase: greeting, ordering, confirming, complete.");

// Option A: Pipeline as extraction handler (non-blocking)
Live::builder()
    .on_extracted_pipeline(
        classifier >> fn_step("store", |state| async move {
            // classifier output is in state, store it for instruction_template
            Ok(())
        }),
        llm.clone(),
    )

// Option B: Dispatch agent on extraction (explicit)
Live::builder()
    .on_extracted_dispatch(
        ("summarize", summarizer),
        ("classify", classifier),
    )
    .on_dispatched_complete(|task_name, result, state, writer| async move {
        if task_name == "classify" {
            // Update instruction based on classification
            let phase = result["phase"].as_str().unwrap_or("unknown");
            state.set("phase", phase);
        }
    })
```

### 4.2 Non-Blocking Tool → Agent Pipeline

When a non-blocking tool completes, run an agent pipeline on its result before
sending to Gemini:

```rust
Live::builder()
    .tool_background_with_pipeline(
        search_flights_tool,
        // Post-process search results through an agent pipeline
        fn_step("parse", |state| async move {
            let raw = state.get::<Value>("tool_result")?;
            let flights = parse_flight_data(&raw);
            state.set("flights", flights);
            Ok(())
        })
        >> AgentBuilder::new("ranker")
            .instruction("Rank flights by value. Consider price, duration, airline quality.")
            .writes("ranked_flights")
        >> fn_step("format", |state| async move {
            let ranked = state.get::<Value>("ranked_flights")?;
            // format_result for Gemini consumption
            state.set("tool_response", ranked);
            Ok(())
        }),
        FlightSearchFormatter,
        llm.clone(),
    )
```

**How this works in the processor:**

```
ToolCall arrives → on_tool_call (Blocking) → no override → tool is Background
  │
  ├→ Immediately send ack: {"status": "Searching flights..."}
  ├→ tokio::spawn:
  │    1. search_flights_tool.call(args).await → raw result
  │    2. state.set("tool_result", raw_result)
  │    3. pipeline.compile(llm).run(&state).await → processed result
  │    4. result = state.get("tool_response")
  │    5. formatted = formatter.format_result(result)
  │    6. writer.send_tool_response(formatted)
  │
  └→ Control lane proceeds immediately (non-blocking)
```

### 4.3 Live Hooks as First-Class Pipeline Triggers

A general mechanism to attach compiled pipelines to any live session event:

```rust
/// A hook that triggers a pipeline when a live event matches.
pub struct LiveHook {
    trigger: LiveTrigger,
    pipeline: Composable,
    mode: CallbackMode,      // Blocking or Concurrent
    state_key: Option<String>, // where to store pipeline output
}

pub enum LiveTrigger {
    OnExtracted(String),       // extractor name
    OnTurnComplete,
    OnConnected,
    OnToolResult(String),      // tool name
    OnPhaseChanged(SessionPhase),
    OnVadEnd,                  // after user stops speaking
    Custom(Arc<dyn Fn(&SessionEvent) -> bool + Send + Sync>),
}
```

**Fluent API**:
```rust
Live::builder()
    .hook(
        LiveTrigger::OnExtracted("OrderState"),
        classifier >> summarizer,
        CallbackMode::Concurrent,
    )
    .hook(
        LiveTrigger::OnTurnComplete,
        context_builder >> fn_step("inject", |state| async move {
            // Build context summary for next turn
            Ok(())
        }),
        CallbackMode::Blocking,  // must complete before next turn
    )
    .hook(
        LiveTrigger::OnToolResult("search_flights"),
        ranker >> formatter_agent,
        CallbackMode::Concurrent,
    )
```

### 4.4 Tools with State Access

Today, tools are isolated — they receive `args: Value` and return `Value`. They
have no access to the session's `State`. This is a fundamental limitation:

```rust
// TODAY: tools are state-blind
let tool = SimpleTool::new("get_order", "Get order by ID", None,
    |args| async move {
        let id = args["id"].as_str()?;
        // Can't read state.get::<Vec<Order>>("orders") — no state access!
        Ok(json!({"error": "No state access"}))
    }
);
```

**Proposed**: `StatefulTool` — tool with state access:

```rust
// gemini-adk-rs (L1)
#[async_trait]
pub trait StatefulToolFunction: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn call(
        &self,
        args: serde_json::Value,
        state: &State,
    ) -> Result<serde_json::Value, ToolError>;
}

// gemini-adk-fluent-rs (L2) — ergonomic registration
impl Live {
    fn tool_with_state<F, Fut>(
        self,
        name: &str,
        description: &str,
        handler: F,
    ) -> Self
    where
        F: Fn(Value, State) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, ToolError>> + Send + 'static,
}
```

**Usage**:
```rust
Live::builder()
    .tool_with_state("get_order_status", "Get current order status",
        |args, state| async move {
            let order: OrderState = state.get("OrderState")
                .ok_or(ToolError::Other("No order state".into()))?;
            Ok(json!({
                "items": order.items,
                "total": order.total,
                "phase": order.phase,
            }))
        }
    )
```

### 4.5 Agent-as-Tool

Wrap an `AgentBuilder` as a tool — the agent runs when the tool is called:

```rust
// gemini-adk-fluent-rs (L2)
impl Live {
    fn agent_tool(
        self,
        agent: AgentBuilder,
        llm: Arc<dyn BaseLlm>,
    ) -> Self
}
```

**Usage**:
```rust
let deep_analyzer = AgentBuilder::new("deep_analyzer")
    .instruction("Perform deep analysis of the query. Return structured JSON.")
    .model(GeminiModel::Gemini2_5Pro)
    .temperature(0.2);

Live::builder()
    .agent_tool(deep_analyzer, llm.clone())  // exposed as tool "deep_analyzer"
    // When model calls deep_analyzer(query: "..."), the agent runs
    // Agent output becomes the tool response
```

**For non-blocking tools:**
```rust
Live::builder()
    .agent_tool_background(deep_analyzer, llm.clone())
    // Model gets immediate ack, agent runs in background
    // Agent output is injected as tool response when done
```

---

## 5. Composition Module Gaps

### 5.1 S (State) — Bridge to Live Session State

**Gap**: `StateTransform` operates on `serde_json::Value`, not `gemini_adk_rs::State`.

**Fix**: Add `State`-native transforms:

```rust
impl StateTransformChain {
    /// Apply this transform chain to a live session State.
    pub fn apply_to_state(&self, state: &State) {
        let mut value = state.to_json();
        self.apply(&mut value);
        state.merge_from_json(&value);
    }
}
```

And bridge in the fluent API:

```rust
Live::builder()
    .on_turn_complete_transform(
        S::pick(&["OrderState", "conversation_summary"])
        >> S::rename(&[("OrderState", "order")])
        >> S::flatten("order")
    )
    // Applied to State automatically at each turn boundary
```

### 5.2 P (Prompt) — Auto-Injection into Agents

**Gap**: Prompt composition creates formatted text but doesn't wire into agents.

**Fix**: Add `.prompt()` to `AgentBuilder`:

```rust
impl AgentBuilder {
    fn prompt(self, composite: PromptComposite) -> Self {
        self.instruction(composite.render())
    }
}
```

**Usage**:
```rust
let agent = AgentBuilder::new("writer")
    .prompt(
        P::role("technical writer")
        + P::task("Write a comprehensive analysis")
        + P::constraint("Under 500 words")
        + P::format("Markdown with headers")
    );
```

### 5.3 M (Middleware) — Scoping and Conditional Application

**Gap**: Middleware applies to everything. No scoping or conditional application.

**Fix**: Add `M::scope()` and `M::when()`:

```rust
impl M {
    /// Only apply middleware to agents matching the predicate.
    fn scope(
        predicate: impl Fn(&str) -> bool + Send + Sync + 'static,
        middleware: impl Middleware,
    ) -> ScopedMiddleware

    /// Only apply middleware when the condition is true.
    fn when(
        condition: impl Fn() -> bool + Send + Sync + 'static,
        middleware: impl Middleware,
    ) -> ConditionalMiddleware
}
```

**Usage**:
```rust
let middleware = M::log()
    | M::scope(|name| name.starts_with("write"), M::cost())
    | M::when(|| cfg!(debug_assertions), M::audit());
```

### 5.4 T (Tools) — State-Aware and Agent-Wrapped Tools

**Gap**: Tools don't have state access. No way to wrap an agent as a tool.

**Fix**: Extend the `T` module:

```rust
impl T {
    /// Tool with access to session State.
    fn stateful(
        name: &str,
        description: &str,
        handler: impl Fn(Value, State) -> BoxFuture<Result<Value, ToolError>>,
    ) -> StatefulToolEntry

    /// Wrap an agent as a tool.
    fn agent(agent: AgentBuilder, llm: Arc<dyn BaseLlm>) -> AgentToolEntry

    /// Wrap an agent as a non-blocking background tool.
    fn agent_background(
        agent: AgentBuilder,
        llm: Arc<dyn BaseLlm>,
    ) -> BackgroundAgentToolEntry
}
```

**Usage**:
```rust
let tools = T::google_search()
    | T::stateful("check_order", "Check order status", |args, state| async move {
        let order: OrderState = state.get("OrderState").unwrap();
        Ok(json!({"status": order.phase}))
    })
    | T::agent_background(deep_analyzer, llm.clone());
```

---

## 6. The Verb Table — What Developers Can Express

After these changes, the full set of verbs available in the fluent layer:

### Agent Construction Verbs

| Verb | Description | Example |
|------|-------------|---------|
| `.instruction()` | Set system instruction | `.instruction("You are a writer.")` |
| `.prompt()` | Set composed prompt | `.prompt(P::role("writer") + P::task("..."))` |
| `.model()` | Set LLM model | `.model(GeminiModel::Gemini2_5Flash)` |
| `.temperature()` | Set temperature | `.temperature(0.7)` |
| `.tools()` | Register tools | `.tools(dispatcher)` |
| `.writes()` / `.reads()` | State flow declarations | `.writes("output").reads("input")` |
| `.sub_agent()` | Add transfer target | `.sub_agent(specialist)` |
| `.guard()` | Conditional execution | `.guard(\|s\| s.get::<bool>("ready") == Some(true))` |
| `.stay()` / `.isolate()` | Transfer control | `.isolate()` |
| `.before_run()` | Pre-execution hook | `.before_run(\|s\| async { log(s); })` |
| `.after_run()` | Post-execution hook | `.after_run(\|s, out\| async { validate(out); })` |
| `.before_tool()` | Pre-tool hook | `.before_tool(\|call, s\| async { ... })` |
| `.after_tool()` | Post-tool hook | `.after_tool(\|call, result, s\| async { ... })` |
| `.on_error()` | Error recovery | `.on_error(\|err, s\| async { fallback(s) })` |
| `.preset()` | Apply callback bundle | `.preset(logging_preset)` |

### Pipeline Composition Verbs

| Verb | Operator | Description | Example |
|------|----------|-------------|---------|
| Sequence | `>>` | Run in order | `a >> b >> c` |
| Parallel | `\|` | Run concurrently | `a \| b \| c` |
| Loop | `*` | Repeat N times | `a * 3` |
| Until | `* until()` | Loop until predicate | `a * until(\|s\| s["done"] == true)` |
| Fallback | `/` | Try until success | `a / b / c` |
| Route | `route()` | State-driven branch | `route("type").eq("a", agent_a).default(agent_b)` |
| Gate | `gate()` | Conditional pass | `gate(pred).then(a).otherwise(b)` |
| Dispatch | `dispatch()` | Fire background | `dispatch(("name", agent))` |
| Join | `join()` | Collect background | `join("name").timeout(30)` |
| Step | `fn_step()` | Inline code | `fn_step("merge", \|s\| async { ... })` |

### Live Session Verbs

| Verb | Description | Example |
|------|-------------|---------|
| `.on_audio()` | Audio callback (Concurrent) | `.on_audio(\|d\| play(d))` |
| `.on_audio_blocking()` | Audio callback (Blocking) | `.on_audio_blocking(\|d\| async { write(d).await })` |
| `.on_text()` | Text delta callback | `.on_text(\|t\| print(t))` |
| `.on_turn_complete()` | Turn done (Blocking) | `.on_turn_complete(\|\| async { ... })` |
| `.on_turn_complete_concurrent()` | Turn done (fire-and-forget) | `.on_turn_complete_concurrent(\|\| async { metrics() })` |
| `.on_extracted()` | Extraction result (Concurrent) | `.on_extracted(\|n, v\| async { ... })` |
| `.on_extracted_blocking()` | Extraction result (Blocking) | `.on_extracted_blocking(\|n, v\| async { ... })` |
| `.on_connected()` | Session init (Blocking) | `.on_connected(\|\| async { load_profile() })` |
| `.extract_turns::<T>()` | Schema-guided extraction | `.extract_turns::<OrderState>(llm, "Extract order")` |
| `.instruction_template()` | State-reactive instruction | `.instruction_template(\|s\| Some("...".into()))` |
| `.on_turn_boundary()` | Context injection | `.on_turn_boundary(\|s, w\| async { ... })` |
| `.before_tool_response()` | Transform tool results | `.before_tool_response(\|r, s\| async { r })` |
| `.tool_behavior()` | Non-blocking tool mode | `.tool_behavior(NonBlocking)` |
| `.tool_background()` | Background tool | `.tool_background(search_tool)` |
| `.tool_with_state()` | State-aware tool | `.tool_with_state("name", "desc", handler)` |
| `.agent_tool()` | Agent as tool | `.agent_tool(analyzer, llm)` |
| `.agent_tool_background()` | Agent as background tool | `.agent_tool_background(analyzer, llm)` |
| `.hook()` | Event → pipeline trigger | `.hook(OnExtracted("X"), pipeline, Concurrent)` |
| `.on_extracted_pipeline()` | Pipeline on extraction | `.on_extracted_pipeline(pipeline, llm)` |
| `.on_extracted_dispatch()` | Dispatch on extraction | `.on_extracted_dispatch(("name", agent))` |
| `.middleware()` | Live session middleware | `.middleware(M::log() \| M::cost())` |

---

## 7. Implementation Priority

### Phase 1: Foundation (Enables Everything Else)

| Item | Layer | Description |
|------|-------|-------------|
| `FnStep` as `Composable` variant | L2 | Inline code in pipelines |
| `Route` and `Gate` operators | L2 | State-driven branching |
| Enhanced `Middleware` trait | L1 | Agent lifecycle + topology hooks + directives |
| `TraceContext` | L1 | Inter-hook state bag for middleware |
| `StatefulToolFunction` trait | L1 | Tools with State access |

### Phase 2: Background Execution

| Item | Layer | Description |
|------|-------|-------------|
| `TaskRegistry` | L1 | Dispatch/join coordination runtime |
| `dispatch()` / `join()` primitives | L2 | Non-blocking agent execution |
| `DispatchDirective` / `LoopDirective` | L1 | Control returns from middleware |

### Phase 3: Live Bridge

| Item | Layer | Description |
|------|-------|-------------|
| `LiveHook` / `LiveTrigger` | L2 | Event → pipeline trigger mechanism |
| `on_extracted_pipeline()` | L2 | Pipeline as extraction handler |
| `agent_tool()` / `agent_tool_background()` | L2 | Agent-as-tool for live sessions |
| `tool_background_with_pipeline()` | L2 | Agent post-processing for non-blocking tools |

### Phase 4: DevEx Polish

| Item | Layer | Description |
|------|-------|-------------|
| Per-agent callback stacks | L2 | before_run/after_run/before_tool/after_tool |
| `Preset` bundles | L2 | Reusable callback collections |
| `M::scope()` / `M::when()` | L2 | Conditional middleware application |
| `P::` auto-injection via `.prompt()` | L2 | Prompt composition → instruction |
| `S::` bridge to live `State` | L2 | State transforms on live session state |
| `T::stateful()` / `T::agent()` | L2 | State-aware and agent-wrapped tools |

---

## 8. Full Example: What It Looks Like When It All Works

A restaurant order agent with:
- Non-blocking menu search (background tool with agent post-processing)
- State-reactive instructions that change per order phase
- Background summarization agent triggered on extraction
- Human-in-the-loop confirmation for placing the final order
- Telemetry middleware scoped to specific agents

```rust
use gemini_adk_fluent_rs::prelude::*;

// ---- Define agents ----

let ranker = AgentBuilder::new("ranker")
    .prompt(
        P::role("menu expert")
        + P::task("Rank menu items by relevance to customer preferences")
        + P::format("JSON array sorted by score")
    )
    .model(GeminiModel::Gemini2_5Flash)
    .temperature(0.3)
    .writes("ranked_items");

let summarizer = AgentBuilder::new("summarizer")
    .instruction("Summarize the current order and conversation tone in 2 sentences.")
    .writes("conversation_summary");

// ---- Define middleware ----

let telemetry = M::log()
    | M::latency()
    | M::scope(|name| name == "ranker", M::cost());

// ---- Define tools ----

let mut dispatcher = ToolDispatcher::new();
dispatcher.register_function(Arc::new(search_menu_tool));
dispatcher.register_function(Arc::new(place_order_tool));

// ---- Build live session ----

let handle = Live::builder()
    .model(GeminiModel::GeminiLive2_5FlashNativeAudio)
    .voice(Voice::Kore)
    .instruction("You are a friendly restaurant order assistant.")
    .tools(dispatcher)
    .tool_behavior(FunctionCallingBehavior::NonBlocking)

    // Menu search: non-blocking, post-processed by ranker agent
    .tool_background_with_pipeline(
        "search_menu",
        fn_step("prep", |state| async move {
            let raw = state.get::<Value>("tool_result").unwrap();
            state.set("menu_items", raw["results"].clone());
            Ok(())
        }) >> ranker,
        MenuSearchFormatter,
        llm.clone(),
    )

    // State-aware tool: order status from extracted state
    .tool_with_state("check_order", "Get current order status",
        |_args, state| async move {
            let order: OrderState = state.get("OrderState")
                .ok_or(ToolError::Other("No order yet".into()))?;
            Ok(serde_json::to_value(&order)?)
        }
    )

    // Extraction pipeline
    .extract_turns::<OrderState>(llm.clone(), "Extract order items, quantities, phase")

    // On extraction → dispatch background summarizer
    .on_extracted_dispatch(("summarize", summarizer))

    // Human approval for placing order
    .on_tool_call(|calls| async move {
        for call in &calls {
            if call.name == "place_order" {
                let approved = show_confirmation_dialog(&call).await;
                if !approved {
                    return Some(vec![FunctionResponse {
                        name: call.name.clone(),
                        response: json!({"error": "Customer cancelled"}),
                        id: call.id.clone(),
                    }]);
                }
            }
        }
        None
    })

    // State-reactive instructions
    .instruction_template(|state| {
        let phase: String = state.get("OrderState.phase").unwrap_or_default();
        let summary: String = state.get("conversation_summary").unwrap_or_default();
        match phase.as_str() {
            "greeting" => Some(format!(
                "Welcome the customer. Be warm and friendly.\n\nContext: {summary}"
            )),
            "ordering" => Some(format!(
                "Help with ordering. Suggest popular items. Upsell drinks.\n\nContext: {summary}"
            )),
            "confirming" => Some(format!(
                "Read back the complete order. Confirm total. Ask for final approval.\n\nContext: {summary}"
            )),
            _ => None,
        }
    })

    // Audio and telemetry
    .on_audio(|data| speaker.write(data))
    .on_text(|t| ui.append_text(t))
    .on_error_concurrent(|err| async move {
        sentry::capture_message(&err, sentry::Level::Warning);
    })

    // Apply middleware
    .middleware(telemetry)

    .connect_vertex("vital-octagon-19612", "us-central1", token)
    .await?;

// Session is live. User speaks, model responds, tools fire,
// agents run in background, state updates reactively.
handle.done().await?;
```

**What happens when the user says "Show me pasta dishes":**

```
t=0s   User speaks → audio flows to Gemini
t=1s   Model: ToolCall search_menu({query: "pasta"})
       → Processor: tool is Background mode
       → Immediately: ack {"status": "Searching menu for pasta..."}
       → Model keeps talking: "Let me find our pasta options for you!"
       → Background:
         1. search_menu_tool.call({query: "pasta"}) → raw menu data (1.5s)
         2. fn_step("prep") → extracts results into state
         3. ranker agent → LLM call to rank by relevance (1s)
         4. MenuSearchFormatter.format_result() → formatted for model
         5. writer.send_tool_response() → model gets ranked results

t=3s   Model: "We have 4 great pasta dishes! The Truffle Carbonara is our
        most popular, followed by..."

t=4s   TurnComplete → Extractor runs → OrderState extracted
       → on_extracted_dispatch → summarizer fires in background (2s LLM call)
       → instruction_template reads phase="ordering" → instruction updated
       → Model's next turn uses "Help with ordering. Suggest popular items..."

t=6s   Summarizer finishes → state.set("conversation_summary", "Customer is
        interested in pasta. Friendly tone. Exploring options.")
       → Next instruction_template call includes this context
```

No spaghetti code. Every concern is expressed with a single verb. Agent pipelines,
non-blocking tools, live callbacks, middleware, and state management all compose
through a unified fluent API.
