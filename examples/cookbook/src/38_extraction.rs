//! # 38 — Deterministic Extraction (no model)  (Tier: Run)
//!
//! What it teaches: pull typed fields out of the transcript with CPU
//! recognizers (no LLM, no network, no accelerator), and feed them straight
//! into a `Flow` guard so a stage advances deterministically.
//!
//! Key concepts:
//! - `Recognizer` — `integer`/`money`/`one_of`/`fuzzy`/`yes_no`/`regex`/`datetime`
//! - `#[derive(Extract)]` — declare a typed record of recognized fields on a struct
//! - `.extract_record(spec)` on `Live` + `Flow` `done(captured([..]))` interplay
//!
//! Runs real logic: Yes — runs recognizers on sample utterances and drives a
//! Flow step to completion from the recognized fields (no credentials needed).

use gemini_adk_fluent_rs::prelude::*;
use serde_json::{json, Value};

/// Declare the record as a struct — each field names a deterministic recognizer.
/// The derive generates `Order::extract() -> Extract`.
#[derive(Extract)]
#[extract(name = "order", window = 3)]
struct Order {
    #[recognize(integer_near = ["order", "want"])]
    quantity: Option<i64>,
    #[recognize(one_of = ["pizza", "salad", "soda"])]
    item: Option<String>,
    #[recognize(datetime)]
    #[extract(state = "when")]
    pickup: Option<Value>,
    #[recognize(yes_no)]
    confirmed: Option<bool>,
}

fn main() {
    println!("=== 38: Deterministic Extraction ===\n");

    // 1. Recognizers run on the CPU over what the user said.
    println!("--- Recognizers on a sample utterance ---");
    let utterance = "yeah I'd like to order 2 large pizzas, name is Jonson";
    for (label, rec) in [
        (
            "quantity",
            Recognizer::integer_near(["order", "want", "like"]),
        ),
        ("item", Recognizer::one_of(["pizza", "salad", "soda"])),
        ("name", Recognizer::fuzzy(["Johnson", "Jackson", "Jensen"])),
        ("confirmed", Recognizer::yes_no()),
    ] {
        match rec.recognize(utterance) {
            Some((v, conf)) => println!("    {label:10} = {v}   (confidence {conf:.2})"),
            None => println!("    {label:10} = <none>"),
        }
    }

    // 2. Declare a record once (via `#[derive(Extract)]`); it compiles to a
    //    TurnExtractor that runs the recognizers and promotes fields to State.
    let order = Order::extract();

    println!("\n--- In a live session ---");
    println!("    Live::builder()");
    println!("        .extract_record(order)        // deterministic, no model");
    println!("        .govern(order_flow)           // Flow reads done(captured([..]))");
    println!("        .connect_from_env().await?;");

    // 3. The multiplicative payoff: extracted fields drive a Flow guard.
    let flow = Flow::new()
        .step("take_order")
        .posture("Take the customer's order.")
        .done(Guard::captured(["quantity", "item"])) // ← filled by the recognizers
        .step("confirm")
        .after("take_order")
        .done(Guard::is_true("confirmed"))
        .step("done")
        .after("confirm")
        .terminal()
        .require(["done"])
        .build()
        .expect("valid flow");

    let mut mon = FlowMonitor::new(flow, FlowMode::Enforce);
    let state = State::new();

    println!("\n--- Flow before extraction ---");
    println!("    take_order: {:?}", mon.verdict("take_order", &state));

    // Simulate the RecordExtractor promoting recognized fields into State.
    let _ = order; // (registered on Live in a real session; here we apply the result)
    state.set("quantity", json!(2));
    state.set("item", json!("pizza"));
    mon.on_turn(&state);
    println!("\n--- After deterministic extraction (quantity + item) ---");
    println!("    take_order: {:?}", mon.verdict("take_order", &state));
    println!("    confirm:    {:?}", mon.verdict("confirm", &state));

    state.set("confirmed", true);
    mon.on_turn(&state);
    println!("\n--- After confirmation ---");
    println!("    confirm:    {:?}", mon.verdict("confirm", &state));
    println!(
        "    flow complete: {}  (no LLM involved)",
        mon.is_complete()
    );

    println!("\ndeterministic extraction example completed successfully!");
}
