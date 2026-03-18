//! # 09 — Tool Composition (T:: module)
//!
//! Demonstrates the T:: composition module for combining tools using
//! the `|` operator. Supports built-in Gemini tools, custom functions,
//! and mock tools.
//!
//! Key concepts:
//! - `T::google_search()` — built-in Google Search grounding tool
//! - `T::code_execution()` — built-in code execution tool
//! - `T::url_context()` — built-in URL context tool
//! - `T::simple()` — create a tool from a closure
//! - `T::mock()` — create a mock tool for testing
//! - `|` operator — compose tools additively

use gemini_adk_fluent::prelude::*;
use serde_json::json;

fn main() {
    println!("=== 09: Tool Composition (T::) ===\n");

    // ── Built-in tools ──
    // These are Gemini platform tools that don't need local handlers.
    let search = T::google_search();
    let code_exec = T::code_execution();
    let url_ctx = T::url_context();

    println!("Individual built-in tools:");
    println!("  google_search: {} entries", search.len());
    println!("  code_execution: {} entries", code_exec.len());
    println!("  url_context: {} entries", url_ctx.len());

    // ── Composing built-in tools with | ──
    let built_ins = T::google_search() | T::code_execution() | T::url_context();
    println!("\nComposed built-ins: {} entries", built_ins.len());

    // ── Custom tool with T::simple ──
    // T::simple creates a tool from a name, description, and async closure.
    // The closure receives serde_json::Value args and returns a Result.
    let custom_tools = T::simple(
        "get_weather",
        "Get weather for a location",
        |args| async move {
            let city = args
                .get("city")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            Ok(json!({ "temp_c": 22, "condition": "sunny", "city": city }))
        },
    ) | T::simple(
        "get_time",
        "Get current time in a timezone",
        |args| async move {
            let tz = args
                .get("timezone")
                .and_then(|v| v.as_str())
                .unwrap_or("UTC");
            Ok(json!({ "time": "14:30", "timezone": tz }))
        },
    );

    println!("\nCustom tools: {} entries", custom_tools.len());

    // ── Mock tools for testing ──
    // T::mock returns a fixed response — useful for testing without real APIs.
    let mock_tools = T::mock(
        "search_kb",
        "Search knowledge base",
        json!({
            "results": [
                {"title": "Getting Started", "relevance": 0.95},
                {"title": "FAQ", "relevance": 0.82}
            ]
        }),
    ) | T::mock(
        "get_account",
        "Fetch account details",
        json!({
            "id": "ACC-123",
            "name": "Test Account",
            "balance": 1000.00
        }),
    );

    println!("Mock tools: {} entries", mock_tools.len());

    // ── Full composition: built-ins + custom + mocks ──
    let all_tools = T::google_search()
        | T::code_execution()
        | T::simple("calculate", "Do math", |args| async move {
            let expr = args
                .get("expression")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            Ok(json!({ "result": expr, "note": "evaluation placeholder" }))
        })
        | T::mock("lookup", "Look up data", json!({"found": true}));

    println!("\nFull tool composite: {} entries", all_tools.len());
    for (i, entry) in all_tools.entries.iter().enumerate() {
        let name = match entry {
            gemini_adk_fluent::compose::tools::ToolCompositeEntry::Function(f) => f.name().to_string(),
            gemini_adk_fluent::compose::tools::ToolCompositeEntry::BuiltIn(_) => {
                "(built-in)".to_string()
            }
            gemini_adk_fluent::compose::tools::ToolCompositeEntry::Mock { name, .. } => {
                format!("{} (mock)", name)
            }
            _ => "(other)".to_string(),
        };
        println!("  Entry {}: {}", i + 1, name);
    }

    // ── Using T::toolset for bulk registration ──
    // T::toolset takes a Vec of ToolFunction trait objects.
    let tool_a: std::sync::Arc<dyn gemini_adk::ToolFunction> = std::sync::Arc::new(
        gemini_adk::SimpleTool::new("tool_a", "Tool A", None, |_| async { Ok(json!(null)) }),
    );
    let tool_b: std::sync::Arc<dyn gemini_adk::ToolFunction> = std::sync::Arc::new(
        gemini_adk::SimpleTool::new("tool_b", "Tool B", None, |_| async { Ok(json!(null)) }),
    );
    let bulk = T::toolset(vec![tool_a, tool_b]);
    println!("\nBulk toolset: {} entries", bulk.len());

    println!("\nDone.");
}
