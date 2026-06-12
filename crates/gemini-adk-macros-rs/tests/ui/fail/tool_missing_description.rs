//! `#[tool]` requires a string-literal description argument.

use gemini_adk_macros_rs::tool;

#[tool]
async fn no_description() -> Result<serde_json::Value, gemini_adk_rs::error::ToolError> {
    Ok(serde_json::json!({}))
}

fn main() {}
