# gemini-live-rs — Rust ADK for Gemini Live

**A Layered Rust Agent Development Kit for the Gemini Multimodal Live API**

Version 0.2 · March 2026

---

## 1. Vision

Build a Rust-native ADK that talks directly to Gemini Live over raw WebSocket — no SDK wrapper, no Python GIL, no cascaded pipeline. The wire protocol is a first-class citizen, with high-agency mechanisms layered on top for building autonomous agents.

The library serves three audiences simultaneously:

1. **Wire-level users** — want raw WebSocket access to Gemini Live, zero abstraction
2. **App developers** — want an agent runtime with tool dispatch, streaming tools, agent transfer
3. **Orchestration engineers** — want adk-fluent-style algebraic composition for multi-agent pipelines

Additionally, the entire stack is exposed as a **native Python module via PyO3**, giving Python developers Rust performance with a familiar API.

### Why This Exists

The Gemini Multimodal Live API is fundamentally different from the STT→LLM→TTS pipeline model. It offers native bidirectional audio streaming, server-side VAD, integrated function calling, and speech-to-speech with emotional understanding — all over a single WebSocket.

Google's ADK Python provides the right abstractions (LiveRequestQueue, dual-task architecture, streaming tools) but is limited by the Python GIL. adk-fluent provides excellent DX but inherits the same performance ceiling.

This library combines ADK's runtime architecture with adk-fluent's composition ergonomics, implemented in Rust for true parallelism and predictable latency.

---

## 2. Architecture: Three Layered Crates + Python Bindings

```
gemini-live-rs/
├── crates/
│   ├── gemini-live-wire/         # Layer 0: Raw protocol + transport
│   ├── gemini-live-runtime/      # Layer 1: Agent runtime
│   ├── gemini-live/              # Layer 2: Fluent DX
│   └── gemini-live-python/       # PyO3 bindings (all layers)
├── examples/
└── docs/
```

Each layer is independently usable. Advanced users mix layers freely.

| Layer | Crate | Purpose | Depends On |
|-------|-------|---------|------------|
| 0 | `gemini-live-wire` | Wire protocol types, WebSocket transport, buffers, telemetry | Nothing |
| 1 | `gemini-live-runtime` | Agent trait, LiveRequestQueue, tool dispatch, agent transfer, middleware | Layer 0 |
| 2 | `gemini-live` | Fluent builders, operator algebra, composition modules, patterns, testing | Layer 1 |
| Bindings | `gemini-live-python` | PyO3 native Python module exposing all layers | Layer 2 |

### Origin: JS SDK Audit + ADK Python Analysis

This design is informed by:

1. **JS GenAI SDK audit** (`googleapis/js-genai`) — verified wire protocol correctness, identified missing built-in tool types (`urlContext`, `googleSearch`, `codeExecution`), missing `thinkingConfig`, `enableAffectiveDialog`
2. **ADK Python source** (`google/adk-python`) — extracted the `LiveRequestQueue` + dual-task architecture, streaming tool lifecycle, agent transfer pattern, input-stream duplication
3. **adk-fluent** (`vamsiramakrishnan/adk-fluent`) — extracted the algebraic composition language, five single-letter modules (S, C, P, M, T), copy-on-write builders, IR compilation, testing utilities

---

## 3. Layer 0: Wire Protocol (`gemini-live-wire`)

### 3.1 Structure

```
crates/gemini-live-wire/src/
├── lib.rs
├── protocol/
│   ├── mod.rs
│   ├── messages.rs       # Client→Server and Server→Client message envelopes
│   └── types.rs          # SessionConfig, Voice, AudioFormat, GeminiModel, Tool
├── transport/
│   ├── mod.rs            # TransportConfig
│   ├── connection.rs     # WebSocket lifecycle, setup handshake, reconnection
│   └── flow.rs           # Token bucket rate limiter
├── buffer/
│   ├── mod.rs            # SPSC ring buffer
│   └── jitter.rs         # Adaptive jitter buffer
├── vad/
│   └── mod.rs            # Energy+ZCR voice activity detection
└── telemetry/
    ├── mod.rs
    ├── spans.rs
    ├── metrics.rs
    └── logging.rs
```

### 3.2 Protocol Fixes (from JS SDK audit)

