# gemini-rs

## Architecture

Three-crate layered stack for the Gemini Multimodal Live API:

```
                    +--------------------------+
                    |    adk-rs-fluent (L2)     |  Fluent DX, operator algebra, composition
                    |  AgentBuilder, Live, S/C  |
                    |  /T/P/M/A, Composable     |
                    +-----------+--------------+
                                |
                    +-----------+--------------+
                    |      rs-adk (L1)          |  Agent runtime, tools, state, phases
                    |  LiveSessionBuilder,      |
                    |  State, TextAgent, Phase  |
                    +-----------+--------------+
                                |
                    +-----------+--------------+
                    |      rs-genai (L0)        |  Wire protocol, transport, auth, types
                    |  SessionHandle, Content,  |
                    |  Transport, Codec, VAD    |
                    +--------------------------+
```

Plus `cookbooks/ui` (Axum WebSocket UI), `cookbooks/agents`, `cookbooks/voice-chat`, `cookbooks/tool-calling`, `cookbooks/transcription`, `cookbooks/text-chat`, and `tools/adk-transpiler`.

## Import Guidance

Always import from the highest-level crate you need:

```rust
// Full fluent DX (recommended for applications)
use adk_rs_fluent::prelude::*;

// Runtime only (building custom processors)
use rs_adk::*;

// Wire protocol only (raw WebSocket access)
use rs_genai::prelude::*;
```

## Core API Patterns

### Fluent Agent Builder (Text Agents)

```rust
let agent = AgentBuilder::new("analyst")
    .model(GeminiModel::Gemini2_0Flash)
    .instruction("Analyze the given topic")
    .temperature(0.3)
    .google_search()
    .thinking(2048)
    .build(llm);

let result = agent.run(&state).await?;
```

Copy-on-write immutable builders -- every setter returns a new builder, original unchanged.

### Live Session (Voice)

```rust
let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .voice(Voice::Kore)
    .instruction("You are a weather assistant")
    .greeting("Greet the user and ask how you can help.")
    .tools(dispatcher)
    .transcription(true, true)
    .on_audio(|data| playback_tx.send(data.clone()).ok())
    .on_text(|t| print!("{t}"))
    .on_interrupted(|| async { playback.flush().await })
    .on_turn_complete(|| async { println!("Turn done") })
    .connect_vertex("project-id", "us-central1", token)
    .await?;

handle.send_audio(pcm_bytes).await?;
handle.send_text("Hello").await?;
handle.disconnect().await?;
```

### Tool Definition

**SimpleTool** (raw JSON args):

```rust
let tool = SimpleTool::new(
    "get_weather", "Get weather for a city",
    Some(json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]})),
    |args| async move {
        let city = args["city"].as_str().unwrap_or("Unknown");
        Ok(json!({"temp": 22, "city": city}))
    },
);
```

**TypedTool** (auto-generated JSON Schema from `schemars::JsonSchema`):

```rust
#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to get weather for
    city: String,
}

let tool = TypedTool::new::<WeatherArgs>(
    "get_weather", "Get weather for a city",
    |args: WeatherArgs| async move {
        Ok(json!({"temp": 22, "city": args.city}))
    },
);
```

**T module composition** for Live sessions:

```rust
Live::builder()
    .with_tools(
        T::simple("get_weather", "Get weather", |args| async move {
            Ok(json!({"temp": 22}))
        })
        | T::google_search()
        | T::code_execution()
    )
```

### State Management

