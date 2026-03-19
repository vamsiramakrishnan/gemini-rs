//! # 07 — State Transforms (S:: module)
//!
//! Demonstrates the S:: composition module for declarative state
//! manipulation. State transforms operate on `serde_json::Value` objects
//! and compose sequentially with `>>`.
//!
//! Key concepts:
//! - `S::pick()` — keep only specified keys
//! - `S::rename()` — rename keys
//! - `S::merge()` — combine keys into a nested object
//! - `S::compute()` — derive new values from existing state
//! - `S::set()` — set a key to a fixed value
//! - `S::drop()` — remove keys
//! - `S::flatten()` — flatten a nested object into top level
//! - `S::defaults()` — set defaults for missing keys
//! - `S::map()` — apply a custom transformation
//! - `>>` operator — chain transforms sequentially

use gemini_adk_fluent_rs::prelude::*;
use serde_json::json;

fn main() {
    println!("=== 07: State Transforms (S::) ===\n");

    // ── S::pick — keep only selected keys ──
    let mut state =
        json!({"name": "Alice", "age": 30, "email": "alice@example.com", "noise": "ignore"});
    println!("Original state: {}", state);

    S::pick(&["name", "age"]).apply(&mut state);
    println!("After S::pick([name, age]): {}", state);
    assert_eq!(state, json!({"name": "Alice", "age": 30}));

    // ── S::rename — rename keys ──
    let mut state = json!({"old_name": "Bob", "old_score": 95});
    S::rename(&[("old_name", "name"), ("old_score", "score")]).apply(&mut state);
    println!("\nAfter S::rename: {}", state);
    assert_eq!(state, json!({"name": "Bob", "score": 95}));

    // ── S::merge — combine keys into a nested object ──
    let mut state = json!({"city": "NYC", "zip": "10001", "country": "US", "other": "data"});
    S::merge(&["city", "zip", "country"], "address").apply(&mut state);
    println!(
        "\nAfter S::merge([city, zip, country] -> address): {}",
        state
    );

    // ── S::compute — derive new values ──
    let mut state = json!({"price": 100, "quantity": 5});
    S::compute("total", |s| {
        let price = s.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let qty = s.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
        json!(price * qty)
    })
    .apply(&mut state);
    println!("\nAfter S::compute(total = price * quantity): {}", state);
    assert_eq!(state["total"], json!(500.0));

    // ── S::set — set a fixed value ──
    let mut state = json!({"existing": 1});
    S::set("status", json!("active")).apply(&mut state);
    println!("\nAfter S::set(status, active): {}", state);

    // ── S::drop — remove keys ──
    let mut state = json!({"keep": 1, "remove_me": 2, "also_remove": 3});
    S::drop(&["remove_me", "also_remove"]).apply(&mut state);
    println!("\nAfter S::drop([remove_me, also_remove]): {}", state);
    assert_eq!(state, json!({"keep": 1}));

    // ── S::flatten — unnest an object ──
    let mut state = json!({"user": {"name": "Carol", "role": "admin"}, "active": true});
    S::flatten("user").apply(&mut state);
    println!("\nAfter S::flatten(user): {}", state);
    assert_eq!(state["name"], "Carol");
    assert_eq!(state["role"], "admin");

    // ── S::defaults — set missing values only ──
    let mut state = json!({"name": "Dave"});
    S::defaults(json!({"name": "default", "role": "viewer", "theme": "dark"})).apply(&mut state);
    println!("\nAfter S::defaults: {}", state);
    assert_eq!(state["name"], "Dave"); // not overwritten
    assert_eq!(state["role"], "viewer"); // filled in
    assert_eq!(state["theme"], "dark"); // filled in

    // ── Chaining with >> ──
    println!("\n--- Chained transforms ---");
    let chain = S::pick(&["findings", "score"])
        >> S::rename(&[("findings", "research")])
        >> S::compute("grade", |s| {
            let score = s.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if score >= 90.0 {
                json!("A")
            } else if score >= 80.0 {
                json!("B")
            } else {
                json!("C")
            }
        })
        >> S::set("reviewed", json!(true));

    let mut state = json!({"findings": "Quantum computing is...", "score": 92.0, "noise": "x"});
    println!("Before chain: {}", state);
    chain.apply(&mut state);
    println!("After chain:  {}", state);
    assert_eq!(state["research"], "Quantum computing is...");
    assert_eq!(state["grade"], "A");
    assert_eq!(state["reviewed"], true);
    assert!(state.get("noise").is_none()); // removed by pick

    // ── S::map — custom transform ──
    let mut state = json!({"items": ["a", "b", "c"]});
    S::map(|s| {
        let count = s
            .get("items")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        s["item_count"] = json!(count);
    })
    .apply(&mut state);
    println!("\nAfter S::map (count items): {}", state);
    assert_eq!(state["item_count"], 3);

    println!("\nDone.");
}
