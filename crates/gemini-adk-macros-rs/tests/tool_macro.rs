//! Integration tests for the `#[tool]` attribute macro.
//!
//! Proc-macro crates can only be tested through a downstream crate, so these
//! live in `tests/` (not `#[cfg(test)] mod`). They exercise the generated code
//! against the real `gemini-adk-rs` crate graph.

use gemini_adk_macros_rs::tool;
use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::tool::{ToolDispatcher, ToolFunction};
use serde_json::{Value, json};

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
        .map(|p| p.as_object().map(serde_json::Map::is_empty).unwrap_or(true))
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

// ── Doc-comment descriptions, typed outputs, any error ────────────────────

#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema, Debug, PartialEq)]
enum Units {
    Metric,
    Imperial,
}

#[derive(serde::Serialize)]
struct Forecast {
    city: String,
    high_c: i32,
}

/// Forecast the weather
/// for a city.
///
/// # Arguments
///
/// * `city` - The city name,
///   e.g. "Paris".
/// * `units` - How to report temperatures.
/// * `days` - How far ahead to look.
///
/// # Errors
///
/// When the city is unknown.
#[tool]
async fn forecast(city: String, units: Units, days: Option<u8>) -> std::io::Result<Forecast> {
    if city == "Atlantis" {
        return Err(std::io::Error::other("unknown city: Atlantis"));
    }
    let _ = (units, days);
    Ok(Forecast { city, high_c: 21 })
}

/// Add two numbers.
#[tool]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}

/// Always refuses, with a specific tool error.
#[tool]
async fn refuse() -> Result<Value, ToolError> {
    Err(ToolError::InvalidArgs("refused on purpose".into()))
}

/// Record a note; returns nothing.
#[tool]
async fn note(text: String) {
    let _ = text;
}

/// Only exists when the flag is on.
#[cfg(any())]
#[tool]
async fn never_compiled(x: NotAType) -> Result<Value, ToolError> {
    unreachable!()
}

#[test]
fn the_doc_comment_is_the_description_and_the_attribute_overrides_it() {
    assert_eq!(forecast().description(), "Forecast the weather for a city.");
    assert_eq!(
        get_weather().description(),
        "Get the current weather for a city"
    );
}

#[test]
fn arguments_section_becomes_parameter_descriptions() {
    let schema = forecast().parameters().unwrap();
    let props = &schema["properties"];
    assert_eq!(
        props["city"]["description"],
        "The city name, e.g. \"Paris\"."
    );
    assert_eq!(props["units"]["description"], "How to report temperatures.");
    assert_eq!(props["days"]["description"], "How far ahead to look.");
}

/// The macro used to send raw `schema_for!` output, which the API rejects for
/// `Option<T>` (`"type": [.., "null"]`) and misreads for nested enums (`$ref`).
#[test]
fn the_schema_is_wire_clean() {
    let schema = forecast().parameters().unwrap();
    let rendered = schema.to_string();
    assert!(!rendered.contains("$ref"), "{rendered}");
    assert!(!rendered.contains("definitions"), "{rendered}");
    assert!(!rendered.contains("$schema"), "{rendered}");
    assert_eq!(schema["properties"]["days"]["type"], "integer");
    assert_eq!(schema["properties"]["units"]["type"], "string");
    assert_eq!(
        schema["properties"]["units"]["enum"],
        json!(["Metric", "Imperial"])
    );
    assert_eq!(schema["required"], json!(["city", "units"]));
}

#[tokio::test]
async fn any_serialize_output_is_returned_as_json() {
    let out = forecast()
        .call(json!({ "city": "Paris", "units": "Metric" }))
        .await
        .unwrap();
    assert_eq!(out, json!({ "city": "Paris", "high_c": 21 }));
    assert_eq!(
        add().call(json!({ "a": 2, "b": 3 })).await.unwrap(),
        json!(5)
    );
    assert_eq!(
        note().call(json!({ "text": "hi" })).await.unwrap(),
        Value::Null
    );
}

#[tokio::test]
async fn any_error_becomes_a_tool_error_and_a_tool_error_keeps_its_kind() {
    let err = forecast()
        .call(json!({ "city": "Atlantis", "units": "Metric" }))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::ExecutionFailed(m) if m == "unknown city: Atlantis"),
        "{err:?}"
    );
    let err = refuse().call(json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err:?}");
}

/// Look up the caller's balance. The context supplies the account; the
/// model supplies only the currency.
#[tool]
async fn balance(
    currency: String,
    ctx: gemini_adk_rs::tool::ToolContext,
) -> Result<Value, ToolError> {
    let account: String = ctx.state.get("account_id").unwrap_or_default();
    Ok(json!({ "account": account, "currency": currency, "call": ctx.call_id }))
}

#[test]
fn a_context_parameter_is_not_shown_to_the_model() {
    let schema = balance().parameters().unwrap();
    assert!(schema["properties"].get("currency").is_some());
    assert!(schema["properties"].get("ctx").is_none(), "{schema}");
}

#[tokio::test]
async fn a_context_parameter_is_filled_by_the_runtime() {
    let state = gemini_adk_rs::State::new();
    state.set("account_id", "A-17").unwrap();
    let mut dispatcher = ToolDispatcher::new();
    dispatcher.register(balance());
    let out = dispatcher
        .call_function_in(
            "balance",
            json!({ "currency": "EUR" }),
            gemini_adk_rs::tool::ToolContext::new(state).with_call_id("call-9"),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        json!({ "account": "A-17", "currency": "EUR", "call": "call-9" })
    );

    // Called outside a session, it gets a detached context.
    let out = balance().call(json!({ "currency": "EUR" })).await.unwrap();
    assert_eq!(out["account"], "");
}
