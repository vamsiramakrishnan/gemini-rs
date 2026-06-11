//! Anchor: a well-formed `#[tool]` fn expands and compiles, including the
//! prelude and fully-qualified `Option` spellings for optional parameters.

use gemini_adk_macros_rs::tool;
use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::tool::ToolFunction;
use serde_json::{json, Value};

#[tool("Get the current weather for a city")]
async fn get_weather(
    city: String,
    units: Option<String>,
    note: std::option::Option<String>,
) -> Result<Value, ToolError> {
    Ok(json!({
        "city": city,
        "units": units.unwrap_or_else(|| "metric".to_string()),
        "note": note,
    }))
}

fn main() {
    let t = get_weather();
    assert_eq!(t.name(), "get_weather");
    // Both `Option` spellings must be optional in the generated schema.
    let params = t.parameters().expect("schema");
    let required: Vec<&str> = params["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(required, vec!["city"]);
}
