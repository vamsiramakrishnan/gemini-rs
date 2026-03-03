# Tool System

Tools let the model call your Rust functions during a live session. Gemini
sends a `FunctionCall`, your tool executes, and you return a `FunctionResponse`.

## SimpleTool

The quickest way to define a tool -- wrap an async closure:

```rust,ignore
use rs_adk::tool::SimpleTool;
use serde_json::json;

let weather = SimpleTool::new(
    "get_weather",
    "Get current weather for a city",
    Some(json!({
        "type": "object",
        "properties": {
            "city": { "type": "string", "description": "City name" }
        },
        "required": ["city"]
    })),
    |args| async move {
        let city = args["city"].as_str().unwrap_or("Unknown");
        Ok(json!({ "city": city, "temperature_c": 22, "condition": "Partly cloudy" }))
    },
);
```

The fourth argument is the JSON Schema for parameters. Pass `None` for
parameterless tools.

## TypedTool

Type-safe tools with auto-generated schemas. Define a struct with `JsonSchema`
and `Deserialize`:

```rust,ignore
use rs_adk::tool::TypedTool;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to get weather for
    city: String,
    /// Temperature units (celsius or fahrenheit)
    #[serde(default = "default_units")]
    units: String,
}
fn default_units() -> String { "celsius".to_string() }

let tool = TypedTool::new(
    "get_weather",
    "Get current weather for a city",
    |args: WeatherArgs| async move {
        Ok(serde_json::json!({ "temp": 22, "city": args.city, "units": args.units }))
    },
);
```

Doc comments on fields become parameter descriptions. Required vs optional is
inferred from `#[serde(default)]`. Invalid arguments return
`ToolError::InvalidArgs`.

## ToolFunction Trait

For full control, implement `ToolFunction` directly. Use this when your tool
holds state (connection pools, caches):

```rust,ignore
use async_trait::async_trait;
use rs_adk::tool::ToolFunction;
use rs_adk::error::ToolError;

struct DatabaseLookup { pool: sqlx::PgPool }

#[async_trait]
impl ToolFunction for DatabaseLookup {
    fn name(&self) -> &str { "lookup_account" }
    fn description(&self) -> &str { "Look up an account by ID" }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": { "account_id": { "type": "string" } },
            "required": ["account_id"]
        }))
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let id = args["account_id"].as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing account_id".into()))?;
        Ok(serde_json::json!({ "account_id": id, "balance": 4250.00 }))
    }
}
```

## StreamingTool

For tools that yield multiple results over time via an `mpsc::Sender`:

```rust,ignore
#[async_trait]
impl StreamingTool for ProgressTracker {
    fn name(&self) -> &str { "track_progress" }
    fn description(&self) -> &str { "Track a long-running operation" }
    fn parameters(&self) -> Option<serde_json::Value> { None }

    async fn run(
        &self,
        args: serde_json::Value,
        yield_tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<(), ToolError> {
        for step in 0..5 {
            yield_tx.send(json!({ "step": step })).await
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        }
        Ok(())
    }
}
```

Register via `dispatcher.register_streaming(Arc::new(tool))`.

## InputStreamingTool

For tools that receive live input (audio, video) while running. They get a
`broadcast::Receiver<InputEvent>` alongside the yield channel:

```rust,ignore
async fn run(
    &self,
    _args: serde_json::Value,
    mut input_rx: broadcast::Receiver<InputEvent>,
    yield_tx: mpsc::Sender<serde_json::Value>,
) -> Result<(), ToolError> {
    while let Ok(event) = input_rx.recv().await {
        // Process input events, yield partial results
    }
    Ok(())
}
```

## Built-in Tools

Gemini provides server-side tools requiring no implementation:

```rust,ignore
// Direct methods
Live::builder().google_search().code_execution().url_context()

// Or T:: composition with pipe operator
Live::builder().with_tools(T::google_search() | T::code_execution() | T::url_context())
```

## Agent as Tool

`TextAgentTool` wraps a text-mode agent as a callable tool for voice sessions.
The agent runs via `BaseLlm::generate()` and shares the session's `State`:

```rust,ignore
// Direct registration
let tool = TextAgentTool::new("verify_identity", "Verify caller", verifier, state.clone());
dispatcher.register(tool);

// Fluent API
Live::builder()
    .agent_tool("verify_identity", "Verify caller identity", verifier_agent)
    .agent_tool("calc_payment", "Calculate payment plans", calc_pipeline)
```

State sharing is bidirectional -- the text agent reads live-extracted values
and its mutations are visible to watchers and phase transitions.

## Tool Registration

**ToolDispatcher (L1)**

```rust,ignore
let mut dispatcher = ToolDispatcher::new();
dispatcher.register(my_tool);                        // impl ToolFunction
dispatcher.register_function(Arc::new(my_tool));     // Arc<dyn ToolFunction>
dispatcher.register_streaming(Arc::new(stream_tool));

Live::builder().tools(dispatcher).connect(config).await?;
```

**T:: composition (fluent API)**

```rust,ignore
Live::builder()
    .with_tools(
        T::function(Arc::new(weather_tool))
        | T::simple("calculate", "Evaluate expression", |args| async move {
            Ok(json!({"result": 42}))
        })
        | T::google_search()
    )
```

**Toolset from a vec**

```rust,ignore
let tools: Vec<Arc<dyn ToolFunction>> = vec![Arc::new(a), Arc::new(b), Arc::new(c)];
Live::builder().with_tools(T::toolset(tools))
```

## Tool Call Handling

The `on_tool_call` callback fires when the model requests tool execution.
Return `Some(responses)` to handle manually, or `None` for auto-dispatch:

```rust,ignore
.on_tool_call(|calls, state| async move {
    let responses: Vec<FunctionResponse> = calls.iter().map(|call| {
        let result = match call.name.as_str() {
            "get_weather" => execute_weather(&call.args),
            "verify_identity" => {
                let result = verify(&call.args);
                if result["verified"].as_bool() == Some(true) {
                    state.set("identity_verified", true);  // promote to state
                }
                result
            }
            _ => json!({"error": "unknown tool"}),
        };
        FunctionResponse { name: call.name.clone(), response: result, id: call.id.clone() }
    }).collect();
    Some(responses)
})
```

The callback receives `State` so you can promote tool results to keys that
drive phase transitions and watchers.

## Phase-Scoped Tools

Restrict available tools per conversation phase. The processor rejects calls
to tools not in the phase's `tools_enabled` list:

```rust,ignore
.phase("verify_identity")
    .instruction("Verify the caller's identity")
    .tools(vec!["verify_identity".into(), "log_compliance_event".into()])
    .transition("inform_debt", S::is_true("identity_verified"))
    .done()
.phase("negotiate")
    .instruction("Negotiate a payment plan")
    .tools(vec!["calculate_payment_plan".into()])
    .transition("arrange_payment", S::is_true("plan_agreed"))
    .done()
```

If `tools_enabled` is `None` (default), all registered tools are available.

## Long-Running Tools

`LongRunningFunctionTool` wraps any `ToolFunction` and tells the model not to
re-invoke while a previous call is pending:

```rust,ignore
use rs_adk::tools::LongRunningFunctionTool;

let long_running = LongRunningFunctionTool::new(Arc::new(MySlowTool::new()));
dispatcher.register(long_running);
```

The `ToolDispatcher` supports timeouts and cancellation:

```rust,ignore
// Custom timeout
dispatcher.call_function_with_timeout("slow_tool", args, Duration::from_secs(60)).await?;

// Cancel via token
dispatcher.call_function_with_cancel("slow_tool", args, cancel_token).await?;

// Configure default timeout (30s default)
let dispatcher = ToolDispatcher::new().with_timeout(Duration::from_secs(10));
```

## Intercepting Tool Responses

Transform tool results before they reach Gemini. Use for PII redaction,
state promotion, or result augmentation:

```rust,ignore
.before_tool_response(|responses, state| async move {
    responses.into_iter().map(|mut r| {
        if r.name == "verify_identity" {
            if r.response["verified"].as_bool() == Some(true) {
                state.set("identity_verified", true);
            }
        }
        if r.name == "lookup_account" {
            r.response = redact_pii(&r.response);
        }
        r
    }).collect()
})
```
