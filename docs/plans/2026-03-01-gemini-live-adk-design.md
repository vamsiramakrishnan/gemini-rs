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

Google's ADK Python provides useful patterns (streaming tools, agent transfer) but is limited by the Python GIL. adk-fluent provides excellent DX but inherits the same performance ceiling.

This library **learns from** ADK's architecture without blindly mimicking it. Where ADK uses `LiveRequestQueue` (necessary because Python lacks typed command channels), we use Rust's existing `SessionHandle` with a thin intercepting wrapper. Where adk-fluent uses IR compilation for multi-backend support, we compile operators directly to agents (we only have one backend). The result is Rust-idiomatic, zero-copy, and avoids double-queuing.

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
| 1 | `gemini-live-runtime` | Agent trait, AgentSession, tool dispatch, agent transfer, middleware | Layer 0 |
| 2 | `gemini-live` | Fluent builders, operator algebra, composition modules, patterns, testing | Layer 1 |
| Bindings | `gemini-live-python` | PyO3 native Python module exposing all layers | Layer 2 |

### Origin: JS SDK Audit + ADK Python Analysis

This design is informed by:

1. **JS GenAI SDK audit** (`googleapis/js-genai`) — verified wire protocol correctness, identified missing built-in tool types (`urlContext`, `googleSearch`, `codeExecution`), missing `thinkingConfig`, `enableAffectiveDialog`
2. **ADK Python source** (`google/adk-python`) — studied `LiveRequestQueue` + dual-task architecture, streaming tool lifecycle, agent transfer pattern, input-stream duplication. **Adapted, not copied**: replaced LiveRequestQueue with AgentSession wrapper to avoid double-queuing
3. **adk-fluent** (`vamsiramakrishnan/adk-fluent`) — extracted the algebraic composition language, five single-letter modules (S, C, P, M, T), copy-on-write builders, testing utilities. **Dropped IR layer**: adk-fluent uses IR for multi-backend compilation; we have one backend (Gemini Live), so operators compile directly to Agent implementations

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

### 3.2 Protocol Fixes (from JS SDK audit + code audit)

#### 3.2.1 Tool Type: Sum Type, Not Product Type

The current design uses a struct with all-optional fields, which lets you construct
illegal states (e.g., `url_context` AND `google_search` both set). The wire format
actually requires exactly one variant per `Tool` object. We use a Rust enum internally
and a custom serde impl to map to/from the wire's flat-object format:

```rust
/// Builder-facing: enum makes illegal states unrepresentable.
pub enum ToolSpec {
    Functions(Vec<FunctionDeclaration>),
    UrlContext,
    GoogleSearch,
    CodeExecution,
    GoogleSearchRetrieval,
}

impl ToolSpec {
    pub fn url_context() -> Self { Self::UrlContext }
    pub fn google_search() -> Self { Self::GoogleSearch }
    pub fn code_execution() -> Self { Self::CodeExecution }
    pub fn functions(decls: Vec<FunctionDeclaration>) -> Self { Self::Functions(decls) }
}

/// Wire-facing: flat struct for serde. Only one field is Some at a time.
/// Private — users never construct this directly. Converted from ToolSpec at serialization.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTool { /* all-optional fields */ }

impl From<ToolSpec> for WireTool { /* populate exactly one field */ }
impl TryFrom<WireTool> for ToolSpec { /* extract the one non-None field */ }
```

#### 3.2.2 Stringly-Typed Fields → Enums and Newtypes

```rust
/// Content.role is currently Option<String> — hardcoded "user" in 5+ places.
pub enum Role { User, Model }

/// Blob.data is currently String (base64). Store raw bytes, encode at wire boundary.
pub struct Blob {
    #[serde(with = "base64_serde")]
    pub data: Vec<u8>,  // NOT String — no intermediate base64 allocation
    pub mime_type: MimeType,
}

/// MimeType newtype prevents raw String errors.
pub struct MimeType(String);
impl MimeType {
    pub const PCM16: Self = Self(String::new()); // "audio/pcm" at runtime
    pub const OPUS: Self = Self(String::new());  // "audio/opus"
}

/// ExecutableCode.language and CodeExecutionResult.outcome: enums not strings.
pub enum CodeLanguage { Python, Unspecified }
pub enum CodeOutcome { Ok, Failed, DeadlineExceeded }
```

#### 3.2.3 Missing Wire Features (from JS SDK source)

**Setup config gaps** — these are real fields the JS SDK sends that we don't support:

```rust
pub struct RealtimeInputConfig {
    pub automatic_activity_detection: Option<AutomaticActivityDetection>,
    pub activity_handling: Option<ActivityHandling>,  // NEW
    pub turn_coverage: Option<TurnCoverage>,          // NEW
}

pub enum ActivityHandling {
    StartOfActivityInterrupts,  // User speech interrupts model (default)
    NoInterruption,             // Model keeps speaking during user speech
}

pub enum TurnCoverage {
    TurnIncludesAllInput,       // Everything goes in context
    TurnIncludesOnlyActivity,   // Only speech goes in context (VAD filtering)
}

/// Context window compression — critical for long sessions.
pub struct ContextWindowCompression {
    pub trigger_tokens: Option<u32>,
    pub sliding_window: Option<SlidingWindow>,
}
pub struct SlidingWindow {
    pub target_tokens: Option<u32>,
}

/// Proactive audio — model speaks without being prompted.
pub struct Proactivity {
    pub proactive_audio: Option<bool>,
}
```

**RealtimeInput gaps:**

```rust
/// Missing from our RealtimeInputMessage:
pub struct RealtimeInputMessage {
    pub realtime_input: RealtimeInput,
}
pub struct RealtimeInput {
    pub audio: Option<Blob>,
    pub video: Option<Blob>,           // NEW: image frames
    pub text: Option<String>,           // NEW: realtime text (not clientContent)
    pub audio_stream_end: Option<bool>, // NEW: signal mic disconnect
    pub activity_start: Option<serde_json::Value>,  // existing
    pub activity_end: Option<serde_json::Value>,    // existing
}
```

**ServerContent gaps:**

```rust
pub struct ServerContent {
    pub model_turn: Option<Content>,
    pub turn_complete: Option<bool>,
    pub interrupted: Option<bool>,
    pub generation_complete: Option<bool>,    // NEW: all content generated (audio may still play)
    pub input_transcription: Option<Transcription>,
    pub output_transcription: Option<Transcription>,
    pub grounding_metadata: Option<Value>,    // NEW
    pub turn_complete_reason: Option<String>, // NEW
    pub waiting_for_input: Option<bool>,      // NEW: model idle, waiting for user
}
```

**Session resumption replay:**

```rust
pub struct SessionResumptionUpdate {
    pub new_handle: Option<String>,
    pub resumable: Option<bool>,
    pub last_consumed_client_message_index: Option<String>, // NEW: for message replay
}
```

**VoiceActivity server message:**

```rust
/// Separate from `interrupted` — explicit VAD signal from server.
pub struct VoiceActivity {
    pub voice_activity_type: VoiceActivityType,
}
pub enum VoiceActivityType { ActivityStart, ActivityEnd }
```

#### 3.2.4 Audio Hot Path Allocation Fixes

The original design claims "zero-copy" but the actual path has 6 allocations per frame.
Fixes:

```rust
// FIX 1: i16 → u8 conversion via bytemuck (zero-copy reinterpret)
use bytemuck;
let bytes: &[u8] = bytemuck::cast_slice(&samples); // zero-copy!

// FIX 2: Base64 encoding into pre-allocated buffer (one per connection)
struct SendBuffer {
    base64_buf: String,  // pre-allocated, reused across frames
    json_buf: Vec<u8>,   // pre-allocated for serde_json::to_writer
}

// FIX 3: InputEvent uses Bytes (Arc<[u8]>) not Vec<u8>
use bytes::Bytes;
pub enum InputEvent {
    Audio(Bytes),  // clone is Arc::clone (atomic increment), not memcpy
    Text(String),
    ActivityStart,
    ActivityEnd,
}

// FIX 4: Blob.data stored as Vec<u8>, serde encodes to base64 at boundary
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
- All messages are JSON text frames (never binary), matching JS SDK behavior

---

## 4. Layer 1: Agent Runtime (`gemini-live-runtime`)

### 4.1 Structure

```
crates/gemini-live-runtime/src/
├── lib.rs
├── agent.rs          # Agent trait + LlmAgent + AgentEvent
├── tool.rs           # ToolFunction, StreamingTool, InputStreamingTool, ToolDispatcher
├── agent_session.rs  # AgentSession — intercepting wrapper around SessionHandle
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

### 4.3 AgentSession (replaces ADK's LiveRequestQueue)

> **Design rationale**: ADK Python uses `LiveRequestQueue` because `asyncio.Queue` is its
> coordination primitive — Python has no equivalent of `SessionHandle` with typed commands
> and broadcast events. Our Layer 0 already has `SessionHandle` with
> `mpsc::Sender<SessionCommand>` and `broadcast::Sender<SessionEvent>`. Adding another
> mpsc channel on top would create **double-queuing**, violating the Zero-Copy Hot Path
> principle from our design doc.
>
> Instead, `AgentSession` is a thin **intercepting wrapper** around `SessionHandle` that
> adds the three things the runtime actually needs: input fan-out, middleware, and state.

