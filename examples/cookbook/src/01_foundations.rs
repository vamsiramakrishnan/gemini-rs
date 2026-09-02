//! # 01 — Foundations: Agents, Tools & Callbacks
//!
//! The starting point for the cookbook. Three foundations in one example:
//!
//! 1. **Agents** — build a named agent with `AgentBuilder`, set the model and
//!    sampling parameters, and use copy-on-write templates and data contracts.
//! 2. **Tools** — attach `SimpleTool` (raw JSON args) and `TypedTool`
//!    (schema auto-derived from a Rust struct), register them on a
//!    `ToolDispatcher`, and add built-in Gemini tools.
//! 3. **Callbacks** — hook the agent lifecycle with the `M::` middleware
//!    module: model/tool hooks, event taps, and resilience layers.
//!
//! Everything here is construction-only (no network), so it runs without
//! credentials.

use gemini_adk_fluent_rs::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

fn main() {
    println!("=== 01: Foundations ===\n");
    agents();
    tools();
    callbacks();
    println!("\nDone.");
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Agents — AgentBuilder, copy-on-write, sampling params, data contracts
// ─────────────────────────────────────────────────────────────────────────────
fn agents() {
    println!("── Agents ─────────────────────────────────────────────\n");

    // AgentBuilder uses copy-on-write: each setter returns a new builder.
    let agent = AgentBuilder::new("analyst")
        .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
        .instruction("Analyze the given topic and provide key insights.")
        .temperature(0.3);

    println!("Agent name:        {}", agent.name());
    println!("Model:             {:?}", agent.get_model());
    println!("Temperature:       {:?}", agent.get_temperature());

    // Clone the builder and modify it — the original stays unchanged.
    let creative = agent.clone().temperature(0.95);
    let precise = agent.clone().temperature(0.1);
    assert_eq!(agent.get_temperature(), Some(0.3)); // original unchanged
    assert_eq!(creative.get_temperature(), Some(0.95));
    assert_eq!(precise.get_temperature(), Some(0.1));
    println!(
        "Copy-on-write: original={:?}, creative={:?}, precise={:?}",
        agent.get_temperature(),
        creative.get_temperature(),
        precise.get_temperature(),
    );

    // Additional sampling parameters + thinking budget.
    let detailed = AgentBuilder::new("detailed-analyst")
        .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
        .instruction("Provide thorough analysis with citations.")
        .temperature(0.5)
        .top_p(0.9)
        .top_k(40)
        .max_output_tokens(4096)
        .thinking(2048);
    println!(
        "\nDetailed: top_p={:?}, top_k={:?}, max_tokens={:?}, thinking={:?}",
        detailed.get_top_p(),
        detailed.get_top_k(),
        detailed.get_max_output_tokens(),
        detailed.get_thinking_budget(),
    );

    // Text-only mode.
    let text_agent = AgentBuilder::new("writer")
        .instruction("Write clear prose.")
        .text_only();
    println!("Writer is text_only: {}", text_agent.is_text_only());

    // Data contracts: declare which state keys an agent reads and writes.
    // Used by `check_contracts()` to find wiring bugs at build time.
    let researcher = AgentBuilder::new("researcher")
        .instruction("Research the topic.")
        .writes("findings")
        .writes("sources");
    let writer = AgentBuilder::new("writer")
        .instruction("Write an article from findings.")
        .reads("findings")
        .writes("draft");
    println!("\nResearcher writes: {:?}", researcher.get_writes());
    println!("Writer reads:      {:?}", writer.get_reads());
    println!("Writer writes:     {:?}\n", writer.get_writes());
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Tools — SimpleTool, TypedTool, ToolDispatcher, built-in tools
// ─────────────────────────────────────────────────────────────────────────────
fn tools() {
    println!("── Tools ──────────────────────────────────────────────\n");

    // SimpleTool: you supply the JSON Schema and receive `serde_json::Value` args.
    let weather_tool = SimpleTool::new(
        "get_weather",
        "Get current weather for a city",
        Some(json!({
            "type": "object",
            "properties": { "city": { "type": "string", "description": "City name" } },
            "required": ["city"]
        })),
        |args| async move {
            let city = args["city"].as_str().unwrap_or("Unknown");
            Ok(json!({ "temp_c": 22, "condition": "sunny", "city": city }))
        },
    );
    println!(
        "SimpleTool:  {} — {}",
        weather_tool.name(),
        weather_tool.description()
    );

    // TypedTool: schema derived from a `JsonSchema` struct — prevents drift
    // between your code and the schema the model sees.
    #[derive(Deserialize, JsonSchema)]
    struct CalculateArgs {
        /// First operand
        a: f64,
        /// Second operand
        b: f64,
        /// Operation: add, subtract, multiply, divide
        operation: String,
    }

    let calc_tool = TypedTool::<CalculateArgs>::new(
        "calculate",
        "Perform arithmetic operations",
        |args: CalculateArgs| async move {
            let result = match args.operation.as_str() {
                "add" => args.a + args.b,
                "subtract" => args.a - args.b,
                "multiply" => args.a * args.b,
                "divide" if args.b != 0.0 => args.a / args.b,
                "divide" => return Ok(json!({ "error": "division by zero" })),
                _ => return Ok(json!({ "error": "unknown operation" })),
            };
            Ok(json!({ "result": result }))
        },
    );
    println!(
        "TypedTool:   {} — {}",
        calc_tool.name(),
        calc_tool.description()
    );

    // ToolDispatcher routes function calls to the correct tool by name.
    let mut dispatcher = ToolDispatcher::new();
    dispatcher.register(weather_tool);
    dispatcher.register(calc_tool);
    println!("Dispatcher:  {} tools registered", dispatcher.len());

    // Built-in Gemini platform tools (no local handler needed).
    let search_agent = AgentBuilder::new("researcher")
        .instruction("Research topics using web search.")
        .google_search()
        .code_execution()
        .url_context();
    println!(
        "Built-ins:   agent '{}' has {} tools attached\n",
        search_agent.name(),
        search_agent.tool_count(),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Callbacks — the M:: middleware module
// ─────────────────────────────────────────────────────────────────────────────
fn callbacks() {
    println!("── Callbacks (middleware) ─────────────────────────────\n");

    // Built-in observability: M::log() logs events, M::latency() tracks timing.
    let observability = M::log() | M::latency();
    println!("Observability stack: {} layers", observability.len());

    // Model hooks fire around every LLM call.
    let model_hooks = M::before_model(|_req| {
        println!("  [before_model] About to call the LLM");
        Ok(())
    }) | M::after_model(|_req, _resp| {
        println!("  [after_model] LLM responded");
        Ok(())
    });
    println!("Model hooks: {} layers", model_hooks.len());

    // Tool hooks fire before each tool invocation and can reject calls.
    let tool_hooks = M::before_tool(|call| {
        if call.name == "dangerous_tool" {
            return Err("Blocked: dangerous_tool is not allowed".into());
        }
        Ok(())
    });
    println!("Tool hooks: {} layers", tool_hooks.len());

    // M::tap() receives every AgentEvent — useful for custom telemetry.
    let tap = M::tap(|event| println!("  [tap] Event: {event:?}"));
    println!("Tap middleware: {} layers", tap.len());

    // Resilience: retry, circuit breaker, rate limiting, timeout.
    let resilience = M::retry(3)
        | M::circuit_breaker(5)
        | M::rate_limit(10)
        | M::timeout(Duration::from_secs(30));
    println!("Resilience stack: {} layers", resilience.len());

    // Compose everything with `|` into one stack.
    let full_stack = M::log()
        | M::latency()
        | M::before_model(|_req| Ok(()))
        | M::after_model(|_req, _resp| Ok(()))
        | M::before_tool(|_call| Ok(()))
        | M::retry(3)
        | M::trace()
        | M::audit()
        | M::metrics()
        | M::cost();
    println!("Full stack: {} layers", full_stack.len());

    // Scoped middleware applies only to named agents.
    let scoped = M::scope(&["researcher", "writer"], M::log() | M::latency());
    println!("Scoped middleware: {} layers", scoped.len());

    // Agent lifecycle hooks.
    let lifecycle = M::before_agent(|ctx| {
        println!("  [before_agent] session={:?}", ctx.session_id);
        Ok(())
    }) | M::after_agent(|ctx| {
        println!("  [after_agent] session={:?}", ctx.session_id);
        Ok(())
    });
    println!("Lifecycle hooks: {} layers", lifecycle.len());
}