```rust
let state = State::new();

// Basic get/set with automatic serde serialization
state.set("name", "Alice");
let name: Option<String> = state.get("name");

// Atomic read-modify-write
let count = state.modify("count", 0u32, |n| n + 1);

// Prefix-scoped accessors
state.app().set("flag", true);              // writes "app:flag"
state.user().set("name", "Bob");            // writes "user:name"
state.session().set("turn_count", 5);       // writes "session:turn_count"
state.turn().set("transcript", "hello");    // writes "turn:transcript" (cleared each turn)
state.bg().set("task_id", "abc");           // writes "bg:task_id"
let risk: Option<f64> = state.derived().get("risk");  // reads "derived:risk" (read-only)

// Derived fallback: state.get("risk") auto-checks "derived:risk" if "risk" not found
state.set("derived:risk", 0.85);
assert_eq!(state.get::<f64>("risk"), Some(0.85));

// Compile-time typed keys
const TURN_COUNT: StateKey<u32> = StateKey::new("session:turn_count");
state.set_key(&TURN_COUNT, 5);
let count: Option<u32> = state.get_key(&TURN_COUNT);

// Zero-copy borrow
let len = state.with("name", |v| v.as_str().unwrap().len());

// Delta tracking (transactional)
let tracked = state.with_delta_tracking();
tracked.set("key", "val");
tracked.commit();   // or tracked.rollback();
```

**State prefixes**: `session:`, `derived:` (read-only), `turn:` (cleared each turn), `app:`, `bg:`, `user:`, `temp:`

### Phase System

```rust
Live::builder()
    .phase("greeting")
        .instruction("Welcome the user warmly")
        .transition("main", |s| s.get::<bool>("greeted").unwrap_or(false))
        .on_enter(|state, writer| async move { state.set("entered", true); })
        .done()
    .phase("main")
        .dynamic_instruction(|s| {
            let topic: String = s.get("topic").unwrap_or_default();
            format!("Discuss {topic}")
        })
        .tools(vec!["search".into(), "lookup".into()])
        .transition("farewell", |s| s.get::<bool>("done").unwrap_or(false))
        .guard(|s| s.get::<bool>("verified").unwrap_or(false))
        .with_context(|s| format!("Customer: {}", s.get::<String>("name").unwrap_or_default()))
        .done()
    .phase("farewell")
        .instruction("Say goodbye")
        .terminal()
        .done()
    .initial_phase("greeting")
    // Phase defaults inherited by all phases
    .phase_defaults(|p| {
        p.with_state(&["emotional_state", "risk_level"])
         .when(|s| s.get::<String>("risk").unwrap_or_default() == "high", "Show extra empathy.")
         .prompt_on_enter(true)
    })
```

### Extraction Pipeline

```rust
#[derive(Deserialize, Serialize, JsonSchema)]
struct OrderState { items: Vec<String>, phase: String }

let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .instruction("Restaurant order assistant")
    .extract_turns::<OrderState>(flash_llm, "Extract order items and phase")
    .on_extracted(|name, value| async move { println!("{name}: {value}"); })
    .connect_vertex(project, location, token)
    .await?;

// Read latest extraction at any time
let order: Option<OrderState> = handle.extracted("OrderState");
```

### Text Agent Combinators

```rust
// Sequential pipeline: a >> b >> c
let pipeline = AgentBuilder::new("writer").instruction("Write a draft")
    >> AgentBuilder::new("reviewer").instruction("Review and improve");

// Parallel fan-out: a | b
let fan_out = AgentBuilder::new("research") | AgentBuilder::new("summarize");

// Fixed loop: agent * 3
let polished = AgentBuilder::new("refiner").instruction("Polish") * 3;

// Conditional loop: agent * until(predicate)
let converge = AgentBuilder::new("iterate") * until(|v| v["done"].as_bool().unwrap_or(false));

// Fallback chain: a / b
let robust = AgentBuilder::new("primary") / AgentBuilder::new("fallback");

// Compile and run
let agent = pipeline.compile(llm);
let result = agent.run(&state).await?;
```

### Watchers and Temporal Patterns

```rust
Live::builder()
    // State watchers
    .watch("app:score")
        .crossed_above(0.9)
        .then(|old, new, state| async move { state.set("alert", true); })
    .watch("app:status")
        .changed_to(json!("complete"))
        .blocking()
        .then(|old, new, state| async move { /* ... */ })
    // Temporal patterns
    .when_sustained("confused", |s| s.get::<bool>("confused").unwrap_or(false),
        Duration::from_secs(30), |state, writer| async move { /* offer help */ })
    .when_turns("stuck", |s| s.get::<bool>("repeating").unwrap_or(false),
        3, |state, writer| async move { /* break loop */ })
```

