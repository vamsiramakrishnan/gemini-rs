# Text Agent Combinators

Compose request/response agents with the `TextAgent` interface. Sequential,
parallel, branching, loop, and fallback combinators determine when each agent
runs and where its result goes. Text generation does not require a Live
WebSocket session or local audio dependencies.

## Asking, chatting, streaming

Every text agent answers the same four calls:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let agent = AgentBuilder::new("assistant")
    .instruction("Be brief.")
    .build(GeminiLlm::from_env()?)?;

// One question, one answer.
let answer: String = agent.ask("What is a tokio runtime?").await?;

// A typed answer: the type's JSON Schema is the response schema, and a reply
// that does not parse is sent back once for correction.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Summary { title: String, bullets: Vec<String> }
let summary: Summary = agent.ask_as("Summarize the Rust ownership model.").await?;

// A conversation that remembers its turns.
let mut chat = agent.chat();
chat.send("My name is Ada.").await?;
let reply = chat.send("What is my name?").await?;

// The reply as it is written: text deltas, tool calls and results, then the
// finished result.
let mut events = agent.stream("Tell me a short story.");
while let Some(event) = events.next().await {
    match event? {
        RunEvent::TextDelta(text) => print!("{text}"),
        RunEvent::ToolCall { name, .. } => eprintln!("[calling {name}]"),
        RunEvent::Finished(result) => eprintln!("\n[{} tokens]", result.usage.total_tokens),
        _ => {}
    }
}
```

`AgentBuilder::output::<T>()` fixes the response schema for every call, and
`RunResult::parse::<T>()` reads a reply back as `T`.

When an agent has middleware — which may rewrite a reply, for example to
redact it — each model turn is streamed only after the middleware has seen it.
Without middleware, text streams as the model writes.

### One error type for application code

Each layer returns its own precise error: `AgentError` for agents and Live
sessions, `LlmError` for a model call, `ConversationError` from the
conversation compiler, `PersistenceError` for snapshots. A function that
uses several of them can return `gemini_adk::Result` from the `gemini-adk`
facade crate. Its `Error` converts from each layer's error with `?` and
keeps the original as its `source()`:

```rust,ignore
use gemini_adk::prelude::*;

async fn run() -> gemini_adk::Result<()> {
    let agent = AgentBuilder::new("assistant")
        .instruction("Answer in one sentence.")
        .build(GeminiLlm::from_env()?)?; // an LlmError, then an AgentError
    println!("{}", agent.ask("Hello").await?);
    let notes = std::fs::read_to_string("notes.txt")?; // an io::Error
    let _ = notes;
    Ok(())
}
```

`Error::is_retryable()` is true for a rate limit, a transient provider or
transport failure, or a timeout, and false for configuration, spec or input
errors. `Error::llm()` returns the model call's `LlmError` when that was the
failure.

## The TextAgent Trait

```rust,ignore
#[async_trait]
pub trait TextAgent: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, state: &State) -> Result<String, AgentError>;

    // Provided:
    async fn run_with(&self, request: RunRequest, state: &State) -> Result<RunResult, AgentError>;
    fn run_stream(&self, request: RunRequest, state: State) -> BoxStream<'_, Result<RunEvent, AgentError>>;
    // ask, ask_as, chat and stream are built on these.
}
```

`run_with` is the primitive: a `RunRequest` carries the new turn (text or
media), the conversation before it and an optional response schema; a
`RunResult` reports the reply, the turns to append to the history, token usage,
every tool call and the number of model calls. `LlmTextAgent` implements it
natively; any other agent gets it through `run`.

`run` is the form the combinators below use. `State` is a concurrent typed
key-value store shared across a pipeline: an agent reads its prompt from the
`"input"` key and writes its reply to `"output"`. `Arc<A>`, `Box<A>` and `&A`
are agents whenever `A` is, so a built `Arc<dyn TextAgent>` can go straight
into any combinator.

## Combinator Reference

| Combinator | Purpose | Analogy |
|---|---|---|
| `LlmTextAgent` | Core agent -- generate, tool dispatch, loop | `Gemini(prompt)` |
| `FnTextAgent` | Wrap a closure as an agent (no LLM call) | `\|state\| { ... }` |
| `SequentialTextAgent` | Run agents in order, pipe output forward | `A >> B >> C` |
| `ParallelTextAgent` | Run agents concurrently, collect all results | `[A, B, C]` |
| `RaceTextAgent` | Run concurrently, return first success | `A \| B \| C` |
| `RouteTextAgent` | Route to agent based on state predicate | `if X -> A, if Y -> B` |
| `FallbackTextAgent` | Try agents in order until one succeeds | `A ?? B ?? C` |
| `LoopTextAgent` | Repeat until max iterations or predicate | `while(!done) { A }` |
| `MapOverTextAgent` | Apply agent to each item in a state list | `items.map(A)` |
| `TapTextAgent` | Read-only side effect (logging, metrics) | `tap(log)` |
| `TimeoutTextAgent` | Wrap an agent with a time limit | `timeout(5s, A)` |
| `DispatchTextAgent` | Fire-and-forget background tasks | `spawn(A, B)` |
| `JoinTextAgent` | Wait for dispatched tasks to complete | `join(tasks)` |

## LlmTextAgent -- The Core Agent

`LlmTextAgent` is the workhorse. It calls `BaseLlm::generate()`, dispatches any tool calls the model makes, feeds tool results back, and loops until the model produces a final text response (up to 10 rounds).

```rust,ignore
use gemini_adk_rs::text::LlmTextAgent;
use gemini_adk_rs::llm::{GeminiLlm, GeminiLlmParams};

