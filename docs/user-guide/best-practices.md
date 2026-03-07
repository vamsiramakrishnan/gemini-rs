# Best Practices & Common Mistakes

Practical guidance for building with the gemini-rs stack. Organized by category: architecture decisions, performance constraints, common pitfalls, and testing patterns.

## Architecture Best Practices

### Use the highest-level crate that fits your needs

The three-crate stack is layered for a reason. Reach for the highest level that covers your use case:

```
L2 (adk-rs-fluent)  -- Fluent DX, operator algebra, AgentBuilder, Live builder
L1 (rs-adk)         -- Agent runtime, tools, state, phases, TextAgent
L0 (rs-genai)       -- Wire protocol, transport, auth, raw WebSocket
```

For applications, start with L2. Drop to L1 if you need custom processor logic. Drop to L0 only for raw WebSocket access or custom transport implementations.

```rust,ignore
// Recommended for applications
use adk_rs_fluent::prelude::*;

// Only if building custom processors
use rs_adk::*;

// Only for raw wire access
use rs_genai::prelude::*;
```

### Use ContextInjection steering for multi-phase voice apps

Most multi-phase voice apps share a stable base persona across phases. Use `SteeringMode::ContextInjection` to set the persona once at connect and deliver phase-specific behavior as lightweight model-role context turns. This avoids the latency spike of system instruction replacement on every phase transition.

```rust,ignore
// Recommended for most apps
Live::builder()
    .instruction("You are a helpful restaurant reservation assistant.")
    .steering_mode(SteeringMode::ContextInjection)
    .phase("greeting")
        .instruction("Welcome the guest and ask how you can help.")
        .done()
    .phase("booking")
        .instruction("Help find an available time slot.")
        .done()
    .initial_phase("greeting")
```

Only use `InstructionUpdate` when phases represent genuinely different agent personas (e.g., switching from a receptionist to a triage nurse). See the [Steering Modes guide](steering-modes.md) for the full decision matrix and anti-patterns.

### Keep tool callbacks fast — or use background execution

The model waits for standard tool responses before continuing. A slow tool blocks the entire conversation turn. For tools that need to do expensive work (database queries, external API calls, LLM pipelines), you have two options:

1. **Set timeouts and cache** for tools that must complete before the model continues
2. **Use background execution** for tools where the model can continue speaking while results arrive

```rust,ignore
// Option 1: fast tool with timeout
let tool = SimpleTool::new("lookup", "Quick lookup", None, |args| async move {
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        db.query(&args["id"]),
    ).await
    .map_err(|_| ToolError::ExecutionFailed("Database timeout".into()))?;
    Ok(json!(result))
});

// Option 2: background execution — model gets an ack immediately
Live::builder()
    .tools(dispatcher)
    .tool_background("search_knowledge_base")  // zero dead-air
```

### Use concurrent callbacks for fire-and-forget work

Control-lane callbacks default to `Blocking` — the event loop waits for completion. For fire-and-forget work (logging, analytics, broadcasting to a UI), use `_concurrent` variants to avoid blocking the pipeline:

```rust,ignore
// Blocking: appropriate when ordering matters
.on_turn_complete(|| async { tx.send(TurnComplete).ok(); })

// Concurrent: fire-and-forget — doesn't block the next event
.on_extracted_concurrent(|name, val| async move {
    broadcast_to_ui(name, val).await;
})
.on_error_concurrent(|e| async move {
    send_to_error_tracker(&e).await;
})
.on_disconnected_concurrent(|reason| async move {
    info!("Disconnected: {reason:?}");
})
```

### Use State::modify() for atomic updates

`state.get()` followed by `state.set()` is a race condition under concurrent access. Use `modify()` for atomic read-modify-write:

```rust,ignore
// Bad: race condition
let count: u32 = state.get("count").unwrap_or(0);
state.set("count", count + 1);

// Good: atomic read-modify-write
let count = state.modify("count", 0u32, |n| n + 1);
```

### Extraction is out-of-band

Turn extractors run asynchronously after each turn completes. They do not block the model's response. This means:

- Extracted values may not be available immediately after a turn
- Do not rely on extraction results being instant for the next tool call
- Use watchers if you need to react when extracted values change

```rust,ignore
// Extraction runs asynchronously -- the model may start its next turn
// before extraction completes
handle.extracted::<OrderState>("OrderState"); // may return stale data briefly
```

### Phase transitions are reactive

Phase transitions fire on the next state check after the condition becomes true, not the instant state changes. This is by design -- it prevents mid-turn phase switching that would confuse the model.

```rust,ignore
// The transition predicate is checked after each turn, not continuously
.phase("greeting")
    .instruction("Welcome the user")
    .transition("main", S::is_true("greeted"))
    .done()
```

### Declare tools at session start

Voice sessions (Live API) do not support adding or removing tool definitions mid-session. All tools must be declared in the `SessionConfig` before connecting. Only instructions can be updated during the session.

```rust,ignore
// Tools declared at build time -- cannot change after connect
let handle = Live::builder()
    .tools(dispatcher)  // fixed for the session's lifetime
    .connect_vertex(project, location, token)
    .await?;

// Instructions CAN be updated mid-session
handle.update_instruction("New instruction text").await?;
```

### Use typed tools over simple tools

`TypedTool` auto-generates JSON Schema from your Rust struct via `schemars::JsonSchema`. This prevents schema drift and gives you compile-time type safety on arguments:

```rust,ignore
// Prefer TypedTool -- schema stays in sync with code
#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to get weather for
    city: String,
    /// Temperature unit (celsius or fahrenheit)
    unit: Option<String>,
}

let tool = TypedTool::new::<WeatherArgs>(
    "get_weather", "Get weather for a city",
    |args: WeatherArgs| async move {
        Ok(json!({"temp": 22, "city": args.city}))
    },
);
```

### Use `StateKey<T>` for frequently accessed keys

Compile-time typed keys prevent typos and give you type inference:

```rust,ignore
const TURN_COUNT: StateKey<u32> = StateKey::new("session:turn_count");
const RISK_LEVEL: StateKey<f64> = StateKey::new("derived:risk");

// Type-safe access -- no risk of typos or wrong types
state.set_key(&TURN_COUNT, 5);
let count: Option<u32> = state.get_key(&TURN_COUNT);
```

## Performance Best Practices

### Fast lane callbacks must complete in under 1ms

The three-lane processor architecture separates hot-path audio processing (fast lane) from control logic (control lane). Fast lane callbacks are synchronous and must not:

- Allocate heap memory
- Acquire locks or mutexes
- Perform async operations
- Make system calls

```rust,ignore
// Good: fast lane callback -- just forward to a channel
let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
.on_audio(move |data| { tx.send(data.clone()).ok(); })

// Bad: allocating and locking in the fast lane
.on_audio(move |data| {
    let processed = expensive_processing(data);  // too slow
    mutex.lock().push(processed);                 // blocks
})
```

### Use `Arc<dyn SessionWriter>` -- do not clone session handles

When you need to share the session writer across tasks, wrap it in `Arc`:

```rust,ignore
// Good: share via Arc
let writer: Arc<dyn SessionWriter> = handle.writer();
let writer_clone = writer.clone();
tokio::spawn(async move { writer_clone.send_text("hello").await; });

// Bad: cloning the entire handle
let handle_clone = handle.clone();  // unnecessary overhead
```

### Extractors run concurrently

Multiple turn extractors execute via `futures::future::join_all`, not sequentially. This means adding more extractors does not linearly increase latency -- they run in parallel.

```rust,ignore
// These three extractors run concurrently after each turn
Live::builder()
    .extract_turns::<Sentiment>(flash, "Extract emotional state")
    .extract_turns::<OrderInfo>(flash, "Extract order details")
    .extract_turns::<RiskScore>(flash, "Assess compliance risk")
```

### SessionSignals uses AtomicU64

`last_activity_ns` is tracked with atomic operations (~1ns overhead), not `Mutex<Instant>`. Telemetry counters use atomic CAS operations. This means telemetry collection has near-zero impact on the hot path.

