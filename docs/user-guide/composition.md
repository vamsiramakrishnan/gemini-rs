# S.C.T.P.M.A Operator Algebra

Six namespace modules for declaratively composing agent primitives. Each maps to a dimension of agent configuration: **S**tate, **C**ontext, **T**ools, **P**rompt, **M**iddleware, **A**rtifacts. They compose using Rust operators so agent definitions read like algebraic expressions.

## The Six Operators

| Namespace | Operator | Purpose | Key Methods |
|---|---|---|---|
| `S::` | `>>` | State transforms | `pick`, `rename`, `merge`, `flatten`, `set`, `defaults`, `drop`, `map` |
| `C::` | `+` | Context engineering | `window`, `user_only`, `model_only`, `head`, `truncate`, `filter`, `from_state` |
| `T::` | `\|` | Tool composition | `simple`, `function`, `google_search`, `code_execution`, `toolset` |
| `P::` | `+` | Prompt composition | `role`, `task`, `constraint`, `format`, `example`, `persona`, `guidelines` |
| `M::` | `\|` | Middleware layers | `log`, `latency`, `timeout`, `retry`, `audit`, `circuit_breaker` |
| `A::` | `+` | Artifact schemas | `output`, `input`, `json_output`, `json_input`, `text_output`, `text_input` |

## S -- State Transforms

State transforms mutate a `serde_json::Value` representing agent state. Chain them with `>>` for sequential application.

### Methods

| Method | What it does |
|---|---|
| `S::pick(&["a", "b"])` | Keep only the listed keys, drop everything else |
| `S::drop(&["x"])` | Remove the listed keys |
| `S::rename(&[("old", "new")])` | Rename keys according to mappings |
| `S::merge(&["x", "y"], "combined")` | Merge listed keys into a single nested object |
| `S::flatten("nested")` | Flatten a nested object into the top level |
| `S::set("key", json!(42))` | Set a key to a fixed value |
| `S::defaults(json!({"k": "v"}))` | Set default values for missing keys |
| `S::map(fn)` | Apply a custom transformation function |

### State Predicates

`S` also provides predicates for phase transition guards and `.when()` modifiers:

| Predicate | What it checks |
|---|---|
| `S::is_true("key")` | Key holds a truthy boolean |
| `S::eq("key", "value")` | Key equals a specific string |
| `S::one_of("key", &["a", "b"])` | Key matches any of the listed strings |

### Example

```rust,ignore
use gemini_adk_fluent_rs::compose::S;

// Chain transforms: pick keys, then rename
let transform = S::pick(&["name", "age"]) >> S::rename(&[("name", "customer_name")]);

let mut state = json!({"name": "Alice", "age": 30, "internal_id": "x123"});
transform.apply(&mut state);
// Result: {"customer_name": "Alice", "age": 30}

// Predicates for phase transitions
Live::builder()
    .phase("verify")
        .instruction("Verify the customer's identity")
        .transition("main", S::is_true("verified"))
        .done()
    .phase("main")
        .instruction("Handle the request")
        .transition("billing", S::eq("issue_type", "billing"))
        .transition("tech", S::one_of("issue_type", &["technical", "setup"]))
        .done()
```

## C -- Context Engineering

Context policies filter and transform conversation history. Compose them with `+` to combine multiple policies.

### Methods

| Method | What it does |
|---|---|
| `C::window(n)` | Keep only the last `n` messages |
| `C::head(n)` | Keep only the first `n` messages |
| `C::last(n)` | Alias for `window(n)` |
| `C::user_only()` | Keep only user messages |
| `C::model_only()` | Keep only model messages |
| `C::text_only()` | Keep only messages containing text parts |
| `C::exclude_tools()` | Remove messages with function call/response parts |
| `C::sample(n)` | Keep every n-th message |
| `C::truncate(max_chars)` | Truncate to approximately `max_chars` total text (keeps most recent) |
| `C::prepend(content)` | Add a message at the start of context |
| `C::append(content)` | Add a message at the end of context |
| `C::from_state(&["key1", "key2"])` | Inject state values as a context preamble |
| `C::dedup()` | Remove adjacent duplicate messages |
| `C::empty()` | Return empty context (for isolated agents) |
| `C::filter(fn)` | Filter messages by a custom predicate on `Content` |
| `C::map(fn)` | Transform each message with a custom function |
| `C::custom(fn)` | Full custom filter over the entire history |

### Example

```rust,ignore
use gemini_adk_fluent_rs::compose::C;

// Keep recent context, no tool noise, inject state
let policy = C::window(20) + C::exclude_tools() + C::from_state(&["user:name", "app:balance"]);

// For isolated sub-agents that should not see conversation history
let isolated = C::empty();

// Character-budget context for cost control
let budget = C::truncate(4000) + C::dedup();
```

## T -- Tool Composition

Compose tools with `|`. Mix runtime function tools with built-in Gemini tools.

### Methods