// Reads GEMINI_API_KEY (or Vertex AI settings) and GEMINI_TEXT_MODEL, and
// fails here — naming the variable to set — if the configuration is incomplete.
let llm = GeminiLlm::from_env()?;

let agent = LlmTextAgent::new("analyst", llm)
    .instruction("Analyze the given topic and produce a summary.")
    .model("gemini-2.5-pro")
    .temperature(0.3)
    .max_output_tokens(2048)
    .tools(Arc::new(tool_dispatcher));

println!("{}", agent.ask("Explain Rust's ownership model").await?);
```

## FnTextAgent -- Zero-Cost Transforms

When you need a pipeline step that does not call an LLM -- data formatting, validation, state manipulation -- use `FnTextAgent`:

```rust,ignore
use gemini_adk_rs::text::FnTextAgent;

let formatter = FnTextAgent::new("format_output", |state| {
    let raw = state.get::<String>("input").unwrap_or_default();
    let formatted = format!("## Summary\n\n{raw}");
    state.set("output", &formatted);
    Ok(formatted)
});
```

## Building Pipelines

### Sequential: A >> B >> C

Each agent's output becomes the next agent's input via `state.set("input", &output)`. The final agent's output is the pipeline result.

```rust,ignore
use gemini_adk_rs::text::SequentialTextAgent;

let pipeline = SequentialTextAgent::new("analysis_pipeline", vec![
    Arc::new(LlmTextAgent::new("extract", llm.clone())
        .instruction("Extract key claims from the text.")),
    Arc::new(LlmTextAgent::new("validate", llm.clone())
        .instruction("Fact-check each claim. Flag unsupported ones.")),
    Arc::new(LlmTextAgent::new("summarize", llm.clone())
        .instruction("Produce a final summary with confidence ratings.")),
]);

let state = State::new();
state.set("input", raw_document);
let summary = pipeline.run(&state).await?;
```

### Parallel: Run concurrently, collect all

All branches execute concurrently via `tokio::spawn`. Results are joined with newlines.

```rust,ignore
use gemini_adk_rs::text::ParallelTextAgent;

let multi_perspective = ParallelTextAgent::new("perspectives", vec![
    Arc::new(LlmTextAgent::new("technical", llm.clone())
        .instruction("Analyze from a technical perspective.")),
    Arc::new(LlmTextAgent::new("business", llm.clone())
        .instruction("Analyze from a business perspective.")),
    Arc::new(LlmTextAgent::new("legal", llm.clone())
        .instruction("Analyze from a legal perspective.")),
]);
```

### Race: First to finish wins

Like `ParallelTextAgent`, but returns only the first result and cancels the rest. Useful for redundancy or trying multiple model configurations:

```rust,ignore
use gemini_adk_rs::text::RaceTextAgent;

let fastest = RaceTextAgent::new("race", vec![
    Arc::new(LlmTextAgent::new("fast", fast_llm.clone())
        .instruction("Answer the question.")),
    Arc::new(LlmTextAgent::new("thorough", slow_llm.clone())
        .instruction("Answer the question in depth.")),
]);
```

### Route: State-driven branching

Evaluate predicates against state. First match wins, with a default fallback:

```rust,ignore
use gemini_adk_rs::text::{RouteTextAgent, RouteRule};

let router = RouteTextAgent::new(
    "issue_router",
    vec![
        RouteRule::new(
            |s| s.get::<String>("category") == Some("billing".into()),
            Arc::new(billing_agent),
        ),
        RouteRule::new(
            |s| s.get::<String>("category") == Some("technical".into()),
            Arc::new(tech_agent),
        ),
    ],
    Arc::new(general_agent), // default
);
```

### Fallback: Try until one succeeds

Attempts each candidate in order. Returns the first `Ok` result. If all fail, returns the last error:

```rust,ignore
use gemini_adk_rs::text::FallbackTextAgent;