## Common Mistakes

### Vertex AI sends Binary WebSocket frames

Vertex AI sends Binary frames, not Text frames. The `TungsteniteTransport` handles this transparently, but if you are debugging at the WebSocket level, do not expect text frames.

### Native audio model only supports AUDIO output

The `Gemini2_0FlashLive` model supports only `Modality::Audio` output, not `Modality::Text`. If you need text responses, use `.text_only()` on the builder, which sets `Modality::Text` explicitly:

```rust,ignore
// Voice output (default for live model)
Live::builder().model(GeminiModel::Gemini2_0FlashLive)
    // response_modalities defaults to [Audio]

// Text-only output
Live::builder().model(GeminiModel::Gemini2_0FlashLive).text_only()
    // response_modalities set to [Text]
```

### Cannot update tool definitions mid-session

This is a Gemini Live API constraint. Tools are declared once at session start. Only instructions can be updated. If you need different tools in different phases, declare all tools up front and use phase-scoped tool filtering:

```rust,ignore
// Declare ALL tools at build time
Live::builder()
    .tools(all_tools_dispatcher)
    .phase("greeting")
        .tools(vec![])  // no tools in greeting phase
        .done()
    .phase("main")
        .tools(vec!["search".into(), "lookup".into()])  // filter to these
        .done()
```

The processor rejects tool calls not in the current phase's `tools_enabled` list.

### Wrong Vertex AI endpoint

The global Vertex AI endpoint is `wss://aiplatform.googleapis.com/...`, NOT `wss://global-aiplatform.googleapis.com/...`. This is handled automatically by the `Platform` enum, but matters if you are constructing URLs manually.

### API version mismatch

Google AI uses `v1beta`, Vertex AI uses `v1beta1`. Again, the `Platform` enum handles this, but be aware when reading API docs.

### State prefix confusion

State keys can have prefixes: `session:`, `derived:`, `turn:`, `app:`, `bg:`, `user:`, `temp:`. When using `state.get("risk")`, the derived fallback automatically checks `derived:risk` if `risk` is not found. You do not need to manually check both:

```rust,ignore
// The derived fallback handles this automatically
state.set("derived:risk", 0.85);
assert_eq!(state.get::<f64>("risk"), Some(0.85));

// Use scoped accessors for clarity
state.derived().set("risk", 0.85);  // writes "derived:risk"
state.app().set("mode", "production");  // writes "app:mode"
state.turn().set("transcript", text);   // writes "turn:transcript" (cleared each turn)
```

### Forgetting to declare tools in SessionConfig

Tools must be declared at session start. If you register tools in the `ToolDispatcher` but do not include their declarations in the session config, the model will not know they exist.

### Blocking in on_audio callback

The `on_audio` callback runs on the fast lane. Blocking it stalls the entire audio pipeline:

```rust,ignore
// Bad: blocking the audio pipeline
.on_audio(|data| {
    std::thread::sleep(Duration::from_millis(10));  // stalls everything
})

// Good: non-blocking forward
let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
.on_audio(move |data| { tx.send(data.clone()).ok(); })
```

### Forgetting .done() on phase builders

Phase builder chains must end with `.done()` to return to the `Live` builder. Without it, you are still configuring the phase when you think you are configuring the session:

```rust,ignore
// Wrong: missing .done() -- next call configures the phase, not the session
Live::builder()
    .phase("greeting")
        .instruction("Welcome the user")
        // .done() is missing!
    .phase("main")  // this might not do what you expect
        .instruction("Handle the request")

// Correct
Live::builder()
    .phase("greeting")
        .instruction("Welcome the user")
        .done()  // returns to Live builder
    .phase("main")
        .instruction("Handle the request")
        .done()
```

### Forgetting .initial_phase()

The phase machine requires an explicit initial phase. Without it, no phase is active and phase-scoped instructions will not apply:

```rust,ignore
Live::builder()
    .phase("greeting").instruction("...").done()
    .phase("main").instruction("...").done()
    .initial_phase("greeting")  // required
```

### Using instruction_template with phases

