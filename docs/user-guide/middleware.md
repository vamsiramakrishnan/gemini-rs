# Middleware & Processors

Middleware wraps the agent lifecycle (before/after execution, tool calls,
errors). Processors transform LLM requests and responses in flight. For live
voice sessions, most interception uses `EventCallbacks` instead -- middleware
and processors are primarily for text-mode agent pipelines.

## Middleware Trait

Implement `Middleware` to hook into agent and tool lifecycle events. All
methods are optional -- implement only what you need:

```rust,ignore
use async_trait::async_trait;
use gemini_adk_rs::middleware::Middleware;
use gemini_adk_rs::error::{AgentError, ToolError};
use gemini_genai_rs::prelude::FunctionCall;

struct AuditMiddleware {
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Middleware for AuditMiddleware {
    fn name(&self) -> &str { "audit" }

    async fn before_agent(&self, ctx: &InvocationContext) -> Result<(), AgentError> {
        self.log.lock().push("Agent started".into());
        Ok(())
    }

    async fn after_agent(&self, ctx: &InvocationContext) -> Result<(), AgentError> {
        self.log.lock().push("Agent completed".into());
        Ok(())
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        self.log.lock().push(format!("Tool '{}' called", call.name));
        Ok(())
    }

    async fn after_tool(
        &self, call: &FunctionCall, result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        self.log.lock().push(format!("Tool '{}' returned", call.name));
        Ok(())
    }

    async fn on_tool_error(
        &self, call: &FunctionCall, err: &ToolError,
    ) -> Result<(), AgentError> {
        self.log.lock().push(format!("Tool '{}' failed: {err}", call.name));
        Ok(())
    }

    async fn on_error(&self, err: &AgentError) -> Result<(), AgentError> {
        self.log.lock().push(format!("Agent error: {err}"));
        Ok(())
    }
}
```

Returning `Err` from any hook aborts the pipeline.

## MiddlewareChain

Compose multiple middleware into an ordered chain. `before_*` hooks run in
registration order; `after_*` hooks run in reverse order (unwinding):

```rust,ignore
use gemini_adk_rs::middleware::MiddlewareChain;

let mut chain = MiddlewareChain::new();
chain.add(Arc::new(LogMiddleware::new()));
chain.add(Arc::new(LatencyMiddleware::new()));
chain.add(Arc::new(RetryMiddleware::new(3)));

// Insert at front
chain.prepend(Arc::new(SecurityMiddleware::new()));

assert_eq!(chain.len(), 4);
```

## RequestProcessor

Transform outbound requests before they reach the LLM:

```rust,ignore
use async_trait::async_trait;
use gemini_adk_rs::processors::{RequestProcessor, ProcessorError};
use gemini_adk_rs::llm::LlmRequest;

struct ContextInjector { context: String }

#[async_trait]
impl RequestProcessor for ContextInjector {
    fn name(&self) -> &str { "context_injector" }

    async fn process_request(
        &self, mut request: LlmRequest,
    ) -> Result<LlmRequest, ProcessorError> {
        match &mut request.system_instruction {
            Some(existing) => { existing.push_str("\n\n"); existing.push_str(&self.context); }
            None => { request.system_instruction = Some(self.context.clone()); }
        }
        Ok(request)
    }
}
```

## ResponseProcessor

Transform inbound responses after they come from the LLM:

```rust,ignore
struct ResponseSanitizer;

#[async_trait]
impl ResponseProcessor for ResponseSanitizer {
    fn name(&self) -> &str { "sanitizer" }

    async fn process_response(
        &self, mut response: LlmResponse,
    ) -> Result<LlmResponse, ProcessorError> {
        for part in &mut response.content.parts {
            if let gemini_genai_rs::prelude::Part::Text { text } = part {
                *text = text.replace("```", "");
            }
        }
        Ok(response)
    }
}
```

## Built-in Processors

**InstructionInserter** -- prepends or appends a system instruction:

```rust,ignore
use gemini_adk_rs::processors::InstructionInserter;

let inserter = InstructionInserter::new("Always respond in JSON format.");
let processed = inserter.process_request(request).await?;
// Appends to existing instruction if one is already set
```

**ContentFilter** -- filters content parts by type:

```rust,ignore
use gemini_adk_rs::processors::ContentFilter;

let filter = ContentFilter::text_only();
// Removes inline images, audio -- keeps only text parts
```

## Processor Chains

Chain multiple processors into a pipeline:

```rust,ignore
use gemini_adk_rs::processors::RequestProcessorChain;

let mut chain = RequestProcessorChain::new();
chain.add(InstructionInserter::new("Be concise."));
chain.add(InstructionInserter::new("Respond in English."));
chain.add(ContentFilter::text_only());

let processed = chain.process(request).await?;
// system_instruction = "Be concise.\nRespond in English."
```

`ResponseProcessorChain` works the same way for responses.

## Built-in Middleware

**LogMiddleware** -- structured logging via `tracing` (requires
`tracing-support` feature):

```rust,ignore
let log = LogMiddleware::new();
// Logs: agent starting/completed, tool call starting/completed/failed, errors
```

**LatencyMiddleware** -- records wall-clock timing for tool calls:

```rust,ignore
let latency = Arc::new(LatencyMiddleware::new());
chain.add(latency.clone());

// After some tool calls...
for record in latency.tool_latencies() {
    println!("{}: {:?} (success={})", record.name, record.elapsed, record.success);
}
latency.clear();  // reset for next window
```

**RetryMiddleware** -- advisory retry tracking. Counts errors and exposes
`should_retry()`:

```rust,ignore
let retry = Arc::new(RetryMiddleware::new(3));
chain.add(retry.clone());

// After running the agent...
if retry.should_retry() {
    retry.record_attempt();
    // re-run the agent
}
retry.reset();  // reuse for another run
```

`RetryMiddleware` does not automatically retry -- it tracks errors and the
caller decides.

## Custom Middleware Example

A rate-limiting middleware that tracks tool call frequency:

```rust,ignore
struct RateLimitMiddleware {
    max_per_minute: u32,
    count: AtomicU32,
    window_start: parking_lot::Mutex<Instant>,
}

#[async_trait]
impl Middleware for RateLimitMiddleware {
    fn name(&self) -> &str { "rate_limit" }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        let mut start = self.window_start.lock();
        if start.elapsed() > Duration::from_secs(60) {
            *start = Instant::now();
            self.count.store(0, Ordering::SeqCst);
        }
        let n = self.count.fetch_add(1, Ordering::SeqCst);
        if n >= self.max_per_minute {
            return Err(AgentError::Other("Rate limit exceeded".into()));
        }
        Ok(())
    }
}
```

## Middleware vs Callbacks

| Use case | Mechanism |
|----------|-----------|
| Log every tool call | `Middleware` (`before_tool` / `after_tool`) |
| Track tool latency | `LatencyMiddleware` |
| Handle tool results in live session | `on_tool_call` callback |
| Transform LLM requests | `RequestProcessor` |
| Inject context at turn boundaries | `on_turn_boundary` callback |
| React to extracted state changes | `watch()` watcher |
| Intercept tool responses before Gemini | `before_tool_response` callback |
| Retry failed agent runs | `RetryMiddleware` |

In live voice sessions, most interception uses `EventCallbacks` because the
session runs over a persistent WebSocket. Middleware and processors are for
text-mode agent pipelines where request/response cycles are explicit.