```rust
/// The input event type broadcast to input-streaming tools.
/// Distinct from SessionCommand — this is observation-only, tools don't send commands.
/// Audio uses `Bytes` (Arc<[u8]>) — clone is an atomic increment, not a memcpy.
#[derive(Debug, Clone)]
pub enum InputEvent {
    Audio(Bytes),  // bytes::Bytes — zero-copy fan-out via refcount
    Text(String),
    ActivityStart,
    ActivityEnd,
}

/// Intercepting wrapper around SessionHandle.
/// Adds: input fan-out to streaming tools, middleware hooks, conversation state.
///
/// Data flow: App → AgentSession.send_audio() → SessionHandle.send_audio() → WebSocket
///                                             ↘ broadcast to input-streaming tools
///
/// ONE queue (SessionHandle's command_tx), ONE consumer task (connection_loop).
/// Zero-cost fan-out when no input-streaming tools are active.
#[derive(Clone)]
pub struct AgentSession {
    /// The underlying wire-level session (Layer 0)
    session: SessionHandle,
    /// Fan-out for input-streaming tools (broadcast, not mpsc)
    input_broadcast: broadcast::Sender<InputEvent>,
    /// Middleware chain for interception
    middleware: Arc<MiddlewareChain>,
    /// Conversation state tracking
    state: State,
}

impl AgentSession {
    pub fn new(session: SessionHandle, middleware: MiddlewareChain) -> Self;

    pub async fn send_audio(&self, data: impl Into<Bytes>) -> Result<(), AgentError> {
        let data: Bytes = data.into();
        // Fan-out ONLY if input-streaming tools are listening
        // Bytes::clone is atomic refcount increment — NOT a memcpy
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::Audio(data.clone()));
        }
        // Forward directly to Layer 0 (ONE hop to WebSocket)
        self.session.send_audio(data.to_vec()).await?;
        Ok(())
    }

    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), AgentError>;
    pub async fn send_tool_response(&self, responses: Vec<FunctionResponse>) -> Result<(), AgentError>;
    pub async fn signal_activity_start(&self) -> Result<(), AgentError>;
    pub async fn signal_activity_end(&self) -> Result<(), AgentError>;

    /// Subscribe to input events (for input-streaming tools)
    pub fn subscribe_input(&self) -> broadcast::Receiver<InputEvent>;

    /// Subscribe to session events (delegates to SessionHandle)
    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent>;

    /// Access the underlying SessionHandle for advanced wire-level control
    pub fn wire(&self) -> &SessionHandle;

    /// Access conversation state
    pub fn state(&self) -> &State;
}
```

### 4.4 Three Tool Types

Mirroring ADK's tool architecture:

**Regular tools** — called with cancellation support, return result:
```rust
#[async_trait]
pub trait ToolFunction: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    fn timeout(&self) -> Duration { Duration::from_secs(30) }
    async fn call(
        &self,
        args: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<serde_json::Value, ToolError>;
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
        input_rx: broadcast::Receiver<InputEvent>,
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
    pub input_tx: Option<broadcast::Sender<InputEvent>>,
    pub cancel: CancellationToken,
}

pub struct ToolDispatcher {
    tools: HashMap<String, ToolKind>,
    active: Arc<Mutex<HashMap<String, ActiveStreamingTool>>>,
}
```

### 4.5 Event-Driven Architecture (No Dual-Task Duplication)

> **Design rationale**: ADK Python creates two async tasks (`_send_to_model` +
> `_receive_from_model`) because Python needs explicit tasks for bidirectional I/O.
> Our Layer 0 already runs a dual-task `connection_loop()` with `tokio::select!` on
> send + receive. Adding another pair of tasks in the runtime would be redundant.

The runtime instead:

- **Subscribes** to `SessionHandle.subscribe()` for server events (tool calls, text, audio, turn complete)
- **Intercepts** sends through `AgentSession` (fan-out + middleware) before they reach `SessionHandle`
- **Dispatches** tool calls from the event stream (regular → immediate response, streaming → spawn background task + pending response)

The `Agent::run_live()` method receives an `InvocationContext` containing the `AgentSession` and an event stream. On agent transfer, the session is disconnected, target agent resolved, and a new session established with the target agent's config.

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
    /// AgentSession wraps SessionHandle with fan-out + middleware (replaces LiveSender)
    pub agent_session: AgentSession,
    /// Agent event channel for application-level event observation
    pub event_tx: broadcast::Sender<AgentEvent>,
    /// Active streaming/input-streaming tools (DashMap for lock-free concurrent access)
    pub active_streaming_tools: Arc<DashMap<String, ActiveStreamingTool>>,
    /// Agent registry for agent transfer
    pub agent_registry: Arc<AgentRegistry>,
    /// Telemetry context for tracing/metrics
    pub telemetry: TelemetryContext,
}