| Method | What it does |
|---|---|
| `T::simple(name, desc, fn)` | Create a tool from a name, description, and async closure |
| `T::function(arc_fn)` | Register an existing `Arc<dyn ToolFunction>` |
| `T::google_search()` | Add built-in Google Search |
| `T::url_context()` | Add built-in URL context fetching |
| `T::code_execution()` | Add built-in code execution |
| `T::toolset(vec)` | Combine multiple tool functions into one composite |

### Example

```rust,ignore
use gemini_adk_fluent_rs::compose::T;

// Combine custom tools with built-ins
let tools = T::simple("get_weather", "Get weather for a city", |args| async move {
        let city = args["city"].as_str().unwrap_or("Unknown");
        Ok(json!({"temp": 22, "city": city}))
    })
    | T::google_search()
    | T::code_execution();

assert_eq!(tools.len(), 3);

// Use in a Live session builder
Live::builder()
    .with_tools(tools)
```

## P -- Prompt Composition

Compose structured prompt sections with `+`. Each section has a semantic kind that determines its rendering format.

### Methods

| Method | Renders as | Kind |
|---|---|---|
| `P::role("analyst")` | `"You are analyst."` | Role |
| `P::task("analyze data")` | `"Your task: analyze data"` | Task |
| `P::constraint("be concise")` | `"Constraint: be concise"` | Constraint |
| `P::format("JSON")` | `"Output format: JSON"` | Format |
| `P::example("input", "output")` | `"Example:\nInput: ...\nOutput: ..."` | Example |
| `P::context("background info")` | `"Context: background info"` | Context |
| `P::persona("friendly, direct")` | `"Persona: friendly, direct"` | Persona |
| `P::guidelines(&["be clear", ...])` | `"Guidelines:\n- be clear\n- ..."` | Guidelines |
| `P::text("free-form text")` | The text as-is | Text |

### PromptSection Kinds

The `PromptSectionKind` enum provides semantic categories:

| Kind | Purpose |
|---|---|
| `Role` | Agent role definition |
| `Task` | Task description |
| `Constraint` | Behavioral constraint |
| `Format` | Output format specification |
| `Example` | Input/output example |
| `Context` | Background context |
| `Persona` | Personality description |
| `Guidelines` | Bulleted guideline list |
| `Text` | Free-form text |

### Instruction Modifiers

`P` also provides instruction modifier factories that bridge the prompt module to the live phase system:

| Method | What it does |
|---|---|
| `P::with_state(&["key1", "key2"])` | Append selected state keys to the instruction |
| `P::when(predicate, text)` | Conditionally append text based on state |
| `P::context_fn(fn)` | Append dynamic text from a formatting function |

### Example

```rust,ignore
use gemini_adk_fluent_rs::compose::P;

// Build a structured prompt
let prompt = P::role("a senior financial analyst")
    + P::task("Review the quarterly earnings report and identify trends")
    + P::constraint("Use only data from the provided report")
    + P::constraint("Flag any numbers that seem inconsistent")
    + P::format("Markdown with headers for each section")
    + P::guidelines(&[
        "Start with an executive summary",
        "Include specific numbers when citing trends",
        "End with a risk assessment",
    ]);

// Render to a single instruction string
let instruction: String = prompt.into();
// "You are a senior financial analyst.\n\nYour task: Review the quarterly...\n\n..."

// Instruction modifiers for phases
Live::builder()
    .phase("negotiation")
        .instruction("Negotiate a payment arrangement")
        .modifiers(vec![
            P::with_state(&["emotional_state", "willingness_to_pay"]),
            P::when(
                |s| s.get::<String>("risk").unwrap_or_default() == "high",
                "IMPORTANT: Show extra empathy and offer flexible options.",
            ),
            P::context_fn(|s| {
                let name = s.get::<String>("user:name").unwrap_or_default();
                format!("Customer: {name}")
            }),
        ])
        .done()
```

## M -- Middleware Composition

Compose middleware layers with `|`. Middleware intercepts agent events, tool calls, and errors.

### Methods

| Method | What it does |
|---|---|
| `M::log()` | Log all agent events |
| `M::latency()` | Track execution latency |
| `M::timeout(duration)` | Enforce a time limit |
| `M::retry(max)` | Retry on failure up to `max` times |
| `M::cost()` | Track tool call counts as a cost proxy |
| `M::rate_limit(rps)` | Enforce max requests per second |
| `M::circuit_breaker(threshold)` | Open circuit after consecutive failures |
| `M::trace()` | Create distributed tracing spans |
| `M::audit()` | Record all tool calls for review |
| `M::tap(fn)` | Custom event observer |
| `M::before_tool(fn)` | Custom filter before each tool invocation |
| `M::validate(fn)` | Validate tool input arguments |

### Example

```rust,ignore
use gemini_adk_fluent_rs::compose::M;
use std::time::Duration;

// Production middleware stack
let middleware = M::log()
    | M::latency()
    | M::timeout(Duration::from_secs(30))
    | M::retry(3)
    | M::circuit_breaker(5)
    | M::audit();

assert_eq!(middleware.len(), 6);

// Custom validation
let validated = M::validate(|call| {
    if call.name == "delete_account" && call.args.get("confirm").is_none() {
        return Err("delete_account requires 'confirm' argument".into());
    }
    Ok(())
});
```

