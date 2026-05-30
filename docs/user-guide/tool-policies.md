# Per-Tool Policies

Per-tool policies let you attach execution constraints to individual tools
without changing their implementation. They are expressed in the `T::` namespace
and composed with the same `|` operator as the rest of the tool algebra.

---

## PolicyTool

Internally, `T::timeout`, `T::cached`, and `T::confirm` all wrap the target
tool in a `PolicyTool` — a `ToolFunction` decorator that carries a `ToolPolicy`
and enforces it on every call.

```rust,ignore
pub struct ToolPolicy {
    pub timeout: Option<Duration>,
    pub cache: bool,
    pub confirm: bool,
    pub confirm_message: Option<String>,
}
```

Policies compose: wrapping the same tool twice (e.g.,
`T::cached(T::timeout(tool, dur))`) applies both timeout and cache.

---

## Timeout

`T::timeout(tool, duration)` races each call against `tokio::time::timeout`.
If the tool's future does not complete within the given duration the call
returns `ToolError::Timeout(duration)` and the inner future is dropped.

```rust,ignore
use std::time::Duration;

Live::builder()
    .with_tools(
        T::timeout(
            T::simple("search_kb", "Search the knowledge base", |args| async move {
                // potentially slow call
                Ok(call_search_api(args).await?)
            }),
            Duration::from_secs(10),
        )
        | T::google_search()
    )
```

Timeout enforcement is fully implemented: `tokio::time::timeout` wraps the
inner `call` future, and any elapse produces `ToolError::Timeout`.

---

## Caching

`T::cached(tool)` memoizes successful results keyed by `(tool name,
canonical-JSON args)`. Repeat calls with identical arguments return the cached
value without re-invoking the tool. Errors are never cached.

Canonical JSON sorts object keys lexicographically, so `{"b":2,"a":1}` and
`{"a":1,"b":2}` produce the same cache key.

```rust,ignore
// Weather results cached for the lifetime of the session
Live::builder()
    .with_tools(
        T::cached(T::simple("get_weather", "Get weather for a city", |args| async move {
            Ok(call_weather_api(&args["city"]).await?)
        }))
    )
```

Cache entries live for the lifetime of the `PolicyTool` instance (i.e., the
session). There is no TTL or maximum entry count. If you need time-bounded
caching, combine with a timeout and manage expiry in your tool implementation.

Caching is fully implemented in `PolicyTool::call`.

---

## Confirmation flag

`T::confirm(tool, message)` marks a tool as requiring user confirmation before
it runs. The flag and optional hint message are recorded on the `ToolPolicy`
and surfaced at runtime via `PolicyTool::requires_confirmation()`.

```rust,ignore
Live::builder()
    .with_tools(
        T::confirm(
            T::simple("send_email", "Send an email to the customer", |args| async move {
                Ok(do_send_email(args).await?)
            }),
            "This will send an email to the customer. Are you sure?",
        )
    )
```

### Enforcing confirmation with a provider

Confirmation is **enforced at dispatch** when you wire a `ConfirmationProvider`.
Before running any tool that reports `requires_confirmation()`, the
`ToolDispatcher` consults the provider; a denied decision returns
`ToolError::Cancelled` instead of executing the tool. Enforcement is **opt-in** —
with no provider configured, confirmation-gated tools run normally (the flag is
still surfaced via `requires_confirmation()`).

Wire one onto a dispatcher directly, or via `Live::confirmation_provider`:

```rust,ignore
use std::sync::Arc;

let handle = Live::builder()
    .with_tools(T::confirm(send_email_tool, "Send this email?"))
    // Any async closure `Fn(ConfirmationRequest) -> impl Future<Output = ToolConfirmation>`
    // works, or implement the `ConfirmationProvider` trait.
    .confirmation_provider(Arc::new(|req: ConfirmationRequest| async move {
        if approved_by_operator(&req.tool_name, &req.args).await {
            ToolConfirmation::confirmed()
        } else {
            ToolConfirmation::denied("operator rejected the action")
        }
    }))
    .connect_from_env()
    .await?;
```

For tests and simple defaults, `StaticConfirmation::allow_all()` /
`StaticConfirmation::deny_all("reason")` provide uniform providers. The same
`ToolDispatcher::with_confirmation_provider` / `set_confirmation_provider` API
gates tools in text-agent pipelines too.

---

## Async / Background Tool Execution

