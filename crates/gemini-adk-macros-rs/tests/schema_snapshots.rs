//! Snapshots of what the macros hand the model and the runtime.
//!
//! A tool's parameter schema is an API contract: a change to it changes what
//! the model is asked to produce. These tests pin the schema each macro
//! generates to a committed JSON file under `tests/snapshots/`, so any change
//! shows up in review as a diff of that file.
//!
//! After an intended change, regenerate the files and commit them:
//!
//! ```text
//! UPDATE_SNAPSHOTS=1 cargo test -p gemini-adk-macros-rs --test schema_snapshots
//! ```

use std::path::PathBuf;

use gemini_adk_macros_rs::tool;
use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::extract::RecordExtractor;
use gemini_adk_rs::live::TurnExtractor;
use gemini_adk_rs::tool::{ToolContext, ToolFunction};
use gemini_adk_rs::{Extract, Frame};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// Compare `actual` with `tests/snapshots/{name}.json`, or write it there when
/// `UPDATE_SNAPSHOTS` is set.
fn assert_snapshot(name: &str, actual: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.json"));
    let rendered = format!("{}\n", serde_json::to_string_pretty(actual).unwrap());
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "no snapshot at {}; run with UPDATE_SNAPSHOTS=1 to create it",
            path.display()
        )
    });
    assert_eq!(
        rendered,
        expected,
        "{name}: the generated schema changed. If intended, rerun with \
         UPDATE_SNAPSHOTS=1 and commit {}",
        path.display()
    );
}

fn tool_snapshot(tool: &dyn ToolFunction) -> Value {
    json!({
        "name": tool.name(),
        "description": tool.description(),
        "parameters": tool.parameters(),
    })
}

/// A seat preference.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum Seating {
    /// Inside the restaurant.
    Indoor,
    /// On the terrace.
    Outdoor,
}

/// Who the booking is for.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Guest {
    /// Full name.
    name: String,
    /// A phone number to call back.
    phone: Option<String>,
}

/// Get the current weather for a city.
///
/// # Arguments
///
/// * `city` - The city name, e.g. "Paris".
/// * `units` - "metric" or "imperial"; metric when omitted.
#[tool]
async fn get_weather(city: String, units: Option<String>) -> Result<Value, ToolError> {
    Ok(json!({ "city": city, "units": units }))
}

/// Book a table.
///
/// # Arguments
///
/// * `party_size` - How many people.
/// * `seating` - Where to seat them.
/// * `guest` - Who the booking is for.
/// * `requests` - Special requests, one per item.
#[tool]
async fn book_table(
    party_size: u8,
    seating: Option<Seating>,
    guest: Guest,
    requests: Vec<String>,
    ctx: ToolContext,
) -> Value {
    let _ = (party_size, seating, guest, requests, ctx);
    Value::Null
}

/// Take no arguments.
#[tool]
async fn ping() -> &'static str {
    "pong"
}

#[derive(Extract)]
#[extract(name = "order", window = 2)]
#[allow(dead_code)]
struct Order {
    #[recognize(integer_near = ["want", "get"])]
    quantity: Option<i64>,
    #[recognize(one_of = ["pizza", "salad", "soda"])]
    item: Option<String>,
    #[recognize(datetime)]
    #[extract(state = "when")]
    pickup: Option<Value>,
    #[recognize(yes_no)]
    confirmed: Option<bool>,
}

#[derive(Frame)]
#[frame(name = "booking")]
#[allow(dead_code)]
struct Booking {
    #[slot(
        prompt = "For how many people?",
        confirm = "low_confidence",
        min = 1,
        max = 12
    )]
    #[recognize(integer_near = ["people", "guests", "party"])]
    party_size: u8,
    #[slot(
        prompt = "What day and time?",
        reprompt = "When would you like to come in?"
    )]
    #[slot(state = "when")]
    #[recognize(datetime)]
    slot: String,
    #[slot(prompt = "Name for the reservation?", pii)]
    name: String,
}

#[test]
fn tool_with_documented_arguments() {
    assert_snapshot("tool_get_weather", &tool_snapshot(&get_weather()));
}

#[test]
fn tool_with_nested_types_and_a_context_parameter() {
    let snapshot = tool_snapshot(&book_table());
    assert!(
        snapshot["parameters"]["properties"].get("ctx").is_none(),
        "the context parameter is not part of the schema"
    );
    assert_snapshot("tool_book_table", &snapshot);
}

#[test]
fn tool_without_arguments() {
    assert_snapshot("tool_ping", &tool_snapshot(&ping()));
}

#[test]
fn extract_record() {
    let extractor = RecordExtractor::new(Order::extract());
    let promotions: Vec<Value> = extractor
        .promotion_rules()
        .iter()
        .map(|p| {
            json!({
                "field": p.field,
                "state_key": p.state_key,
                "merge": format!("{:?}", p.merge),
                "has_predicate": p.accept.is_some(),
            })
        })
        .collect();
    assert_snapshot(
        "extract_order",
        &json!({
            "name": extractor.name(),
            "window": extractor.window_size(),
            "promotions": promotions,
        }),
    );
}

#[test]
fn frame_spec() {
    assert_snapshot(
        "frame_booking",
        &serde_json::to_value(Booking::frame()).unwrap(),
    );
}