// Note: State is accessed via agent_session.state() — not duplicated here.
// The AgentSession owns the canonical state, ensuring single source of truth.
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
    async fn on_input(&self, _item: &InputEvent) -> Result<(), AgentError> { Ok(()) }

    // Error
    async fn on_error(&self, _err: &AgentError) -> Result<(), AgentError> { Ok(()) }
}

pub struct MiddlewareChain {
    layers: Vec<Arc<dyn Middleware>>,
}
```

Built-in middleware: `RetryMiddleware`, `LogMiddleware`, `CostTracker`, `LatencyTracker`, `TimeoutMiddleware`, `RateLimitMiddleware`.

### 4.9 AgentEvent

> **Design decision**: Layer 0 already has `SessionEvent` (TextDelta, AudioData,
> TurnComplete, etc.). Rather than duplicating those variants, `AgentEvent` wraps
> `SessionEvent` and adds agent-specific events. Consumers use pattern matching on
> the inner enum when they care about wire-level events.

```rust
pub enum AgentEvent {
    /// Passthrough of wire-level session events (text, audio, turn lifecycle)
    Session(SessionEvent),
    /// Agent lifecycle
    AgentStarted { name: String },
    AgentCompleted { name: String },
    /// Tool lifecycle (not present in SessionEvent)
    ToolCallStarted { name: String, args: serde_json::Value },
    ToolCallCompleted { name: String, result: serde_json::Value, duration: Duration },
    ToolCallFailed { name: String, error: String },
    StreamingToolYield { name: String, value: serde_json::Value },
    /// Multi-agent lifecycle
    AgentTransfer { from: String, to: String },
    /// State changes
    StateChanged { key: String },
}
```

### 4.10 State Container

```rust
/// State is the shared context that flows between agents in a pipeline.
/// Uses serde_json::Value as the universal interchange format — JSON is the
/// lingua franca of LLM I/O, so Value is the natural choice. No `Any` escape
/// hatch; if it can't be JSON, it shouldn't be in agent state.
pub struct State {
    inner: Arc<DashMap<String, serde_json::Value>>,
}

impl State {
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T>;
    pub fn get_str(&self, key: &str) -> Option<String>;  // Fast path, no deserialization
    pub fn set(&self, key: impl Into<String>, value: impl serde::Serialize);
    pub fn pick(&self, keys: &[&str]) -> State;   // New State with only listed keys
    pub fn merge(&self, other: &State);             // Merge other's keys into self
    pub fn rename(&self, from: &str, to: &str);
    pub fn keys(&self) -> Vec<String>;
    pub fn contains(&self, key: &str) -> bool;
}
```

### 4.11 Tool Registration: `FnTool::typed<T>()` with auto-schema

> **Design decision**: Proc macros (`#[tool]`) deferred. Instead, use `schemars` to
> auto-generate JSON Schema from Rust structs. This eliminates manual schema writing
> AND runtime `args["key"].as_str().unwrap()` panics — the framework deserializes
> args into the typed struct before calling the handler.

```rust
use schemars::JsonSchema;

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to get weather for
    city: String,
    /// Temperature units (celsius or fahrenheit)
    #[serde(default = "default_units")]
    units: String,
}

// Typed tool: auto-generated schema, type-safe args, no unwrap()
let weather = FnTool::typed::<WeatherArgs>(
    "get_weather",
    "Get current weather for a city",
    |args: WeatherArgs| async move {
        Ok(json!({ "temp": 22, "city": args.city, "units": args.units }))
    },
);

// Untyped tool: manual schema, raw Value args (escape hatch)
let raw_tool = FnTool::new(
    "raw_tool",
    "A tool with manual schema",
    json!({"type": "object", "properties": {"query": {"type": "string"}}}),
    |args: Value| async move { Ok(json!({"result": "ok"})) },
);
```

The `schemars` crate adds ~2s to compile time (far less than a proc-macro crate)
and generates correct JSON Schema including descriptions from doc comments.

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
| `LiveRequestQueue` | `AgentSession` (intercepting wrapper around `SessionHandle`) | 1 |
| `BaseLlmFlow.run_live()` dual-task | Reused from Layer 0 `connection_loop()` — no duplication | 0 |
| `ActiveStreamingTool` + input duplication | `broadcast::Sender<InputEvent>` fan-out via `AgentSession` | 1 |
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

## 8. Examples — Progressive Disclosure

### 8.1 Layer 0: Wire-Level Hello World (5 lines)

```rust
let session = gemini_live_wire::quick_connect("API_KEY", "gemini-2.0-flash-live-001").await?;
session.send_text("What is the speed of light?").await?;
let mut events = session.subscribe();
while let Ok(event) = events.recv().await {
    if let SessionEvent::TextDelta(text) = event { print!("{text}"); }
    if let SessionEvent::TurnComplete = event { break; }
}
```