For tools that take significant time — database queries, external API calls,
LLM sub-pipelines — background execution eliminates dead air in voice sessions.

### ToolExecutionMode

```rust,ignore
pub enum ToolExecutionMode {
    Standard,   // tool runs inline; model waits for the result (default)
    Background {
        formatter: Option<Arc<dyn ResultFormatter>>,
        scheduling: Option<FunctionResponseScheduling>,
    },
}
```

### How background execution works

1. The model sends a `FunctionCall` for a background-declared tool.
2. An immediate "running" acknowledgment is sent to the model.
3. The tool is spawned as a Tokio task. The model continues speaking.
4. When the task completes, the result is injected into the conversation using
   the configured `FunctionResponseScheduling` mode.

### FunctionResponseScheduling modes

| Mode | Behaviour |
|------|-----------|
| `Interrupt` | Model halts current output and immediately handles the result |
| `WhenIdle` | Model waits until it finishes current output before handling (default) |
| `Silent` | Model integrates the result without notifying the user |

**Platform support**: async tool calling is only supported on **Google AI**.
On Vertex AI, `behavior: NonBlocking` is automatically stripped from
`FunctionDeclaration` setup messages and `scheduling` is stripped from
`FunctionResponse`. You can set these fields unconditionally; the SDK handles
the platform difference. Use `config.supports_async_tools()` to check at runtime.

### L2 fluent API

```rust,ignore
Live::builder()
    .tools(dispatcher)
    .tool_background("search_kb")                    // WhenIdle scheduling by default
    .tool_background_with_scheduling(
        "log_event",
        FunctionResponseScheduling::Silent,          // quiet integration
    )
    .connect_from_env()
    .await?;
```

### L1 builder API

```rust,ignore
LiveSessionBuilder::new(config)
    .dispatcher(dispatcher)
    .tool_execution_mode("search_kb", ToolExecutionMode::Background {
        formatter: None,
        scheduling: Some(FunctionResponseScheduling::WhenIdle),
    })
    .connect()
    .await?;
```

### Custom result formatting

Implement `ResultFormatter` to control the JSON shape of acknowledgment and
completion messages:

```rust,ignore
use gemini_adk_rs::live::background_tool::{ResultFormatter, DefaultResultFormatter};

struct VoiceFriendlyFormatter;

impl ResultFormatter for VoiceFriendlyFormatter {
    fn format_running(&self, call: &FunctionCall) -> Value {
        json!({ "status": "searching", "for": call.args["query"] })
    }

    fn format_result(&self, call: &FunctionCall, result: Result<Value, ToolError>) -> Value {
        match result {
            Ok(v)  => json!({ "status": "found",  "tool": call.name, "data": v }),
            Err(e) => json!({ "status": "failed", "tool": call.name, "error": e.to_string() }),
        }
    }

    fn format_cancelled(&self, call_id: &str) -> Value {
        json!({ "status": "cancelled", "id": call_id })
    }
}

// Register with custom formatter:
Live::builder()
    .tool_background_with_formatter("search_kb", Arc::new(VoiceFriendlyFormatter))
```

If `formatter` is `None`, `DefaultResultFormatter` is used, which produces:

```json
{ "status": "running",   "tool": "search_kb" }
{ "status": "completed", "tool": "search_kb", "result": { ... } }
{ "status": "error",     "tool": "search_kb", "error": "..." }
{ "status": "cancelled", "call_id": "fc_123" }
```

### Cancellation

Background tasks are cancelled when:
- The server sends `ToolCallCancellation`
- The session disconnects
- `LiveHandle` is dropped

`BackgroundToolTracker` provides belt-and-suspenders cleanup: both the
`CancellationToken` is triggered and the `JoinHandle` is aborted.

---

## Combining policies

Policies and execution modes compose freely:

```rust,ignore
Live::builder()
    .with_tools(
        // 10-second timeout + in-session cache
        T::cached(T::timeout(
            T::simple("get_stock_price", "Get a stock price", |args| async move {
                Ok(fetch_price(&args["ticker"]).await?)
            }),
            Duration::from_secs(10),
        ))
        // confirmation required on dangerous action
        | T::confirm(
            T::simple("cancel_order", "Cancel an order", |args| async move {
                Ok(do_cancel(&args["order_id"]).await?)
            }),
            "Cancel this order — are you sure?",
        )
        | T::google_search()
    )
    .tool_background("get_stock_price")  // also run it in background
```