### Agent-as-Tool

```rust
let verifier = AgentBuilder::new("verifier")
    .instruction("Verify caller identity")
    .build(llm.clone());

Live::builder()
    .agent_tool("verify_identity", "Verify caller identity", verifier)
    .agent_tool("calc_payment", "Calculate payment plans", calc_pipeline)
```

### Async Function Calling & Scheduling

Per-tool non-blocking behavior and response scheduling modes map directly to the Gemini Live API:

```rust
// L0: Per-function behavior on the wire
FunctionDeclaration {
    name: "search_kb".into(),
    description: "Search knowledge base".into(),
    parameters: Some(schema),
    behavior: Some(FunctionCallingBehavior::NonBlocking),  // model continues while tool runs
}

// L0: Scheduling on async responses
FunctionResponse {
    name: "search_kb".into(),
    response: json!({"results": [...]}),
    id: call.id.clone(),
    scheduling: Some(FunctionResponseScheduling::WhenIdle),  // deliver when model finishes
}
```

**Scheduling modes** (on `FunctionResponse`):
- `Interrupt`: Model halts current output and immediately reports the result
- `WhenIdle`: Model waits until it finishes current output before handling
- `Silent`: Model integrates the result without notifying the user

**Platform support**: Async tool calling (`NonBlocking` behavior + scheduling) is only supported on **Google AI**. On **Vertex AI**, these fields are automatically stripped from the wire — `behavior` is removed from `FunctionDeclaration` in the setup message, and `scheduling` is removed from `FunctionResponse`. This means you can set `NonBlocking` and `WhenIdle` unconditionally in your code; the SDK handles the platform difference. Use `config.supports_async_tools()` to check at runtime.

**L1/L2 integration**: `ToolExecutionMode::Background` automatically sets `behavior: NonBlocking` on the wire declaration and passes the scheduling mode through to responses:

```rust
// L2 fluent API
Live::builder()
    .tool_background("search_kb")                                    // default WhenIdle scheduling
    .tool_background_with_scheduling("log_event", FunctionResponseScheduling::Silent)  // silent
```

### Control Plane Features

**Steering Modes** — Control how the phase machine delivers instructions to the model:

```rust
Live::builder()
    // Default: replace system instruction on phase transition
    .steering_mode(SteeringMode::InstructionUpdate)
    // Lighter: inject context via send_client_content (model-role turns)
    .steering_mode(SteeringMode::ContextInjection)
    // Both: instruction on transition, context injection per turn
    .steering_mode(SteeringMode::Hybrid)
```

**Soft Turn Detection** — Proactive silence awareness when `proactiveAudio` is enabled:

```rust
Live::builder()
    .soft_turn_timeout(Duration::from_secs(2))  // Fire soft turn if model stays silent 2s after VAD end
```

Soft turns run a lightweight pipeline (extractors, watchers, phase transitions) but do NOT prompt the model — respecting its decision to stay silent.

**Conversation Repair** — Nudge the model when required information isn't being gathered:

```rust
Live::builder()
    .repair(RepairConfig::default())  // nudge after 3 turns, escalate after 6
    .repair(RepairConfig::new().nudge_after(2).escalate_after(5))
    .phase("gather_info")
        .needs(&["customer_id", "account_number"])
        .transition("escalation", S::is_true("repair:escalation"))
        .done()
```

**Tool Availability Advisory** — Proactively signal available tools on phase transitions:

```rust
Live::builder()
    .tool_advisory(true)   // default: enabled
    .tool_advisory(false)  // disable proactive signaling
```

**Session Persistence** — Survive process restarts:

```rust
Live::builder()
    .persistence(Arc::new(FsPersistence::new("/tmp/sessions")))
    .session_id("user-123-session-456")
```

Built-in backends: `FsPersistence` (filesystem), `MemoryPersistence` (in-memory/tests). Implement `SessionPersistence` trait for custom backends (Redis, DynamoDB, etc.).

**Generation Complete Extraction** — Run extractors on generation complete (pre-truncation):

```rust
Live::builder()
    .extract_on_generation::<FullIntent>(llm, "Extract model's full intended response")
```

This captures the model's full output before interruption truncates it.

## S.C.T.P.M.A Operator Algebra

Six namespaces for composing agent configuration aspects:

| Namespace | Operator | Purpose | Key Methods |
|-----------|----------|---------|-------------|
| `S::` | `>>` | State transforms | `pick`, `rename`, `merge`, `flatten`, `set`, `defaults`, `drop`, `map`, `is_true`, `eq`, `one_of` |
| `C::` | `+` | Context engineering | `window`, `user_only`, `model_only`, `head`, `sample`, `truncate`, `exclude_tools`, `prepend`, `append`, `from_state`, `dedup`, `empty`, `filter`, `map` |
| `T::` | `\|` | Tool composition | `simple`, `function`, `google_search`, `url_context`, `code_execution`, `toolset` |
| `P::` | `+` | Prompt composition | `role`, `task`, `constraint`, `format`, `example`, `text`, `context`, `persona`, `guidelines`, `with_state`, `when`, `context_fn` |
| `M::` | `\|` | Middleware composition | (reserved) |
| `A::` | `+` | Artifact schemas | `output`, `input`, `json_output`, `json_input`, `text_output`, `text_input` |

Examples:

```rust
// State: pick + rename
let transform = S::pick(&["a", "b"]) >> S::rename(&[("a", "x")]);

// Context: window + user-only
let context = C::window(10) + C::user_only() + C::exclude_tools();

// Tools: combine functions with built-ins
let tools = T::simple("greet", "Greet", |_| async { Ok(json!({})) })
    | T::google_search()
    | T::code_execution();

// Prompt: compose sections
let prompt = P::role("analyst") + P::task("analyze data") + P::format("JSON");
let instruction: String = prompt.into();

// Artifacts: declare I/O schemas
let artifacts = A::json_output("report", "Analysis report")
    + A::text_input("source", "Source document");
```

## Key Types by Layer

### L0 (rs-genai) -- Wire Protocol

| Type | Purpose |
|------|---------|
| `SessionConfig` | Session setup configuration (model, voice, tools, VAD, etc.) |
| `SessionHandle` | Connected session -- implements `SessionWriter` + `SessionReader` |
| `SessionWriter` | Trait: send audio/text/video/tool responses |
| `SessionReader` | Trait: subscribe to events |
| `ConnectBuilder` | Ergonomic `ConnectBuilder::new(config).build()` |
| `Content` / `Part` / `Role` | Wire-format message types with builders (`Content::user()`, `Part::text()`) |
| `GeminiModel` | Enum of available models |
| `Voice` | Output voice selection |
| `Tool` / `FunctionDeclaration` | Tool declarations for setup message |
| `FunctionCall` / `FunctionResponse` | Tool call/response wire types |
| `SessionEvent` | Incoming events (audio, text, tool calls, etc.) |
| `Transport` / `TungsteniteTransport` | WebSocket transport trait + default impl |
| `Codec` / `JsonCodec` | Message encoding trait + default impl |
| `AuthProvider` / `VertexAIAuth` / `GoogleAIAuth` | Authentication providers |
| `Platform` | GoogleAI vs VertexAI URL/version logic |
| `VadConfig` / `VoiceActivityDetector` | Voice activity detection |
| `SpscRing` / `AudioJitterBuffer` | Lock-free audio buffers |
| `ApiEndpoint` | Connection endpoint configuration |
| `ResumeInfo` | Session resumption info: handle, resumable flag, last consumed index |
| `UsageInfo` | Token usage metadata: total, prompt, response token counts |