**Critical: Unified `Tool` type** replacing the current `ToolDeclaration` which only supports function declarations.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_declarations: Option<Vec<FunctionDeclaration>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_context: Option<UrlContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_search: Option<GoogleSearch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution: Option<CodeExecution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_search_retrieval: Option<GoogleSearchRetrieval>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UrlContext {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleSearch {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeExecution {}
```

**Missing GenerationConfig fields:**

```rust
pub struct GenerationConfig {
    // ... existing fields ...
    pub thinking_config: Option<ThinkingConfig>,
    pub enable_affective_dialog: Option<bool>,
    pub media_resolution: Option<MediaResolution>,
    pub seed: Option<u32>,
}

pub struct ThinkingConfig {
    pub thinking_budget: Option<u32>,
}

pub enum MediaResolution {
    Low,
    Medium,
    High,
}
```

**SetupPayload `tools` field type change:**

```rust
// Before (broken — can't express urlContext, googleSearch, etc.):
pub tools: Vec<ToolDeclaration>,

// After:
pub tools: Vec<Tool>,
```

### 3.3 Existing Correct Implementation

The following are verified correct against the JS SDK:

- Setup message structure: `{ "setup": { "model": "...", "generationConfig": {...}, ... } }`
- camelCase serialization via `#[serde(rename_all = "camelCase")]`
- ClientContent format: `{ "clientContent": { "turns": [...], "turnComplete": true } }`
- ToolResponse format: `{ "toolResponse": { "functionResponses": [...] } }`
- RealtimeInput format: `{ "realtimeInput": { "audio": {...} } }`
- O(1) server message parsing via string-contains routing (smarter than JS SDK)
- Part polymorphism via `#[serde(untagged)]`
- Google AI and Vertex AI URL construction
- Session resumption, GoAway handling, activity signals

---

## 4. Layer 1: Agent Runtime (`gemini-live-runtime`)

### 4.1 Structure

```
crates/gemini-live-runtime/src/
├── lib.rs
├── agent.rs          # Agent trait + LlmAgent + AgentEvent
├── tool.rs           # ToolFunction, StreamingTool, InputStreamingTool, ToolDispatcher
├── live_queue.rs     # LiveRequestQueue (LiveSender + LiveReceiver)
├── router.rs         # AgentRegistry + agent transfer
├── middleware.rs     # Middleware trait + MiddlewareChain + built-ins
├── context.rs        # InvocationContext
├── state.rs          # State container (typed key-value)
└── flow/
    ├── mod.rs
    ├── barge_in.rs
    └── turn_detection.rs
```

### 4.2 Core Trait: `Agent`

```rust
#[async_trait]
pub trait Agent: Send + Sync + 'static {
    fn name(&self) -> &str;
    async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError>;
    fn tools(&self) -> Vec<Tool> { vec![] }
    fn sub_agents(&self) -> Vec<Arc<dyn Agent>> { vec![] }
}
```

### 4.3 LiveRequestQueue

The Rust equivalent of ADK's `LiveRequestQueue`. Uses Tokio MPSC channels instead of asyncio.Queue:

```rust
pub enum LiveRequest {
    Content(Content),
    Realtime(Blob),
    ActivityStart,
    ActivityEnd,
    Close,
}

#[derive(Clone)]
pub struct LiveSender {
    tx: mpsc::Sender<LiveRequest>,
}

impl LiveSender {
    pub async fn send_content(&self, content: Content) -> Result<(), AgentError>;
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), AgentError>;
    pub async fn send_realtime(&self, blob: Blob) -> Result<(), AgentError>;
    pub fn send_activity_start(&self) -> Result<(), AgentError>;
    pub fn send_activity_end(&self) -> Result<(), AgentError>;
    pub fn close(&self);
}

pub struct LiveReceiver {
    rx: mpsc::Receiver<LiveRequest>,
}

pub fn live_queue(capacity: usize) -> (LiveSender, LiveReceiver);
```

### 4.4 Three Tool Types

Mirroring ADK's tool architecture:

**Regular tools** — called, return result, done:
```rust
#[async_trait]
pub trait ToolFunction: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError>;
}
```

**Streaming tools** — run in background, yield multiple results:
```rust
#[async_trait]
pub trait StreamingTool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn run(
        &self,
        args: serde_json::Value,
        yield_tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<(), ToolError>;
}
```

**Input-streaming tools** — receive duplicated live input while running:
```rust
#[async_trait]
pub trait InputStreamingTool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn run(
        &self,
        args: serde_json::Value,
        input_rx: broadcast::Receiver<LiveRequest>,
        yield_tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<(), ToolError>;
}
```

**ToolDispatcher** unifies all three:
```rust
pub enum ToolKind {
    Function(Arc<dyn ToolFunction>),
    Streaming(Arc<dyn StreamingTool>),
    InputStream(Arc<dyn InputStreamingTool>),
}

pub struct ActiveStreamingTool {
    pub task: JoinHandle<()>,
    pub input_tx: Option<broadcast::Sender<LiveRequest>>,
    pub cancel: CancellationToken,
}

pub struct ToolDispatcher {
    tools: HashMap<String, ToolKind>,
    active: Arc<Mutex<HashMap<String, ActiveStreamingTool>>>,
}
```

### 4.5 Dual-Task Architecture

The core runtime loop — equivalent of ADK's `run_live()`:

- **Task 1 (`_send_to_model`)**: Reads from `LiveRequestQueue`, fans out to input-streaming tools, sends to WebSocket
- **Task 2 (`_receive_from_model`)**: Reads WebSocket events, dispatches tool calls (regular → immediate response, streaming → spawn background task + pending response), emits `AgentEvent`s

Both run concurrently via `tokio::select!`. On agent transfer, Task 1 is cancelled, connection is closed, and the target agent's `run_live()` is called.

### 4.6 Agent Transfer

```rust
pub struct AgentRegistry {
    agents: HashMap<String, Arc<dyn Agent>>,
}

impl AgentRegistry {
    pub fn register(&mut self, agent: Arc<dyn Agent>);
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Agent>>;
}
```

Transfer flow:
1. Model calls `transfer_to_agent` function
2. Configurable delay (default 1s, matching ADK)
3. Close current WebSocket connection
4. Resolve target agent from registry
5. Reconnect with target agent's config (reuses session resumption handle)
6. Run target agent's `run_live()` on the new session

### 4.7 InvocationContext

The session state container that flows through the entire agent execution:

```rust
pub struct InvocationContext {
    pub live_sender: LiveSender,
    pub session: SessionHandle,
    pub event_tx: broadcast::Sender<AgentEvent>,
    pub state: State,
    pub active_streaming_tools: Arc<Mutex<HashMap<String, ActiveStreamingTool>>>,
    pub resumption_handle: Arc<Mutex<Option<String>>>,
    pub middleware: MiddlewareChain,
    pub agent_registry: Arc<AgentRegistry>,
    pub telemetry: TelemetryContext,
}
```

### 4.8 Middleware

```rust
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    fn name(&self) -> &str;

    // Agent lifecycle
    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> { Ok(()) }
    async fn after_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> { Ok(()) }

    // Tool lifecycle
    async fn before_tool(&self, _call: &FunctionCall) -> Result<(), AgentError> { Ok(()) }
    async fn after_tool(&self, _call: &FunctionCall, _result: &serde_json::Value) -> Result<(), AgentError> { Ok(()) }
    async fn on_tool_error(&self, _call: &FunctionCall, _err: &ToolError) -> Result<(), AgentError> { Ok(()) }

    // Model lifecycle
    async fn on_event(&self, _event: &AgentEvent) -> Result<(), AgentError> { Ok(()) }

    // Streaming lifecycle
    async fn on_stream_item(&self, _item: &LiveRequest) -> Result<(), AgentError> { Ok(()) }

    // Error
    async fn on_error(&self, _err: &AgentError) -> Result<(), AgentError> { Ok(()) }
}

pub struct MiddlewareChain {
    layers: Vec<Arc<dyn Middleware>>,
}
```

Built-in middleware: `RetryMiddleware`, `LogMiddleware`, `CostTracker`, `LatencyTracker`, `TimeoutMiddleware`, `RateLimitMiddleware`.

### 4.9 AgentEvent

```rust
pub enum AgentEvent {
    Started { agent_name: String },
    TextDelta(String),
    TextComplete(String),
    AudioData(Vec<u8>),
    InputTranscription(String),
    OutputTranscription(String),
    ToolCallStarted { name: String, args: serde_json::Value },
    ToolCallCompleted { name: String, result: serde_json::Value, duration: Duration },
    ToolCallFailed { name: String, error: String },
    StreamingToolYield { name: String, value: serde_json::Value },
    TurnComplete,
    Interrupted,
    AgentTransfer { from: String, to: String },
    StateChanged { key: String },
    Disconnected(Option<String>),
    Error(String),
}
```

### 4.10 State Container

```rust
pub struct State {
    inner: Arc<DashMap<String, StateValue>>,
}

enum StateValue {
    String(String),
    Json(serde_json::Value),
    Bytes(Vec<u8>),
    Any(Box<dyn Any + Send + Sync>),
}

impl State {
    pub fn get<T: FromStateValue>(&self, key: &str) -> Option<T>;
    pub fn set(&self, key: impl Into<String>, value: impl IntoStateValue);
    pub fn pick(&self, keys: &[&str]) -> State;
    pub fn merge(&self, other: &State);
    pub fn rename(&self, from: &str, to: &str);
}
```

### 4.11 `#[tool]` Proc Macro

```rust
#[tool(description = "Get current weather for a city")]
async fn get_weather(city: String, units: Option<String>) -> Result<serde_json::Value, ToolError> {
    Ok(json!({ "temp": 22, "condition": "sunny", "city": city }))
}
// Expands to: struct GetWeatherTool implementing ToolFunction with auto-generated JSON Schema
```

---

## 5. Layer 2: Fluent DX (`gemini-live`)

### 5.1 Structure

```
crates/gemini-live/src/
├── lib.rs            # Prelude re-exports
├── builder.rs        # AgentBuilder, Pipeline, FanOut, Loop, Fallback, Route
├── operators.rs      # Shr(>>), BitOr(|), Mul(*), Div(//) trait impls
├── compose/
│   ├── state.rs      # S module
│   ├── context.rs    # C module
│   ├── prompt.rs     # P module
│   ├── middleware.rs  # M module
│   └── tools.rs      # T module
├── patterns.rs       # review_loop, cascade, fan_out_merge, supervised, map_over
└── testing.rs        # MockBackend, AgentHarness, check_contracts
```

### 5.2 AgentBuilder

Copy-on-write immutable builder. Every mutation returns a new builder (original unchanged), so builders are safely shareable as templates.

```rust
#[derive(Clone)]
pub struct AgentBuilder {
    inner: Arc<AgentBuilderInner>,
}

impl AgentBuilder {
    pub fn new(name: impl Into<String>) -> Self;

    // Fluent setters
    pub fn model(self, model: GeminiModel) -> Self;
    pub fn instruction(self, inst: impl Into<String>) -> Self;
    pub fn voice(self, voice: Voice) -> Self;
    pub fn temperature(self, t: f32) -> Self;
    pub fn text_only(self) -> Self;
    pub fn thinking(self, budget: u32) -> Self;

    // Tools
    pub fn tool(self, t: impl Into<ToolKind>) -> Self;
    pub fn tools(self, t: impl IntoTools) -> Self;
    pub fn url_context(self) -> Self;
    pub fn google_search(self) -> Self;
    pub fn code_execution(self) -> Self;

    // State data flow
    pub fn writes(self, key: impl Into<String>) -> Self;
    pub fn reads(self, key: impl Into<String>) -> Self;

    // Agent hierarchy
    pub fn sub_agent(self, agent: AgentBuilder) -> Self;
    pub fn isolate(self) -> Self;
    pub fn stay(self) -> Self;

    // Middleware
    pub fn middleware(self, m: impl Into<MiddlewareComposite>) -> Self;

    // Execution shortcuts
    pub fn build(self) -> LlmAgent;
    pub async fn ask(self, api_key: &str, prompt: &str) -> Result<String, AgentError>;
    pub fn stream(self, api_key: &str, prompt: &str) -> impl Stream<Item = Result<String, AgentError>>;
    pub async fn session(self, api_key: &str) -> Result<ChatSession, AgentError>;
    pub async fn map(self, api_key: &str, inputs: Vec<String>, concurrency: usize) -> Vec<Result<String, AgentError>>;
}
```

### 5.3 Operator Algebra

| Operator | Rust Trait | Meaning | Example |
|----------|-----------|---------|---------|
| `>>` | `Shr` | Sequential pipeline | `agent_a >> agent_b` |
| `\|` | `BitOr` | Parallel fan-out | `agent_a \| agent_b` |
| `*` | `Mul<u32>` | Fixed loop | `agent * 3` |
| `*` | `Mul<Until>` | Conditional loop | `agent * until(pred)` |
| `//` | `Div` | Fallback chain | `agent_a // agent_b` |

All types that implement `Composable` participate in the algebra: `AgentBuilder`, `Pipeline`, `FanOut`, `Loop`, `Fallback`, `Route`, `StateTransform`.

### 5.4 Five Composition Modules

#### S — State Transforms

```rust
pub struct S;
impl S {
    pub fn pick(keys: &[&str]) -> StateTransform;
    pub fn rename(mappings: &[(&str, &str)]) -> StateTransform;
    pub fn merge(keys: &[&str], into: &str) -> StateTransform;
    pub fn defaults(defaults: serde_json::Value) -> StateTransform;
    pub fn map(f: impl Fn(&mut State) + Send + Sync + 'static) -> StateTransform;
    pub fn drop(keys: &[&str]) -> StateTransform;
}
// Compose with >>: S::pick("a") >> S::rename(&[("a", "b")])
```

#### C — Context Engineering

```rust
pub struct C;
impl C {
    pub fn window(n: usize) -> ContextPolicy;
    pub fn user_only() -> ContextPolicy;
    pub fn from_state(keys: &[&str]) -> ContextPolicy;
    pub fn custom(f: impl Fn(&State, &[Content]) -> Vec<Content> + Send + Sync + 'static) -> ContextPolicy;
}
// Compose with +: C::window(5) + C::from_state(&["topic"])
```

#### P — Prompt Composition

```rust
pub struct P;
impl P {
    pub fn role(role: &str) -> PromptSection;
    pub fn task(task: &str) -> PromptSection;
    pub fn constraint(c: &str) -> PromptSection;
    pub fn format(f: &str) -> PromptSection;
    pub fn example(input: &str, output: &str) -> PromptSection;
    pub fn text(t: &str) -> PromptSection;
}
// Compose with +: P::role("analyst") + P::task("analyze data") + P::format("JSON")
```

#### M — Middleware

```rust
pub struct M;
impl M {
    pub fn retry(max: u32) -> MiddlewareComposite;
    pub fn log(level: tracing::Level) -> MiddlewareComposite;
    pub fn cost() -> MiddlewareComposite;
    pub fn latency() -> MiddlewareComposite;
    pub fn timeout(duration: Duration) -> MiddlewareComposite;
    pub fn rate_limit(rps: u32) -> MiddlewareComposite;
}
// Compose with |: M::retry(3) | M::log(INFO) | M::cost()
```

#### T — Tool Composition

```rust
pub struct T;
impl T {
    pub fn function<F, Fut>(f: F) -> ToolComposite;
    pub fn agent(builder: AgentBuilder) -> ToolComposite;
    pub fn google_search() -> ToolComposite;
    pub fn url_context() -> ToolComposite;
    pub fn code_execution() -> ToolComposite;
}
// Compose with |: T::function(search) | T::function(calc) | T::google_search()
```

### 5.5 Workflow Types

- **Pipeline** — sequential execution via `>>` or `.step()`
- **FanOut** — parallel execution via `|` or `.branch()`, with configurable merge strategy
- **Loop** — repeated execution via `* n` or `* until(pred)`, with max iterations
- **Fallback** — try-each-until-success via `//`
- **Route** — deterministic branching on state values via `Route::on("key").eq("val", agent)`

### 5.6 Pre-Built Patterns

```rust
pub fn review_loop(worker, reviewer, quality_key, target, max_rounds) -> Pipeline;
pub fn cascade(agents: Vec<AgentBuilder>) -> Fallback;
pub fn fan_out_merge(agents: Vec<AgentBuilder>, merge_key: &str) -> Pipeline;
pub fn supervised(worker, supervisor, approval_key, max_revisions) -> Pipeline;
pub fn map_over(agent, items_key, concurrency) -> MapOver;
pub fn dispatch_join(foreground, background, timeout) -> Pipeline;
```

### 5.7 Testing

```rust
pub struct MockBackend {
    responses: HashMap<String, Vec<String>>,
}
impl MockBackend {
    pub fn new() -> Self;
    pub fn when(self, agent: &str, response: &str) -> Self;
    pub fn when_tool(self, tool: &str, result: serde_json::Value) -> Self;
}

pub struct AgentHarness { backend: MockBackend }
impl AgentHarness {
    pub async fn assert_contains(&self, agent: &AgentBuilder, input: &str, expected: &str);
    pub async fn assert_tools_called(&self, agent: &AgentBuilder, input: &str, tools: &[&str]);
    pub fn check_contracts(&self, pipeline: &Pipeline) -> Vec<ContractViolation>;
}

pub enum ContractViolation {
    UnproducedKey { consumer: String, key: String },
    DuplicateWrite { agents: Vec<String>, key: String },
    OrphanedOutput { producer: String, key: String },
}
```

---

## 6. Python Bindings (`gemini-live-python`)

### 6.1 Structure

```
crates/gemini-live-python/
├── Cargo.toml
├── pyproject.toml
└── src/
    ├── lib.rs            # #[pymodule] entry point
    ├── py_session.rs     # PySession wrapping SessionHandle
    ├── py_agent.rs       # PyAgent wrapping AgentBuilder
    ├── py_tool.rs        # PyTool wrapping Tool trait
    ├── py_events.rs      # Python async iterators over events
    ├── py_config.rs      # Python-friendly SessionConfig
    └── py_types.rs       # Content, Part, FunctionCall etc.
```

### 6.2 Three-Tier Python API

**Tier 1: Raw wire access**
```python
from gemini_live import wire

session = await wire.connect(api_key="...", model="gemini-2.0-flash-live-001",
    tools=[wire.Tool.url_context()], response_modalities=["TEXT"])
session.send_text("What's on https://news.google.com?")

async for event in session.events():
    match event:
        case wire.TextDelta(text): print(text, end="")
        case wire.TurnComplete(): print()
```

**Tier 2: Agent runtime**
```python
from gemini_live import Agent, Tool

@Tool.function
async def get_weather(city: str) -> dict:
    """Get current weather for a city."""
    return {"temp": 22, "condition": "sunny"}

agent = Agent(name="assistant", model="gemini-2.5-flash-preview-native-audio",
    instruction="You are a helpful assistant.", tools=[get_weather])

async with agent.session(api_key="...") as chat:
    response = await chat.send("What's the weather in London?")
```

**Tier 3: Fluent composition**
```python
from gemini_live import Agent, Pipeline, FanOut
pipeline = researcher >> writer >> (reviewer * 3)
```

### 6.3 Implementation

- Built with **maturin** for PyPI distribution
- Uses **pyo3-async-runtimes** for tokio↔asyncio bridging
- All hot-path operations (WebSocket I/O, audio, protocol parsing) run in Rust with GIL released
- Python only enters for tool function callbacks and user-facing API calls
- Tool registration introspects Python function signatures to auto-generate JSON Schema

### 6.4 Performance vs Pure Python ADK

| Operation | Pure Python ADK | gemini-live (PyO3) |
|---|---|---|
| WebSocket send/recv | asyncio + Python | tokio (Rust) — no GIL |
| Audio encoding/base64 | Python bytes | Rust — 10-50x faster |
| Tool dispatch | asyncio.gather | tokio::join! — true parallelism |
| Jitter buffer | N/A | Lock-free SPSC ring |
| Memory per session | ~50-100MB | ~2-5MB + Python ~15MB |
| Concurrent sessions | GIL-limited | Thread pool — linear scaling |

---

## 7. Mapping: ADK Python → Rust

| ADK Python | Rust Equivalent | Layer |
|---|---|---|
| `LiveRequestQueue` | `mpsc::Sender<LiveRequest>` (LiveSender) | 1 |
| `BaseLlmFlow.run_live()` dual-task | `tokio::select!` on send+recv tasks | 0+1 |
| `ActiveStreamingTool` + input duplication | `broadcast::Sender` fan-out to tool streams | 1 |
| `handle_function_calls_live()` | `ToolDispatcher` with async execution | 1 |
| Agent transfer via connection close+reopen | `AgentRouter` with session migration | 1 |
| `InvocationContext` | `InvocationContext` struct | 1 |
| adk-fluent `Agent().instruct().tool()` | `AgentBuilder::new().instruction().tool()` | 2 |
| adk-fluent `>>`, `\|`, `*`, `//` operators | Rust `Shr`, `BitOr`, `Mul`, `Div` trait impls | 2 |
| adk-fluent S, C, P, M, T modules | Rust modules with builder traits | 2 |
| adk-fluent `review_loop`, `cascade` | `patterns::review_loop`, `patterns::cascade` | 2 |
| adk-fluent `MockBackend`, `AgentHarness` | `testing::MockBackend`, `testing::AgentHarness` | 2 |
| PyO3 bindings | `gemini-live-python` crate | Bindings |

---

## 8. Full Example — All Layers

```rust
use gemini_live::prelude::*;

#[tool(description = "Search the web for information")]
async fn web_search(query: String) -> Result<Value, ToolError> {
    Ok(json!({ "results": ["result1", "result2"] }))
}

#[tool(description = "Search academic papers")]
async fn paper_search(query: String, max_results: Option<u32>) -> Result<Value, ToolError> {
    Ok(json!({ "papers": ["paper1", "paper2"] }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY")?;

    // Define specialized agents
    let researcher = AgentBuilder::new("researcher")
        .model(GeminiModel::Gemini2_5FlashNativeAudio)
        .instruction("Research the given topic thoroughly.")
        .tools(T::function(web_search) | T::function(paper_search) | T::url_context())
        .middleware(M::retry(2) | M::latency())
        .writes("findings");

    let writer = AgentBuilder::new("writer")
        .model(GeminiModel::Gemini2_5FlashNativeAudio)
        .instruction("Write a comprehensive report based on {findings}.")
        .reads("findings")
        .writes("draft");

    let reviewer = AgentBuilder::new("reviewer")
        .model(GeminiModel::Gemini2_5FlashNativeAudio)
        .instruction("Review the draft. Set quality to 'good' or 'needs_work'.")
        .reads("draft")
        .writes("quality");

    // Compose using operator algebra
    let pipeline = researcher
        >> S::pick(&["findings"])
        >> writer
        >> (reviewer * until(|s| s.get::<String>("quality").as_deref() == Some("good")).max(3))
        >> AgentBuilder::new("formatter")
            .instruction("Format the final draft as markdown.")
            .reads("draft")
            .text_only();

    // Execute
    let result = pipeline.ask(&api_key, "Quantum computing advances in 2026").await?;
    println!("{result}");

    Ok(())
}
```

---

## 9. Crate Dependencies

### Layer 0: `gemini-live-wire`

| Crate | Purpose |
|---|---|
| `tokio` (full) | Async runtime |
| `tokio-tungstenite` + `native-tls` | WebSocket client |
| `serde` + `serde_json` | JSON codec |
| `base64` | Audio encoding |
| `parking_lot` | Fast mutexes |
| `thiserror` | Error types |
| `uuid` (v4) | Session/turn IDs |
| `bytes` | Byte manipulation |
| `tracing` (optional) | Structured spans |
| `metrics` (optional) | Prometheus metrics |

### Layer 1: `gemini-live-runtime`

| Crate | Purpose |
|---|---|
| `gemini-live-wire` | Wire protocol |
| `dashmap` | Concurrent HashMap (active tools) |
| `tokio-util` | CancellationToken |
| `arc-swap` | Hot-swap configuration |
| `async-trait` | Async trait support |

### Layer 2: `gemini-live`

| Crate | Purpose |
|---|---|
| `gemini-live-runtime` | Agent runtime |
| (proc-macro crate) | `#[tool]` macro |

### Bindings: `gemini-live-python`

| Crate | Purpose |
|---|---|
| `gemini-live` | All layers |
| `pyo3` | Python bindings |
| `pyo3-async-runtimes` | tokio↔asyncio bridge |
| `pythonize` | serde↔Python conversion |

---

*This design document is the result of auditing the JS GenAI SDK, analyzing the ADK Python bidi streaming source, and studying the adk-fluent composition architecture. Every architectural decision maps to a specific pattern proven in production by Google's own implementations.*