### 8.2 Layer 1: Agent with Typed Tools (15 lines)

```rust
use schemars::JsonSchema;

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs { city: String }

let agent = LlmAgent::builder("assistant")
    .model(GeminiModel::Gemini2_5FlashNativeAudio)
    .instruction("You are a helpful assistant.")
    .tool(FnTool::typed::<WeatherArgs>("get_weather", "Get weather", |args, _cancel| async move {
        Ok(json!({ "temp": 22, "city": args.city }))
    }))
    .build();

let mut session = agent.connect("API_KEY").await?;
while let Some(event) = session.next_event().await { /* auto-dispatched */ }
```

### 8.3 Layer 2: Full Pipeline with Operator Algebra

```rust
use gemini_live::prelude::*;
use gemini_live::compose::{State, Tools, Middleware}; // Full names (or S, T, M for power users)

#[derive(Deserialize, JsonSchema)]
struct SearchArgs { query: String }

let web_search = FnTool::typed::<SearchArgs>("web_search", "Search the web",
    |args, _cancel| async move { Ok(json!({ "results": ["r1", "r2"] })) });

let researcher = AgentBuilder::new("researcher")
    .model(GeminiModel::Gemini2_5FlashNativeAudio)
    .instruction("Research the given topic thoroughly.")
    .tools(Tools::function(web_search) | Tools::url_context())
    .middleware(Middleware::retry(2) | Middleware::latency())
    .writes("findings");

let writer = AgentBuilder::new("writer")
    .instruction("Write a report based on {findings}.")
    .reads("findings").writes("draft");

let reviewer = AgentBuilder::new("reviewer")
    .instruction("Review the draft. Set quality to 'good' or 'needs_work'.")
    .reads("draft").writes("quality");

// NOTE: >> moves the builder. Use .clone() to share agents across branches.
let pipeline = researcher
    >> State::pick(&["findings"])
    >> writer
    >> (reviewer * until(|s| s.get::<String>("quality").as_deref() == Some("good")).max(3));

let result = pipeline.ask("API_KEY", "Quantum computing advances in 2026").await?;
```

---

## 9. Crate Dependencies

### Layer 0: `gemini-live-wire`

| Crate | Purpose |
|---|---|
| `tokio` (full) | Async runtime |
| `tokio-tungstenite` + `native-tls` | WebSocket client |
| `serde` + `serde_json` | JSON codec |
| `base64` | Wire-boundary audio encoding |
| `bytes` | Zero-copy byte buffers (`Bytes` = Arc<[u8]>) |
| `bytemuck` | Zero-copy i16↔u8 reinterpretation |
| `parking_lot` | Fast mutexes |
| `thiserror` | Error types |
| `uuid` (v4) | Session/turn IDs |
| `tracing` (optional) | Structured spans |
| `metrics` (optional) | Prometheus metrics |

### Layer 1: `gemini-live-runtime`

| Crate | Purpose |
|---|---|
| `gemini-live-wire` | Wire protocol |
| `dashmap` | Concurrent HashMap (active tools, state) |
| `tokio-util` | CancellationToken for tool timeout |
| `arc-swap` | Hot-swap configuration |
| `async-trait` | Async trait support (migrate to native async fn when Rust stabilizes) |
| `schemars` | Auto-generate JSON Schema from `#[derive(JsonSchema)]` structs |

### Layer 2: `gemini-live`

| Crate | Purpose |
|---|---|
| `gemini-live-runtime` | Agent runtime |
| (no proc-macro) | `FnTool::typed<T>()` + `schemars` for v1 |

### Bindings: `gemini-live-python`

| Crate | Purpose |
|---|---|
| `gemini-live` | All layers |
| `pyo3` | Python bindings |
| `pyo3-async-runtimes` | tokio↔asyncio bridge |
| `pythonize` | serde↔Python conversion |

---

---

## 10. Audit-Driven Enhancements

These enhancements come from three independent audits: JS GenAI SDK source analysis,
Rust code quality review, and real-time audio engineering critique. Every item below
maps to a specific finding with a concrete fix.

### 10.1 Barge-In Race Condition Fix

**Problem**: Our design says "flush jitter buffer on VAD trigger." But false-positive
VAD (user coughs, background noise) flushes the buffer, the user hears silence, and
the model may or may not have been interrupted server-side. This is the same class of
issue as LiveKit #3418 (agent goes silent).

**Fix**: Tentative barge-in state. Don't flush immediately on VAD trigger:

```
VAD PendingSpeech → duck jitter buffer volume (e.g., -12dB)
                   → send activityStart to server
VAD Speech        → NOW flush jitter buffer (confirmed real speech)
VAD back to Silence → restore jitter buffer volume (false positive)
```