### L1 (rs-adk) -- Agent Runtime

| Type | Purpose |
|------|---------|
| `Agent` | Core trait: `name()` + `run_live()` |
| `LiveSessionBuilder` | Builder for callback-driven sessions |
| `LiveHandle` | Runtime handle: `send_audio/text`, `state()`, `telemetry()`, `extracted()` |
| `EventCallbacks` | All callback registrations (audio, text, tool, lifecycle) |
| `State` / `PrefixedState` / `StateKey<T>` | Concurrent typed key-value state with prefix scoping |
| `ToolFunction` / `SimpleTool` / `TypedTool` | Tool traits and implementations |
| `ToolDispatcher` | Routes function calls to registered tools |
| `TextAgent` | Trait for text-based agent pipelines |
| `LlmTextAgent` | Core text agent: generate -> tool dispatch -> loop |
| `SequentialTextAgent` / `ParallelTextAgent` | Agent combinators |
| `LoopTextAgent` / `FallbackTextAgent` / `RouteTextAgent` | More combinators |
| `RaceTextAgent` / `TimeoutTextAgent` / `MapOverTextAgent` | Advanced combinators |
| `TapTextAgent` / `DispatchTextAgent` / `JoinTextAgent` | Observation and async dispatch |
| `Phase` / `PhaseMachine` / `PhaseInstruction` | Declarative conversation phase management |
| `InstructionModifier` | State-reactive instruction composition |
| `Transition` / `TransitionResult` | Phase transition guards and results |
| `TurnExtractor` / `LlmExtractor` | OOB extraction pipeline |
| `TranscriptBuffer` / `TranscriptTurn` / `TranscriptWindow` | Conversation transcript tracking |
| `ComputedRegistry` / `ComputedVar` | Derived state variables |
| `Watcher` / `WatcherRegistry` | State change watchers |
| `TemporalPattern` / `TemporalRegistry` | Time/turn-based pattern detection |
| `SessionSignals` / `SessionTelemetry` | Auto-collected session metrics |
| `BaseLlm` / `GeminiLlm` | LLM abstraction for text agents |
| `TextAgentTool` | Wraps a TextAgent as a callable tool |
| `BackgroundAgentDispatcher` | Fire-and-forget agent dispatch |
| `SoftTurnDetector` | Proactive silence awareness for `proactiveAudio` sessions |
| `SteeringMode` | How phase machine steers: InstructionUpdate, ContextInjection, Hybrid |
| `NeedsFulfillment` / `RepairConfig` / `RepairAction` | Conversation repair protocol |
| `SessionPersistence` / `SessionSnapshot` | Session persistence trait and snapshot type |
| `FsPersistence` / `MemoryPersistence` | Built-in persistence backends |
| `ControlPlaneConfig` | Consolidated control plane settings for the processor |
| `ExtractionTrigger` | When to run extractors: EveryTurn, Interval, AfterToolCall, OnPhaseChange, OnGenerationComplete |

### L2 (adk-rs-fluent) -- Fluent DX

| Type | Purpose |
|------|---------|
| `AgentBuilder` | Copy-on-write immutable builder for agent construction |
| `Live` | Fluent builder for Live sessions |
| `PhaseBuilder` / `PhaseDefaults` | Sub-builders for phase configuration |
| `WatchBuilder` | Sub-builder for state watchers |
| `Composable` / `Pipeline` / `FanOut` / `Loop` / `Fallback` | Operator algebra nodes |
| `S` / `C` / `T` / `P` / `M` / `A` | Composition namespace modules |
| `let_clone!` | Macro to reduce Arc/clone boilerplate in closures |

## Three-Lane Processor Architecture

