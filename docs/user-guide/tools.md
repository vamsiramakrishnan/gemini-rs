# Tool System

Tools let the model call your Rust functions, on a text agent or in a live
session. The model sends a `FunctionCall`, your tool runs, and its result goes
back as a `FunctionResponse`.

## A tool is a documented function

Mark an `async fn` with `#[tool]`. Its doc comment is what the model reads: the
opening prose is the tool's description, and a `# Arguments` section describes
each parameter. The parameter types are the schema.

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

#[derive(serde::Serialize)]
struct Weather {
    city: String,
    celsius: f64,
    condition: String,
}

/// Get the current weather for a city.
///
/// # Arguments
///
/// * `city` - The city name, e.g. "Paris".
/// * `units` - "metric" or "imperial"; metric when omitted.
#[tool]
async fn get_weather(city: String, units: Option<String>) -> Result<Weather, reqwest::Error> {
    fetch_weather(&city, units.as_deref()).await
}

let agent = AgentBuilder::new("weather")
    .tool(get_weather())                     // a text agent
    .build(GeminiLlm::from_env()?)?;
let session = Live::builder().tool(get_weather());   // or a live session
```

What the macro checks and does:

- **Description.** The doc prose up to the first `#` heading. `#[tool("...")]`
  overrides it when the model should read something other than your readers
  do. A tool with neither does not compile.
- **Parameters.** Any owned `Deserialize + JsonSchema` type; `Option<T>` is
  optional. A `# Arguments` item naming a parameter that does not exist is a
  compile error, and so is a borrowed parameter (`&str`: use `String`).
- **Schema.** Produced by `gemini_adk_rs::tool::wire_schema`, the one pipeline
  every Rust type goes through on its way to the API: nested types are inlined
  and `Option<T>` declares a single type, both of which the API requires.
- **Return type.** Any `Serialize` type is the tool's output. A type spelled
  `Result<T, E>` (`anyhow::Result<T>`, `io::Result<T>`, …) is fallible with any
  error: a `ToolError` keeps its kind, anything else becomes
  `ToolError::ExecutionFailed` with its message. A result that is not a JSON
  object reaches the model as `{"output": …}`.
- **Attributes.** `#[cfg]` gates the whole tool; `#[allow]`,
  `#[tracing::instrument]` and the like stay on the body.

The macro emits a constructor, `get_weather()`, returning a value that
implements `ToolFunction`.

## Closures that capture: `T::typed`

A `#[tool]` function cannot capture a database pool or an HTTP client. For that,
`T::typed` takes a closure whose argument is a `JsonSchema` type — the same
schema pipeline, with the environment captured:

```rust,ignore
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Lookup {
    /// The customer's account id.
    account: String,
}

let db = pool.clone();
let balance = T::typed("balance", "Look up an account balance", move |args: Lookup| {
    let db = db.clone();
    async move { Ok(serde_json::json!({ "balance": db.balance(&args.account).await? })) }
});

AgentBuilder::new("support").tools(balance + T::google_search());
```

## Session context: `ToolContext`

A tool sometimes needs more than its arguments: an account id captured
earlier in the conversation, the call's id for a log line or an idempotency
key, or a way to stop when the caller barges in. The runtime passes every
call a `ToolContext` with three fields:

| Field | What it holds |
|---|---|
| `state` | The session `State`. Read what the conversation captured, write what the tool learned. |
| `call_id` | The model's id for this call, when it gave one. |
| `cancel` | A `CancellationToken`. On a Live session it fires when the user barges in on an inline call, and the runtime drops the call's future. A long tool can also check it between steps. |

A tool that wants the context asks for it. Add a `ToolContext` parameter to
a `#[tool]` function. The runtime fills it, and it is left out of the schema
the model sees:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

/// Look up the caller's balance.
///
/// # Arguments
///
/// * `currency` - ISO currency code for the answer.
#[tool]
async fn balance(currency: String, ctx: ToolContext) -> Result<serde_json::Value, ToolError> {
    let account: String = ctx.state.get("account_id").unwrap_or_default();
    Ok(serde_json::json!({ "account": account, "currency": currency }))
}
```

For a closure, use `T::contextual(name, description, |args, ctx| async move
{ .. })`. A hand-written `ToolFunction` overrides `call_with_context`. Its
default forwards to `call`, so existing tools are unchanged.

Called outside a session (`tool.call(args)` in a unit test), a tool gets
`ToolContext::detached()`: empty state, no call id, and a token that never
fires. To test with context, build one with `ToolContext::new(state)
.with_call_id("call-1")` and call `call_with_context`, or call
`ToolDispatcher::call_function_in(name, args, ctx)`.

`ToolContext` in the fluent prelude is this type,
`gemini_adk_rs::tool::ToolContext`. The invocation-context wrapper that had
the name in 2.x stays at `gemini_adk_rs::context::ToolContext`.

## Lower-level forms

`TypedTool::new::<Args>(name, description, closure)` is what `T::typed` builds,
for use with a `ToolDispatcher` directly. `SimpleTool::new(name, description,
schema, closure)` takes a hand-written JSON Schema and raw JSON arguments;
`T::simple(name, description, closure)` is the parameterless form, declaring
no parameters to the model.

## ToolFunction Trait

For full control, implement `ToolFunction` directly. Use this when your tool
holds state (connection pools, caches):

```rust,ignore
use async_trait::async_trait;
use gemini_adk_rs::tool::ToolFunction;
use gemini_adk_rs::error::ToolError;

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
Live::builder().tools(T::google_search() + T::code_execution() + T::url_context())
```

## Per-Tool Policies

Attach execution constraints to individual tools using the `T::` policy
wrappers. Policies compose with `|` like any other tool entry.

```rust,ignore
Live::builder()
    .tools(
        // 10-second timeout on a slow tool
        T::timeout(
            search_kb(),                   // a #[tool] fn
            Duration::from_secs(10),
        )
        // In-session result cache
        + T::cached(get_rate())
        // Confirmation flag (recorded; see note in tool-policies.md)
        + T::confirm(send_email(), "This will send a real email — are you sure?")
    )