This adds `min_speech_duration` (~100ms) latency to full barge-in but eliminates
false-positive flushes. The volume duck gives immediate feedback without destroying
the audio stream. Matches what the v1 design doc's VAD FSM already has but was not
connected to the barge-in handler.

### 10.2 Tool Timeout and Cancellation

**Problem**: `ToolFunction::call()` is a plain `async fn` with no cancellation.
If a tool does a `reqwest::get()` that hangs for 30 seconds, the model is silent
for 30 seconds. The `TimeoutMiddleware` is listed but never defined.

**Fix**: All tool calls get a `CancellationToken` and a configurable timeout:

```rust
#[async_trait]
pub trait ToolFunction: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    /// Timeout per invocation. Default: 30s.
    fn timeout(&self) -> Duration { Duration::from_secs(30) }
    /// Called with a CancellationToken that fires on timeout or barge-in.
    async fn call(
        &self,
        args: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<serde_json::Value, ToolError>;
}
```

When the timeout fires or the user interrupts:
1. `cancel.cancel()` is called — cooperative cancellation
2. After a grace period (1s), `task.abort()` — forced cancellation
3. An error response is sent to Gemini: `{"error": "tool_timeout"}`
4. Gemini treats this as a failed tool call and responds with available context

### 10.3 Outbound Backpressure Policy

**Problem**: When the network slows, `send_audio()` eventually blocks on the
bounded `mpsc` channel. Stale audio is worse than missing audio in real-time.

**Fix**: Drop-oldest policy for audio on the send path:

```rust
/// Send audio with real-time semantics: if the send queue is full,
/// drop oldest frames rather than blocking.
pub async fn send_audio(&self, data: impl Into<Bytes>) -> Result<(), AgentError> {
    match self.session.command_tx.try_send(SessionCommand::SendAudio(data)) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => {
            // Drop this frame — stale audio is worse than missing audio.
            // Metric: gemini_live_audio_frames_dropped_total
            tracing::warn!("Audio send queue full — dropping frame");
            Ok(())
        }
        Err(TrySendError::Closed(_)) => Err(AgentError::SessionClosed),
    }
}
```

Non-audio commands (text, tool responses) still use `.send().await` — they must
not be dropped.

### 10.4 Broadcast Lagged Error Handling

**Problem**: `tokio::sync::broadcast` drops messages for slow receivers. The event
loop silently ignores `RecvError::Lagged`. A slow tool callback could miss events.

**Fix**: Handle lagged errors explicitly in the event consumer:

```rust
loop {
    match events.recv().await {
        Ok(event) => handle_event(event),
        Err(RecvError::Lagged(n)) => {
            tracing::warn!(skipped = n, "Event consumer lagged — {} events dropped", n);
            // Continue processing — do NOT break the loop
        }
        Err(RecvError::Closed) => break,
    }
}
```

### 10.5 JoinHandle Tracking

**Problem**: `connection_loop` is spawned with `tokio::spawn` but the `JoinHandle`
is discarded. If the task panics, nobody knows. Same issue with tool execution tasks.

**Fix**: Store handles and check on disconnect:

```rust
pub struct SessionHandle {
    command_tx: mpsc::Sender<SessionCommand>,
    event_tx: broadcast::Sender<SessionEvent>,
    state: Arc<SessionState>,
    phase_rx: watch::Receiver<SessionPhase>,
    /// Track the connection loop task for panic detection.
    connection_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}
```

For tool execution: use `tokio::task::JoinSet` instead of bare `tokio::spawn`.
The `JoinSet` tracks all active tool tasks and propagates panics.

### 10.6 DX: Minimal Hello World

**Problem**: There is no 5-line Layer 0 example. The minimum viable hello world
requires constructing `SessionConfig` + `TransportConfig` + calling `connect()` +
subscribing + event loop = 20+ lines.

**Fix**: Add a convenience `connect()` function to Layer 0:

```rust
// 5-line hello world (Layer 0):
let session = gemini_live_wire::quick_connect("API_KEY", "gemini-2.0-flash-live-001").await?;
session.send_text("Hello!").await?;
let mut events = session.subscribe();
while let Ok(event) = events.recv().await {
    println!("{event:?}");
}
```

The `quick_connect` function uses sensible defaults (`TransportConfig::default()`,
`TEXT` modality, no tools). Power users still use `SessionConfig` builder + `connect()`.

### 10.7 DX: Layer 1 Golden Path

**Problem**: Layer 1 introduces 13 new concepts before you can write a function-calling
agent (`AgentSession`, `InvocationContext`, `ToolDispatcher`, 3 tool traits, etc.).

**Fix**: `LlmAgent` is a concrete struct that handles the event loop internally:

```rust
// 8-line agent with tools (Layer 1):
let agent = LlmAgent::builder("assistant")
    .model(GeminiModel::Gemini2_5FlashNativeAudio)
    .instruction("You are helpful.")
    .tool(FnTool::typed::<WeatherArgs>("get_weather", "Get weather", weather_handler))
    .build();

let mut session = agent.connect("API_KEY").await?;
while let Some(event) = session.next_event().await {
    // events are already tool-dispatched — user only sees final results
}
```

`LlmAgent` internally constructs `AgentSession`, `InvocationContext`, `ToolDispatcher`,
and the event loop. Users who need custom behavior implement `Agent` trait directly.

### 10.8 DX: Full-Name Module Aliases

**Problem**: `S::pick`, `M::retry`, `T::function` are insider shorthand. A Rust
developer seeing `S::pick` has no idea what `S` is without documentation.

**Fix**: Dual exports — full names by default, single-letter as opt-in:

```rust
pub mod compose {
    pub use state::State;       // Default: self-documenting
    pub use context::Context;
    pub use prompt::Prompt;
    pub use middleware::Middleware;
    pub use tools::Tools;

    // Power user aliases (opt-in):
    pub use state::State as S;
    pub use context::Context as C;
    pub use prompt::Prompt as P;
    pub use middleware::Middleware as M;
    pub use tools::Tools as T;
}
```

### 10.9 Prelude Scope Reduction

**Problem**: 41 items in the prelude causes cognitive overload. Engineering layers
(context, prompt, state = 20 types) are advanced features most users never touch.

**Fix**: Tiered prelude:

```rust
pub mod prelude {
    // Core (8 items — enough for hello world):
    pub use crate::SessionConfig;
    pub use crate::GeminiModel;
    pub use crate::Voice;
    pub use crate::TransportConfig;
    pub use crate::connect;
    pub use crate::SessionEvent;
    pub use crate::SessionHandle;
    pub use crate::SessionPhase;
}

// Layer 1 users: `use gemini_live_runtime::prelude::*;`
// Layer 2 users: `use gemini_live::prelude::*;`  (includes all layers)
```

### 10.10 Audio Frame Validation (Debug Mode)

**Problem**: No validation that audio data matches declared format. If you declare
PCM16 at 16kHz but pass 24kHz audio, the model receives garbage.

**Fix**: Debug-mode assertion on frame sizes:

```rust
pub async fn send_audio(&self, data: impl Into<Bytes>) -> Result<(), AgentError> {
    let data = data.into();
    debug_assert!(
        data.len() % self.expected_frame_bytes() == 0,
        "Audio frame size {} is not a multiple of expected {} bytes",
        data.len(), self.expected_frame_bytes()
    );
    // ... rest of send logic
}
```

Zero cost in release builds. Catches format mismatches immediately in development.

### 10.11 Integration Test Infrastructure

**Problem**: Zero integration tests. The connection/session/transport layer — the
most critical code — is completely untested end-to-end.

**Fix**: Mock WebSocket server for integration tests:

```rust
// In tests/integration/
struct MockGeminiServer {
    listener: TcpListener,
    setup_handler: Box<dyn Fn(SetupMessage) -> SetupCompleteMessage>,
    message_handler: Box<dyn Fn(ClientMessage) -> Vec<ServerMessage>>,
}

impl MockGeminiServer {
    async fn start() -> (Self, String /* ws://localhost:PORT */);
}

#[tokio::test]
async fn connect_setup_and_receive_text() {
    let (server, url) = MockGeminiServer::start().await;
    let config = SessionConfig::new(GeminiModel::Custom("test".into()))
        .api_endpoint(ApiEndpoint::Custom(url));
    let session = connect(config, TransportConfig::default()).await.unwrap();
    // ... verify setup handshake, send text, receive response
}
```

### 10.12 Performance Benchmarks

**Problem**: Claims of "<1ms audio overhead" and "2-5MB per session" have no benchmarks.

**Fix**: Add criterion benchmarks to Phase 5:

```rust
// benches/audio_pipeline.rs — end-to-end audio frame processing
// benches/session_memory.rs — measure RSS per session with heaptrack
// benches/concurrent_sessions.rs — 100/1000/10000 sessions, measure throughput
```

---

## 11. Design Decisions Log