```
  SessionEvent (broadcast)
         |
    +----+----+
    |  Router  |   Zero-work dispatcher, NO state access on hot path
    +--+----+--+
       |    |
  +----+    +----+
  |              |
Fast Lane    Control Lane              Telemetry Lane
(sync <1ms)  (async, can block)        (own broadcast rx)
- on_audio   - on_tool_call            - SessionSignals (AtomicU64)
- on_text    - on_interrupted           - SessionTelemetry (atomic counters)
- on_vad_*   - Phase transitions        - Debounced 100ms flush
- on_input_  - Extractors (concurrent)  - Usage(UsageInfo)
  transcript - Watchers                 - GenerationComplete
             - Computed state           - SessionResumeUpdate(ResumeInfo)
             - Temporal patterns
             - TranscriptBuffer (owned, no mutex)
             - GenerationComplete extractors
             - Soft turn detection
             - Steering (context injection)
             - Tool advisory signaling
             - Conversation repair (nudge/escalate)
             - Session persistence (fire-and-forget)
```

## Development Commands

```bash
# Build the entire workspace
cargo build --workspace

# Run tests
cargo test --workspace

# Run a specific cookbook
cargo run -p ui

# Check without building
cargo check --workspace

# Run with specific features
cargo build -p rs-genai --features "vad,generate,tokens"
```

## Best Practices

- Import from `adk_rs_fluent::prelude::*` for application code -- it re-exports all three layers.
- Use `TypedTool` over `SimpleTool` when possible -- auto-generated schemas prevent drift.
- Use `State::modify()` for atomic read-modify-write instead of separate `get()` + `set()`.
- Use `StateKey<T>` constants for frequently accessed keys to prevent typos.
- Use `state.with()` for zero-copy borrows when you only need to inspect a value.
- Prefer `Live::builder()` (L2) over `LiveSessionBuilder::new()` (L1) for applications.
- Use `Content::user()` and `Content::model()` builders instead of constructing Content manually.
- Register agent tools via `.agent_tool()` to share session State with text agent pipelines.
- Use `.phase_defaults()` to DRY up modifiers shared across all phases.
- Use `.greeting("...")` to make the model speak first on connect.

## Common Mistakes

- **Wrong audio model**: Native audio model (`Gemini2_0FlashLive`) only supports `Modality::Audio` output, NOT `Modality::Text`. Use `.text_only()` for text-only mode with `Gemini2_0FlashLive`.
- **Vertex AI binary frames**: Vertex AI sends Binary WebSocket frames (not Text) -- handled automatically by `TungsteniteTransport`.
- **Vertex AI endpoint**: Use `wss://aiplatform.googleapis.com/...` (NOT `global-aiplatform.googleapis.com`).
- **API versions**: Google AI = `v1beta`, Vertex AI = `v1beta1` -- handled by `Platform` enum.
- **Cannot update tool definitions mid-session**: Voice sessions only allow instruction updates. Tool declarations are fixed at connect time.
- **Fast lane callbacks must be sync and under 1ms**: No allocations, no locks, no async in `on_audio`, `on_text`, `on_vad_*`.
- **Forgetting `.done()`**: Phase builder chains must end with `.done()` to return to the `Live` builder.
- **Forgetting `.initial_phase()`**: Phase machine requires an explicit initial phase name.
- **Using `instruction_template` with phases**: Template replaces the entire instruction -- use `instruction_amendment` or phase modifiers (`P::with_state`, `P::when`) for additive composition.
- **State prefix tax**: `state.get("risk")` auto-falls back to `derived:risk` -- no need to manually check both.

## Workspace Structure

```
crates/
  rs-genai/          L0 wire protocol (rs_genai)
  rs-adk/            L1 agent runtime (rs_adk)
  adk-rs-fluent/     L2 fluent DX (adk_rs_fluent)
cookbooks/
  ui/                Axum WebSocket tester with browser UI
  agents/            Agent composition examples
  voice-chat/        Voice chat cookbook
  tool-calling/      Tool calling cookbook
  transcription/     Transcription cookbook
  text-chat/         Text chat cookbook
tools/
  adk-transpiler/    Code transpilation utilities
```