## A -- Artifact Schemas

Declare input/output artifact schemas with `+`. Artifacts describe data that flows between agents as typed, named entities.

### Methods

| Method | What it does |
|---|---|
| `A::output(name, mime, desc)` | Declare an output artifact |
| `A::input(name, mime, desc)` | Declare an input artifact |
| `A::json_output(name, desc)` | Shorthand for `application/json` output |
| `A::json_input(name, desc)` | Shorthand for `application/json` input |
| `A::text_output(name, desc)` | Shorthand for `text/plain` output |
| `A::text_input(name, desc)` | Shorthand for `text/plain` input |

### Example

```rust,ignore
use gemini_adk_fluent_rs::compose::A;

// Declare what an analysis agent produces and consumes
let artifacts = A::text_input("source_document", "The document to analyze")
    + A::json_output("analysis_report", "Structured analysis results")
    + A::json_output("risk_assessment", "Risk scores and flags");

assert_eq!(artifacts.all_inputs().len(), 1);
assert_eq!(artifacts.all_outputs().len(), 2);
```

## Composable Operators

Beyond the six namespace modules, the `operators` module provides structural composition via Rust operators on `AgentBuilder`:

| Operator | Type | Meaning |
|---|---|---|
| `>>` | `Shr` | Sequential pipeline |
| `\|` | `BitOr` | Parallel fan-out |
| `*` | `Mul<u32>` | Fixed-count loop |
| `*` | `Mul<LoopPredicate>` | Conditional loop |
| `/` | `Div` | Fallback chain |

These produce `Composable` nodes that form a tree. Call `.compile(llm)` to turn the tree into an executable `TextAgent`.

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

// Build a tree
let workflow = AgentBuilder::new("research").instruction("Research the topic")
    >> (AgentBuilder::new("tech").instruction("Technical review")
        | AgentBuilder::new("biz").instruction("Business review"))
    >> AgentBuilder::new("merge").instruction("Merge perspectives");

// Compile and execute
let agent = workflow.compile(llm);
let result = agent.run(&state).await?;
```

The tree auto-flattens: `a >> b >> c` produces a single `Pipeline` with 3 steps, not nested pipelines.

## Combining Operators into Full Agent Configuration

The six namespaces compose orthogonally. Each configures a separate dimension:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

// S: transform state before agent sees it
let state_prep = S::pick(&["customer", "order"]) >> S::defaults(json!({"priority": "normal"}));

// C: control what context the agent sees
let context = C::window(10) + C::exclude_tools();

// T: equip the agent with tools
let tools = T::simple("lookup", "Look up order status", |args| async move {
        Ok(json!({"status": "shipped"}))
    })
    | T::google_search();

// P: compose the instruction
let prompt = P::role("a customer support specialist")
    + P::task("Help the customer with their order inquiry")
    + P::constraint("Never reveal internal order IDs")
    + P::format("Conversational, friendly tone");

// A: declare I/O artifacts
let artifacts = A::json_output("resolution", "How the issue was resolved");

// M: add operational middleware
let middleware = M::log() | M::latency() | M::audit();
```

## Builder Integration

`AgentBuilder` is the entry point for compiling these into executable agents:

```rust,ignore
use gemini_adk_fluent_rs::builder::AgentBuilder;

let agent = AgentBuilder::new("support")
    .model(GeminiModel::Gemini2_0Flash)
    .instruction("Help the customer")   // or use P:: composition
    .temperature(0.5)
    .google_search()                     // or use T:: composition
    .thinking(2048)
    .writes("resolution")
    .reads("customer_name")
    .build(llm);

let result = agent.run(&state).await?;
```

Copy-on-write semantics mean every setter returns a new builder. Use builders as templates:

```rust,ignore
let base = AgentBuilder::new("analyst")
    .instruction("You are a data analyst")
    .temperature(0.3);

// Variants share the base configuration
let conservative = base.clone().temperature(0.1);
let creative = base.clone().temperature(0.9);
```

## Pre-Built Patterns

The `patterns` module provides common multi-agent workflows built from these operators:

```rust,ignore
use gemini_adk_fluent_rs::patterns::*;

// Review loop: worker -> reviewer -> repeat until quality target
let reviewed = review_loop(writer, reviewer, "quality", "good", 3);

// Cascade: try each agent until one succeeds
let robust = cascade(vec![primary, secondary, fallback]);

// Fan-out merge: run all in parallel, merge results
let multi = fan_out_merge(vec![analyst_a, analyst_b, analyst_c]);

// Supervised: worker -> supervisor -> repeat until approval
let approved = supervised(drafter, supervisor, "approved", 5);

// Map-over: apply agent to each item with concurrency limit
let batch = map_over(item_processor, 4);
```
