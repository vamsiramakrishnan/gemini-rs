//! `#[tool]` rejects methods taking `self`.

use gemini_adk_macros_rs::tool;

#[allow(dead_code, reason = "the trybuild fixture only has to compile")]
struct Weather;

impl Weather {
    #[tool("Method, not a free fn")]
    async fn current(&self) -> Result<serde_json::Value, gemini_adk_rs::error::ToolError> {
        Ok(serde_json::json!({}))
    }
}

fn main() {}