```

- **`T::timeout(tool, duration)`** — enforced: `tokio::time::timeout` wraps the
  call; elapse returns `ToolError::Timeout`.
- **`T::cached(tool)`** — enforced: memoizes successful results by
  `(name, canonical-JSON args)`; errors are not cached.
- **`T::confirm(tool, message)`** — enforced at dispatch by a
  `ConfirmationProvider` (`Live::confirmation_provider`,
  `AgentBuilder::confirmation_provider`); a declined call returns
  `ToolError::Declined(reason)` to the model and the tool never runs. A
  session or agent with a confirm-gated tool and no provider is refused at
  `connect`/`build` (and reported by `check_live`), rather than running the
  tool unconfirmed. See [Per-Tool Policies](./tool-policies.md).

For async/background execution (`ToolExecutionMode::Background`,
`FunctionResponseScheduling`) and MCP tool integration, see the dedicated
chapters:

- [Per-tool policies](tool-policies.md) — full reference for timeout, cache,
  confirm, and background scheduling.
- [MCP Tools](mcp-tools.md) — connecting to Model Context Protocol servers.

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

Live::builder().dispatcher(dispatcher).connect(config).await?;
```

**T:: composition (fluent API)**

```rust,ignore
Live::builder()
    .tools(
        T::function(Arc::new(weather_tool))
        | calculate()                         // a #[tool] fn converts directly
        + T::google_search()
    )
```

**Toolset from a vec**

```rust,ignore
let tools: Vec<Arc<dyn ToolFunction>> = vec![Arc::new(a), Arc::new(b), Arc::new(c)];
Live::builder().tools(T::toolset(tools))
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
use gemini_adk_rs::tools::LongRunningFunctionTool;

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

## Background Tool Execution

For tools that take significant time (database queries, API calls, LLM pipelines),
background execution eliminates dead air in voice sessions.

### How It Works

1. Model calls a background tool
2. An immediate "running" acknowledgment is sent back
3. The model continues speaking (e.g., "Let me look that up for you...")
4. When the tool completes, the result is injected into the conversation
5. The model incorporates the result naturally

### L2 API

```rust,ignore
Live::builder()
    .dispatcher(dispatcher)
    .tool_background("search_knowledge_base")
    .tool_background_with_formatter("analyze_doc", Arc::new(MyFormatter))
    .connect_vertex(project, location, token)
    .await?;
```

### L1 API

```rust,ignore
LiveSessionBuilder::new(config)
    .dispatcher(dispatcher)
    .tool_execution_mode("search_knowledge_base", ToolExecutionMode::Background {
        formatter: None,
        scheduling: Some(FunctionResponseScheduling::WhenIdle),
    })
    .connect()
    .await?;
```

### Custom Result Formatting

Implement `ResultFormatter` to control acknowledgment and result shapes:

```rust,ignore
struct VerboseFormatter;

impl ResultFormatter for VerboseFormatter {
    fn format_running(&self, call: &FunctionCall) -> Value {
        json!({ "status": "searching", "query": call.args["query"] })
    }

    fn format_result(&self, call: &FunctionCall, result: Result<Value, ToolError>) -> Value {
        match result {
            Ok(val) => json!({ "status": "done", "tool": call.name, "result": val }),
            Err(e) => json!({ "status": "error", "tool": call.name, "error": e.to_string() }),
        }
    }

    fn format_cancelled(&self, call_id: &str) -> Value {
        json!({ "status": "cancelled", "call_id": call_id })
    }
}
```

### Cancellation

Background tools are automatically cancelled when:
- The server sends `ToolCallCancellation`
- The session disconnects
- `LiveHandle` is dropped

The `BackgroundToolTracker` provides belt-and-suspenders cleanup: both
the `CancellationToken` is triggered and the `JoinHandle` is aborted.

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

## See also

- [Per-Tool Policies](./tool-policies.md) — timeout, caching, confirmation, and background execution
- [MCP Tools](./mcp-tools.md) — connecting to Model Context Protocol servers
- [Text Agent Combinators](./text-agents.md) — using `TextAgentTool` to call agent pipelines as tools
- [cookbook 02 — agent with tools](../../examples/cookbook/src/01_foundations.rs)
- [cookbook 09 — tool composition](../../examples/cookbook/src/03_composition.rs)
```