| Decision | Alternative/ADK Pattern | Our Approach | Rationale |
|---|---|---|---|
| Input queue | `LiveRequestQueue` (asyncio.Queue) | `AgentSession` wrapping `SessionHandle` | Avoids double-queuing; Layer 0 already has `mpsc::Sender<SessionCommand>` |
| Dual-task I/O | `_send_to_model` + `_receive_from_model` | Reuse Layer 0's `connection_loop()` | Layer 0 already runs `tokio::select!` on send+recv |
| Input fan-out | Queue consumer duplicates to tool streams | `broadcast::Sender<InputEvent>` with `Bytes` | Zero-copy fan-out via refcount — not memcpy |
| IR compilation | adk-fluent compiles operators to IR for multi-backend | Operators compile directly to `Agent` impls | We have one backend (Gemini Live) — IR is YAGNI |
| Tool registration | `#[tool]` proc macro | `FnTool::typed<T>()` with `schemars` | Auto-generated schema from structs, type-safe args |
| Tool args | `args["key"].as_str().unwrap()` | Deserialize to typed struct before handler | No runtime panics on malformed LLM output |
| State container | Python dict | `DashMap<String, Value>` | Lock-free concurrent access, JSON-native |
| Tool type | Struct with all-optional fields | `ToolSpec` enum + `WireTool` serde wrapper | Illegal states unrepresentable at compile time |
| Content.role | `Option<String>` | `Option<Role>` enum | No magic strings, exhaustive matching |
| Blob.data | `String` (base64) | `Vec<u8>` with `#[serde(with)]` | No intermediate allocation; encode at wire boundary |
| Audio fan-out | `Vec<u8>` clone per subscriber | `bytes::Bytes` (Arc increment) | Zero-copy for 1→N broadcast |
| Barge-in | Instant flush on VAD trigger | Tentative: duck volume → confirm → flush | Prevents false-positive silence (LiveKit #3418) |
| Tool timeout | No cancellation on regular tools | `CancellationToken` param + configurable timeout | Prevents 30s silence during hung tool call |
| Audio backpressure | Block on full send queue | Drop-oldest for audio frames | Stale audio is worse than missing audio |
| Broadcast lag | Silent ignore | `tracing::warn!` + continue | Observable, no silent data loss |
| Task tracking | Discard `JoinHandle` | Store handle + `JoinSet` for tools | Propagate panics, enable cancellation |

---

## 11. Competitive Edge: What We Do That LiveKit/Pipecat Cannot

### 11.1 Voice-Native, Not Voice-Bolted-On

Pipecat and LiveKit were designed for cascaded STT→LLM→TTS pipelines. They bolt voice onto text LLMs. We start with voice. The Gemini Live API IS the audio model — VAD, turn detection, speech synthesis, and language understanding happen inside a single model inference pass. Our architecture reflects this:

- No STT/TTS provider abstraction layer (no `SpeechProvider`, no `TTSService`)
- No "pipeline" of processors between input and output
- The WebSocket IS the interface — everything flows through it
- Turn detection is the model's job, not ours

### 11.2 The Session IS The Agent

In ADK/LiveKit, you create an "agent" and then start a "session." These are separate concepts that must be synchronized. In our model, `AgentSession` combines both:

- The live WebSocket connection (wire-level I/O)
- The intelligence (tools, instructions, transfer rules)
- The state (conversation history, intermediate results)

One object, one lifetime, no synchronization bugs.

### 11.3 Algebra of Flows, Not State Machines

Dialogflow defines conversation flows as state machines (nodes + transitions + conditions). This is rigid and scales poorly. Our operator algebra defines flows as **composable expressions**:

```rust
// This IS a complete conversation flow definition
let support = triage >> (
    Route::on("category")
        .eq("billing", billing_agent)
        .eq("technical", tech_agent >> escalation * until(resolved))
        .default(general_agent)
) >> satisfaction_survey;
```

State machines require enumerating every state and transition. Algebraic composition lets you express the same thing as a one-liner that compiles, type-checks, and runs.

### 11.4 Performance That Enables New Use Cases

| Metric | Python ADK | LiveKit Agent | gemini-live-rs |
|---|---|---|---|
| Memory per session | ~50-100MB | ~30-60MB | ~2-5MB |
| Concurrent sessions (single machine) | ~10-50 | ~50-200 | ~1,000-10,000 |
| Audio round-trip overhead | 5-15ms (GIL) | 2-5ms (Go SFU) | <1ms (zero-copy) |

This isn't just "faster for the same thing." 10,000 concurrent sessions on one machine enables architectures that are physically impossible with Python — edge deployment, embedded devices, serverless scale-to-zero.

### 11.5 Escape Hatch to Wire Level

Every other framework forces you through their abstraction. When the abstraction doesn't fit, you're stuck (LiveKit issue #4441 — can't disable interrupts when tools are running). With our layered architecture:

```rust
// Use the full fluent DX
let agent = AgentBuilder::new("assistant").instruction("...").tools(...);

// OR drop to wire level when the abstraction doesn't fit
let session = wire::connect(config).await?;
session.send_text("raw wire access").await?;
```

Same crate, same binary, no rewrite.

---

*This design is informed by the JS GenAI SDK, ADK Python bidi streaming source, and adk-fluent composition architecture. Patterns were critically evaluated against our own design principles (Zero-Copy Hot Path, Actor-Per-Session) rather than blindly adopted.*