let robust = FallbackTextAgent::new("robust_lookup", vec![
    Arc::new(primary_agent),
    Arc::new(cache_agent),
    Arc::new(fallback_agent),
]);
```

### Loop: Repeat with termination

Runs the body up to `max` times, optionally breaking early when a state predicate returns `true`:

```rust,ignore
use gemini_adk_rs::text::LoopTextAgent;

let refiner = LoopTextAgent::new("refine", Arc::new(draft_agent), 5)
    .until(|state| {
        state.get::<String>("quality")
            .map(|q| q == "good")
            .unwrap_or(false)
    });
```

### MapOver: Apply agent to each item

Reads a list from state, runs the agent once per item, collects results:

```rust,ignore
use gemini_adk_rs::text::MapOverTextAgent;

let processor = MapOverTextAgent::new("process_items", Arc::new(item_agent), "items")
    .item_key("current_item")
    .output_key("processed_results");

// State must contain: state.set("items", vec!["item1", "item2", "item3"]);
```

### Tap: Observe without mutation

For logging, metrics, or debugging. Returns an empty string and does not mutate the pipeline flow:

```rust,ignore
use gemini_adk_rs::text::TapTextAgent;

let logger = TapTextAgent::new("log_state", |state| {
    let input = state.get::<String>("input").unwrap_or_default();
    tracing::info!("Pipeline state - input length: {}", input.len());
});
```

### Timeout: Time-limited execution

Wraps any agent with a deadline. Returns `AgentError::Timeout` if exceeded:

```rust,ignore
use gemini_adk_rs::text::TimeoutTextAgent;

let bounded = TimeoutTextAgent::new(
    "bounded_analysis",
    Arc::new(slow_agent),
    Duration::from_secs(30),
);
```

### Dispatch + Join: Background tasks

`DispatchTextAgent` spawns agents as background tasks with a concurrency budget. `JoinTextAgent` waits for them:

```rust,ignore
use gemini_adk_rs::text::{DispatchTextAgent, JoinTextAgent, TaskRegistry};

let registry = TaskRegistry::new();
let budget = Arc::new(tokio::sync::Semaphore::new(4)); // max 4 concurrent

let dispatcher = DispatchTextAgent::new(
    "spawn_tasks",
    vec![
        ("research".into(), Arc::new(research_agent) as Arc<dyn TextAgent>),
        ("analysis".into(), Arc::new(analysis_agent) as Arc<dyn TextAgent>),
    ],
    registry.clone(),
    budget,
);

let joiner = JoinTextAgent::new("collect", registry)
    .timeout(Duration::from_secs(60));

// In a pipeline: dispatch, do other work, then join
let pipeline = SequentialTextAgent::new("bg_pipeline", vec![
    Arc::new(dispatcher),
    Arc::new(other_work_agent),
    Arc::new(joiner),
]);
```

## Agent as Tool: Bridging Voice and Text

`TextAgentTool` wraps any `TextAgent` as a `ToolFunction`, so the live voice model can dispatch text agent pipelines as tool calls. State is shared bidirectionally -- the text agent reads live-extracted values, and its mutations are visible to watchers and phase transitions.

```rust,ignore
use gemini_adk_rs::text_agent_tool::TextAgentTool;

// Build a multi-step verification pipeline
let verifier = SequentialTextAgent::new("verify_pipeline", vec![
    Arc::new(LlmTextAgent::new("lookup", flash_llm.clone())
        .instruction("Look up the account in the database")
        .tools(Arc::new(db_tools))),
    Arc::new(LlmTextAgent::new("cross_ref", flash_llm.clone())
        .instruction("Cross-reference identity against account record")),
]);

// Wrap as a tool for the voice session
let tool = TextAgentTool::new(
    "verify_identity",
    "Verify caller identity against account records",
    verifier,
    state.clone(), // shared state with the live session
);

