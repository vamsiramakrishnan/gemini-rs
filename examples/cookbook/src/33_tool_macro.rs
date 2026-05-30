//! # 33 — The `#[tool]` Attribute Macro
//!
//! Demonstrates the `#[tool]` attribute macro, which turns a plain `async fn`
//! into a registrable Gemini tool — no separate args struct, no
//! `TypedTool::new::<Args>` ceremony.
//!
//! Key concepts:
//! - `#[tool("description")]` — annotate an `async fn` to make a tool
//! - parameters become a schemars-generated JSON Schema automatically
//! - `Option<T>` parameters are optional in the schema
//! - the macro generates `fn <name>() -> impl ToolFunction`; register it with
//!   `dispatcher.register_function(Arc::new(<name>()))`
//!
//! Runnable without credentials: it just dispatches a tool call and prints the
//! result.

use gemini_adk_fluent_rs::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;

/// Get the current weather for a city.
#[tool("Get the current weather for a city")]
async fn get_weather(city: String, units: Option<String>) -> Result<Value, ToolError> {
    let units = units.unwrap_or_else(|| "metric".to_string());
    Ok(json!({ "city": city, "temp_c": 22, "condition": "sunny", "units": units }))
}

/// Add two numbers together.
#[tool("Add two integers and return the sum")]
async fn add(a: i64, b: i64) -> Result<Value, ToolError> {
    Ok(json!({ "sum": a + b }))
}

#[tokio::main]
async fn main() {
    println!("=== 33: The #[tool] Attribute Macro ===\n");

    // The macro produced a constructor per tool. Register them in a dispatcher.
    let mut dispatcher = ToolDispatcher::new();
    dispatcher.register_function(Arc::new(get_weather()));
    dispatcher.register_function(Arc::new(add()));

    println!("Registered {} tools.\n", dispatcher.len());

    // Inspect the auto-generated schema for `get_weather`.
    let weather = get_weather();
    if let Some(schema) = weather.parameters() {
        println!("get_weather schema properties:");
        if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
            for name in props.keys() {
                println!("  - {name}");
            }
        }
        println!();
    }

    // Dispatch a tool call exactly as the runtime would on a model FunctionCall.
    let weather_result = dispatcher
        .call_function("get_weather", json!({ "city": "London" }))
        .await
        .expect("get_weather should succeed");
    println!("get_weather(London) -> {weather_result}");

    let add_result = dispatcher
        .call_function("add", json!({ "a": 19, "b": 23 }))
        .await
        .expect("add should succeed");
    println!("add(19, 23) -> {add_result}");

    println!("\nDone.");
}
