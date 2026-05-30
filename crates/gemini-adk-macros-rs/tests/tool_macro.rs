//! Integration tests for the `#[tool]` attribute macro.
//!
//! Proc-macro crates can only be tested through a downstream crate, so these
//! live in `tests/` (not `#[cfg(test)] mod`). They exercise the generated code
//! against the real `gemini-adk-rs` crate graph.

use gemini_adk_macros_rs::tool;
use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::tool::{ToolDispatcher, ToolFunction};
use serde_json::{json, Value};

/// Get the current weather for a city.
#[tool("Get the current weather for a city")]
async fn get_weather(city: String, units: Option<String>) -> Result<Value, ToolError> {
    Ok(json!({
        "city": city,
        "temp_c": 22,
        "units": units.unwrap_or_else(|| "metric".to_string()),
    }))
}

/// A zero-parameter tool to confirm the empty-schema path works.
#[tool("Return the answer to everything")]
async fn answer() -> Result<Value, ToolError> {
    Ok(json!({ "answer": 42 }))
}

#[tokio::test]
async fn metadata_is_correct() {
    let t = get_weather();
    assert_eq!(t.name(), "get_weather");
    assert_eq!(t.description(), "Get the current weather for a city");
}

#[tokio::test]
async fn parameters_schema_has_expected_properties() {
    let t = get_weather();
    let params = t.parameters().expect("should produce a schema");
    let props = &params["properties"];
    assert!(props.get("city").is_some(), "schema should contain 'city'");
    assert!(
        props.get("units").is_some(),
        "schema should contain 'units'"
    );

    // `city` is required (non-Option), `units` is optional.
    let required: Vec<&str> = params["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(required.contains(&"city"), "city should be required");
    assert!(!required.contains(&"units"), "units should not be required");
}

#[tokio::test]
async fn call_runs_the_body() {
    let t = get_weather();
    let result = t
        .call(json!({ "city": "London", "units": "imperial" }))
        .await
        .unwrap();
    assert_eq!(result["city"], "London");
    assert_eq!(result["temp_c"], 22);
    assert_eq!(result["units"], "imperial");
}

#[tokio::test]
async fn optional_param_defaults_to_none() {
    let t = get_weather();
    let result = t.call(json!({ "city": "Paris" })).await.unwrap();
    assert_eq!(result["units"], "metric");
}

#[tokio::test]
async fn invalid_args_return_error() {
    let t = get_weather();
    // Missing required field "city".
    let err = t.call(json!({ "units": "metric" })).await.unwrap_err();
    match err {
        ToolError::InvalidArgs(msg) => assert!(msg.contains("city"), "msg: {msg}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn zero_param_tool_works() {
    let t = answer();
    assert_eq!(t.name(), "answer");
    let params = t.parameters().expect("schema");
    // Empty object schema — no properties (or an empty properties map).
    let has_no_props = params
        .get("properties")
        .map(|p| p.as_object().map(|o| o.is_empty()).unwrap_or(true))
        .unwrap_or(true);
    assert!(has_no_props, "zero-param tool should have no properties");

    let result = t.call(json!({})).await.unwrap();
    assert_eq!(result["answer"], 42);
}

#[tokio::test]
async fn registers_in_dispatcher() {
    let mut d = ToolDispatcher::new();
    d.register_function(std::sync::Arc::new(get_weather()));
    d.register_function(std::sync::Arc::new(answer()));
    assert_eq!(d.len(), 2);

    let result = d
        .call_function("get_weather", json!({ "city": "Tokyo" }))
        .await
        .unwrap();
    assert_eq!(result["city"], "Tokyo");
}