dispatcher.register_function(Arc::new(tool));
```

When the voice model calls `verify_identity`, the entire sequential pipeline runs via `BaseLlm::generate()` (not over WebSocket), and the result is returned as the tool response.

## Fluent Operator Algebra

If you use the `gemini-adk-fluent-rs` crate (L2), you get operator syntax for composing agents:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

// Sequential pipeline: >>
let pipeline = AgentBuilder::new("writer").instruction("Write a draft")
    >> AgentBuilder::new("reviewer").instruction("Review and improve");

// Parallel fan-out: |
let analysis = AgentBuilder::new("tech").instruction("Technical analysis")
    | AgentBuilder::new("business").instruction("Business analysis");

// Fixed loop: * N
let polished = AgentBuilder::new("refiner").instruction("Polish the text") * 3;

// Conditional loop: * until(predicate)
let converge = AgentBuilder::new("iterate").instruction("Improve")
    * until(|v| v["quality"].as_str() == Some("good"));

// Fallback chain: /
let robust = AgentBuilder::new("primary").instruction("Try this first")
    / AgentBuilder::new("backup").instruction("Fall back to this");

// Compile the tree into an executable TextAgent
let agent = pipeline.compile(llm)?;
let result = agent.run(&state).await?;
```

## Real-World Pattern: Multi-Step Analysis Pipeline

Here is a complete pipeline that extracts claims from a document, validates them in parallel, and produces a confidence-rated summary:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let extract = AgentBuilder::new("extract")
    .instruction("Extract all factual claims from the text. Output one claim per line.")
    .temperature(0.1);

let validate = AgentBuilder::new("validate")
    .instruction("For each claim, determine if it is supported, unsupported, or misleading.")
    .google_search()
    .temperature(0.2);

let summarize = AgentBuilder::new("summarize")
    .instruction("Produce a final report with confidence ratings for each claim.")
    .temperature(0.3);

// extract -> validate -> summarize
let pipeline = extract >> validate >> summarize;

let agent = pipeline.compile(llm)?;
let state = State::new();
state.set("input", document_text);

let report = agent.run(&state).await?;
```

For pre-built patterns like review loops and supervised workflows, see the `patterns` module:

```rust,ignore
use gemini_adk_fluent_rs::patterns::{review_loop, cascade, fan_out_merge, supervised};

// Worker -> Reviewer -> repeat until quality passes
let reviewed = review_loop(
    AgentBuilder::new("writer").instruction("Write a report"),
    AgentBuilder::new("reviewer").instruction("Rate quality as 'good' or 'needs_work'"),
    "quality",  // state key to check
    "good",     // target value
    3,          // max rounds
);

// Try each model until one succeeds
let robust = cascade(vec![primary_agent, secondary_agent, fallback_agent]);

// Run all in parallel, merge results
let multi = fan_out_merge(vec![tech_analyst, business_analyst, legal_analyst]);

// Worker -> Supervisor with approval gate
let approved = supervised(
    AgentBuilder::new("drafter").instruction("Draft the document"),
    AgentBuilder::new("supervisor").instruction("Approve or request revisions"),
    "approved",  // boolean state key
    5,           // max revisions
);
```

## See also

- [Composition](./composition.md) — operator syntax (`>>`, `|`, `*`, `/`) for building pipelines with `AgentBuilder`
- [Tools](./tools.md) — equipping text agents with tools via `ToolDispatcher`
- [cookbook 04 — sequential pipeline](../../examples/cookbook/src/02_combinators.rs)
- [cookbook 05 — parallel fan-out](../../examples/cookbook/src/02_combinators.rs)
- [cookbook 06 — loop agent](../../examples/cookbook/src/02_combinators.rs)
- [cookbook 11 — route branching](../../examples/cookbook/src/11_route_branching.rs)
- [cookbook 12 — fallback chain](../../examples/cookbook/src/12_fallback_chain.rs)
- [cookbook 13 — review loop](../../examples/cookbook/src/13_review_loop.rs)
- [cookbook 14 — map-over](../../examples/cookbook/src/14_map_over.rs)

## Testing without a model

`MockLlm` stands in for the model: `MockLlm::text` repeats one reply,
`MockLlm::script` replies in order (and fails if the agent calls once more than
the script allows), and `MockLlm::from_fn` computes each reply from the
request. It records every request, so a test asserts on what the agent sent —
instructions, history, tool declarations, sampling — as well as on what it
answered.

```rust,ignore
use gemini_adk_fluent_rs::testing::{LlmResponse, MockLlm};

let llm = MockLlm::script([
    LlmResponse::tool_call("order_status", serde_json::json!({ "id": "A-17" })),
    LlmResponse::from_text("Your order has shipped."),
]);
let agent = AgentBuilder::new("support")
    .tool(order_status())
    .build(llm.clone())?;

assert_eq!(agent.ask("Where is order A-17?").await?, "Your order has shipped.");
assert_eq!(llm.call_count(), 2);
```

`MockLlm` streams text replies one word at a time, so `stream` and
`send_stream` are exercised the way a real model would drive them.
