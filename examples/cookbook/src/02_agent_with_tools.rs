//! # 02 — Agent with Tools
//!
//! Demonstrates attaching tools to agents using both `SimpleTool` (raw JSON args)
//! and `TypedTool` (auto-generated JSON Schema from a Rust struct).
//!
//! Key concepts:
//! - `SimpleTool::new()` — define a tool with a name, description, schema, and closure
//! - `TypedTool::new::<T>()` — derive the schema from `schemars::JsonSchema`
//! - `.google_search()` / `.code_execution()` — built-in Gemini tools on AgentBuilder

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_rs::{SimpleTool, ToolFunction, TypedTool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

fn main() {
    println!("=== 02: Agent with Tools ===\n");

    // ── SimpleTool: raw JSON args ──
    // You supply the JSON Schema manually and receive `serde_json::Value` args.
    let weather_tool = SimpleTool::new(
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
            println!("  [get_weather] Called with city={}", city);
            Ok(json!({ "temp_c": 22, "condition": "sunny", "city": city }))
        },
    );

    println!("SimpleTool created:");
    println!("  name:        {}", weather_tool.name());
    println!("  description: {}", weather_tool.description());

    // ── TypedTool: auto-generated schema ──
    // Define a Rust struct with `JsonSchema` and the schema is derived automatically.
    // This prevents drift between your code and the schema the model sees.
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
                "divide" => {
                    if args.b == 0.0 {
                        return Ok(json!({ "error": "division by zero" }));
                    }
                    args.a / args.b
                }
                _ => return Ok(json!({ "error": "unknown operation" })),
            };
            println!(
                "  [calculate] {} {} {} = {}",
                args.a, args.operation, args.b, result
            );
            Ok(json!({ "result": result }))
        },
    );

    println!("\nTypedTool created:");
    println!("  name:        {}", calc_tool.name());
    println!("  description: {}", calc_tool.description());
    println!(
        "  schema:      {}",
        serde_json::to_string_pretty(&calc_tool.parameters().unwrap()).unwrap()
    );

    // ── Registering tools with ToolDispatcher ──
    // ToolDispatcher routes function calls to the correct tool by name.
    let mut dispatcher = gemini_adk_rs::ToolDispatcher::new();
    dispatcher.register(weather_tool);
    dispatcher.register(calc_tool);

    println!("\nToolDispatcher has {} tools registered", dispatcher.len());

    // ── Built-in tools on AgentBuilder ──
    // These are Gemini platform tools (no local handler needed).
    let search_agent = AgentBuilder::new("researcher")
        .instruction("Research topics using web search.")
        .google_search()
        .code_execution()
        .url_context();

    println!(
        "\nAgent '{}' has {} built-in tools attached",
        search_agent.name(),
        search_agent.tool_count(),
    );

    println!("\nDone.");
}
