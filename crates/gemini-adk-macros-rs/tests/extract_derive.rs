//! Integration tests for the `#[derive(Extract)]` macro.
//!
//! Proc-macro crates can only be tested through a downstream crate, so these
//! live in `tests/` (not `#[cfg(test)] mod`). They exercise the generated code
//! against the real `gemini-adk-rs` crate graph.

use gemini_adk_rs::extract::RecordExtractor;
use gemini_adk_rs::live::TranscriptTurn;
use gemini_adk_rs::live::TurnExtractor;
use gemini_adk_rs::Extract; // the derive macro (macro namespace)
use serde_json::{json, Value};

#[derive(Extract)]
#[extract(name = "order", window = 2)]
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
    // No `#[recognize]` — ignored by the derive, but still marked used.
    note: Option<String>,
}

fn turn(user: &str) -> TranscriptTurn {
    TranscriptTurn {
        turn_number: 0,
        user: user.to_string(),
        model: String::new(),
        tool_calls: Vec::new(),
        timestamp: std::time::Instant::now(),
    }
}

#[test]
fn derived_record_has_name_window_and_fields() {
    let record = Order::extract();
    let ext = RecordExtractor::new(record);
    assert_eq!(ext.name(), "order");
    assert_eq!(ext.window_size(), 2);
    // Four recognized fields (note has no recognizer).
    assert_eq!(ext.promotion_rules().len(), 4);
    // The custom state key from `#[extract(state = "when")]` is honored.
    assert!(ext.promotion_rules().iter().any(|p| p.state_key == "when"));
}

#[tokio::test]
async fn derived_record_extracts_fields() {
    let ext = RecordExtractor::new(Order::extract());
    let window = vec![turn("yes I want 2 pizza tomorrow at 6pm")];
    let out = ext.extract(&window).await.unwrap();
    assert_eq!(out["quantity"], json!(2));
    assert_eq!(out["item"], json!("pizza"));
    assert_eq!(out["confirmed"], json!(true));
    assert_eq!(out["pickup"], json!({ "day": "tomorrow", "time": "18:00" }));
}
