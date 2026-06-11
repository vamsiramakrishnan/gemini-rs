//! `#[tool]` rejects non-async functions.

use gemini_adk_macros_rs::tool;

#[tool("Not async")]
fn not_async() -> Result<serde_json::Value, gemini_adk_rs::error::ToolError> {
    Ok(serde_json::json!({}))
}

fn main() {}