`instruction_template` replaces the entire instruction, overwriting phase-specific instructions. For additive composition, use `instruction_amendment` or phase modifiers:

```rust,ignore
// Bad: template replaces everything, including phase instruction
.instruction_template(|state| format!("Context: {}", state.get::<String>("ctx").unwrap_or_default()))

// Good: amendment adds to the phase instruction
.instruction_amendment(|state| format!("\nContext: {}", state.get::<String>("ctx").unwrap_or_default()))

// Better: use P:: modifiers on phases
.phase("main")
    .instruction("Handle customer requests")
    .modifiers(vec![
        P::with_state(&["emotional_state"]),
        P::context_fn(|s| format!("Customer: {}", s.get::<String>("name").unwrap_or_default())),
    ])
    .done()
```

## Testing Patterns

### Use MockTransport for unit testing

`MockTransport` lets you test without real WebSocket connections. Inject scripted server responses:

```rust,ignore
use rs_genai::transport::MockTransport;

let mock = MockTransport::new(vec![
    // Scripted server messages
    ServerMessage::SetupComplete { ... },
    ServerMessage::ServerContent { ... },
]);

let (handle, _) = ConnectBuilder::new(config)
    .transport(mock)
    .build()
    .await?;
```

### State is cheap to construct

`State::new()` creates an empty concurrent map. Use it freely in tests:

```rust,ignore
#[tokio::test]
async fn test_my_agent() {
    let state = State::new();
    state.set("input", "test query");
    state.set("user:name", "Test User");

    let result = my_agent.run(&state).await.unwrap();
    assert!(result.contains("expected output"));

    // Verify state mutations
    assert_eq!(state.get::<bool>("processed"), Some(true));
}
```

### Test text agent pipelines with mock LLMs

Implement `BaseLlm` to create deterministic test fixtures:

```rust,ignore
struct MockLlm(String);

#[async_trait]
impl BaseLlm for MockLlm {
    fn model_id(&self) -> &str { "mock" }

    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            content: Content {
                role: Some(Role::Model),
                parts: vec![Part::Text { text: self.0.clone() }],
            },
            finish_reason: Some("STOP".into()),
            usage: None,
        })
    }
}

#[tokio::test]
async fn test_pipeline() {
    let llm: Arc<dyn BaseLlm> = Arc::new(MockLlm("mock output".into()));
    let agent = AgentBuilder::new("test")
        .instruction("Analyze this")
        .build(llm);

    let state = State::new();
    state.set("input", "test data");
    let result = agent.run(&state).await.unwrap();
    assert_eq!(result, "mock output");
}
```

### Test composable operators structurally

Verify the operator tree structure without running agents:

```rust,ignore
#[test]
fn pipeline_structure() {
    let pipeline = AgentBuilder::new("a") >> AgentBuilder::new("b") >> AgentBuilder::new("c");
    match pipeline {
        Composable::Pipeline(p) => assert_eq!(p.steps.len(), 3),
        _ => panic!("expected Pipeline"),
    }
}

#[test]
fn fan_out_structure() {
    let fan = AgentBuilder::new("x") | AgentBuilder::new("y");
    match fan {
        Composable::FanOut(f) => assert_eq!(f.branches.len(), 2),
        _ => panic!("expected FanOut"),
    }
}
```

### Test state transforms in isolation

`S::` transforms operate on `serde_json::Value` and can be tested without any agent infrastructure:

```rust,ignore
#[test]
fn state_transform_chain() {
    let chain = S::pick(&["name", "age"]) >> S::rename(&[("name", "customer")]);
    let mut state = json!({"name": "Alice", "age": 30, "internal": "x"});
    chain.apply(&mut state);
    assert_eq!(state, json!({"customer": "Alice", "age": 30}));
}
```

### Test context policies in isolation

`C::` policies operate on `Vec<Content>` slices:

```rust,ignore
#[test]
fn context_window() {
    let history = vec![
        Content::user("a"),
        Content::model("b"),
        Content::user("c"),
    ];
    let result = C::window(2).apply(&history);
    assert_eq!(result.len(), 2);
}
```
