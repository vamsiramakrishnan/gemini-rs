//! # 10 — Guards (G:: module)
//!
//! Demonstrates the G:: guard composition module for validating agent output.
//! Guards compose with `|` and all must pass for output to be accepted.
//!
//! Key concepts:
//! - `G::json()` — validate that output is valid JSON
//! - `G::length(min, max)` — validate output length bounds
//! - `G::pii()` — detect potential PII (email, phone patterns)
//! - `G::topic(deny_list)` — block output mentioning denied topics
//! - `G::budget(max_tokens)` — enforce an estimated token budget
//! - `G::custom(fn)` — arbitrary validation logic
//! - `|` operator — compose guards (all must pass)
//! - `.check()` — validate a single guard
//! - `.check_all()` — validate a composite, returning all violations

use gemini_adk_fluent_rs::prelude::*;

fn main() {
    println!("=== 10: Guards (G::) ===\n");

    // ── G::json — output must be valid JSON ──
    let json_guard = G::json();
    println!("G::json():");
    println!(
        "  Valid JSON:   {:?}",
        json_guard.check(r#"{"key": "value"}"#)
    );
    println!("  Invalid JSON: {:?}", json_guard.check("not json at all"));

    // ── G::length — output must be within bounds ──
    let length_guard = G::length(10, 100);
    println!("\nG::length(10, 100):");
    println!("  'hello':             {:?}", length_guard.check("hello"));
    println!(
        "  50 chars:            {:?}",
        length_guard.check(&"x".repeat(50))
    );
    println!(
        "  200 chars:           {:?}",
        length_guard.check(&"x".repeat(200))
    );

    // ── G::pii — detect potential PII ──
    let pii_guard = G::pii();
    println!("\nG::pii():");
    println!(
        "  Clean text:          {:?}",
        pii_guard.check("The weather is nice today.")
    );
    println!(
        "  With email:          {:?}",
        pii_guard.check("Contact user@example.com for details.")
    );

    // ── G::topic — block denied topics ──
    let topic_guard = G::topic(&["violence", "gambling", "drugs"]);
    println!("\nG::topic([violence, gambling, drugs]):");
    println!(
        "  Safe text:           {:?}",
        topic_guard.check("A beautiful day in the park.")
    );
    println!(
        "  Mentions violence:   {:?}",
        topic_guard.check("The scene depicted violence.")
    );
    println!(
        "  Mentions gambling:   {:?}",
        topic_guard.check("He went gambling at the casino.")
    );

    // ── G::budget — estimated token budget ──
    let budget_guard = G::budget(50);
    println!("\nG::budget(50):");
    println!(
        "  Short text:          {:?}",
        budget_guard.check("Brief answer.")
    );
    println!(
        "  Long text:           {:?}",
        budget_guard.check(&"word ".repeat(100))
    );

    // ── G::regex — block forbidden patterns ──
    let regex_guard = G::regex("password");
    println!("\nG::regex('password'):");
    println!(
        "  Clean:               {:?}",
        regex_guard.check("Your account is active.")
    );
    println!(
        "  Contains 'password': {:?}",
        regex_guard.check("Your password is 12345.")
    );

    // ── G::custom — arbitrary validation ──
    let custom_guard = G::custom(|output| {
        if output.lines().count() > 5 {
            Err(format!(
                "Output has {} lines, max 5 allowed",
                output.lines().count()
            ))
        } else {
            Ok(())
        }
    });
    println!("\nG::custom (max 5 lines):");
    println!(
        "  3 lines:             {:?}",
        custom_guard.check("line1\nline2\nline3")
    );
    println!(
        "  7 lines:             {:?}",
        custom_guard.check("1\n2\n3\n4\n5\n6\n7")
    );

    // ── Composing guards with | ──
    // When composed, all guards must pass. check_all() returns all violations.
    println!("\n--- Composed guards ---");
    let composite = G::json() | G::length(1, 500) | G::pii();
    println!(
        "Composite: json | length(1,500) | pii = {} guards",
        composite.len()
    );

    // Test against valid JSON, correct length, no PII.
    let good_output = r#"{"analysis": "Revenue grew 15% in Q3."}"#;
    let violations = composite.check_all(good_output);
    println!("\nGood output violations: {:?}", violations);
    assert!(violations.is_empty());

    // Test against invalid JSON with PII.
    let bad_output = "Contact alice@example.com for the report that is definitely not JSON.";
    let violations = composite.check_all(bad_output);
    println!("Bad output violations:  {:?}", violations);
    assert!(!violations.is_empty());

    // ── Safety guardrails composite ──
    let safety = G::pii()
        | G::topic(&["violence", "self-harm", "illegal"])
        | G::length(1, 2000)
        | G::budget(500)
        | G::custom(|output| {
            if output.to_uppercase() == output && output.len() > 20 {
                Err("Output appears to be ALL CAPS (shouting)".into())
            } else {
                Ok(())
            }
        });

    println!("\nSafety guardrails: {} guards", safety.len());
    println!(
        "  Normal text:  {} violations",
        safety.check_all("A helpful response.").len()
    );
    println!(
        "  Risky text:   {} violations",
        safety
            .check_all("Contact me about illegal drugs at user@evil.com for violence tips.")
            .len()
    );

    println!("\nDone.");
}
