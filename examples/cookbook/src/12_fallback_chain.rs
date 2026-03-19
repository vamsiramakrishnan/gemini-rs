//! Walk Example 12: Fallback Chain — The `/` Operator and Cascade Pattern
//!
//! Demonstrates the fallback chain pattern where agents are tried in sequence
//! until one succeeds. This provides resilience: if a primary agent fails or
//! produces an error, the system automatically falls through to backup agents.
//!
//! Features used:
//!   - `/` operator (Div) for fallback composition
//!   - cascade() pattern helper
//!   - FallbackTextAgent (L1 runtime type)
//!   - Composable::Fallback (operator algebra node)
//!   - FnTextAgent for mock agents

use std::sync::Arc;

use gemini_adk_fluent::prelude::*;

#[tokio::main]
async fn main() {
    println!("=== Walk 12: Fallback Chain — / Operator and Cascade ===\n");

    // ── Using the `/` operator directly ──────────────────────────────────
    // The `/` operator creates a Composable::Fallback. When compiled, it
    // produces a FallbackTextAgent that tries each candidate in order.

    println!("--- Part 1: The / Operator ---");

    // Primary agent: simulates a premium API that sometimes fails
    let primary = FnTextAgent::new("premium_api", |state| {
        let query = state
            .get::<String>("input")
            .unwrap_or_else(|| "test".into());
        // Simulate failure when query contains "fail"
        if query.contains("fail") {
            Err(gemini_adk::error::AgentError::Other(
                "Premium API unavailable".into(),
            ))
        } else {
            Ok(format!("[Premium] High-quality answer for: {query}"))
        }
    });

    // Secondary agent: always available but lower quality
    let secondary = FnTextAgent::new("standard_api", |state| {
        let query = state
            .get::<String>("input")
            .unwrap_or_else(|| "test".into());
        Ok(format!("[Standard] Basic answer for: {query}"))
    });

    // Tertiary agent: last resort
    let tertiary = FnTextAgent::new("cached_response", |_state| {
        Ok("[Cache] Here is a cached response from our knowledge base.".to_string())
    });

    // Build a 3-level fallback chain using FallbackTextAgent directly
    let fallback_agent = FallbackTextAgent::new(
        "resilient_answerer",
        vec![
            Arc::new(primary) as Arc<dyn TextAgent>,
            Arc::new(secondary) as Arc<dyn TextAgent>,
            Arc::new(tertiary) as Arc<dyn TextAgent>,
        ],
    );

    // Test with a successful query
    let state = State::new();
    state.set("input", "What is Rust?");
    let result = fallback_agent.run(&state).await.unwrap();
    println!("  Success case: {result}");

    // Test with a failing query — should fall through to secondary
    let state = State::new();
    state.set("input", "Please fail gracefully");
    let result = fallback_agent.run(&state).await.unwrap();
    println!("  Fallback case: {result}");

    // ── Using the cascade() pattern helper ───────────────────────────────
    // The cascade() function creates a Composable::Fallback from AgentBuilders.
    // This is useful when you want to define agents with builder syntax.

    println!("\n--- Part 2: cascade() Pattern ---");

    let robust_pipeline = cascade(vec![
        AgentBuilder::new("fast_model")
            .instruction("Give a quick answer")
            .temperature(0.1),
        AgentBuilder::new("thorough_model")
            .instruction("Give a detailed answer")
            .temperature(0.5),
        AgentBuilder::new("fallback_model")
            .instruction("Give any reasonable answer")
            .temperature(0.9),
    ]);

    // Inspect the structure
    match &robust_pipeline {
        Composable::Fallback(f) => {
            println!("  Cascade has {} candidates:", f.candidates.len());
            for (i, candidate) in f.candidates.iter().enumerate() {
                if let Composable::Agent(builder) = candidate {
                    println!(
                        "    {}: {} (temp={:?})",
                        i + 1,
                        builder.name(),
                        builder.get_temperature()
                    );
                }
            }
        }
        _ => unreachable!(),
    }

    // ── Using the `/` operator with AgentBuilder ─────────────────────────
    // The operator algebra lets you write concise fallback chains.

    println!("\n--- Part 3: Operator Algebra ---");

    let chain = AgentBuilder::new("gpt4_equivalent").instruction("Premium response")
        / AgentBuilder::new("gpt35_equivalent").instruction("Standard response")
        / AgentBuilder::new("rule_based").instruction("Template response");

    match &chain {
        Composable::Fallback(f) => {
            println!("  Operator chain has {} candidates", f.candidates.len());
            // The `/` operator flattens, so we get 3 candidates, not nested
            assert_eq!(f.candidates.len(), 3);
            println!("  Flattening works correctly: 3 candidates (not nested)");
        }
        _ => unreachable!(),
    }

    // ── Combining fallback with pipeline ─────────────────────────────────
    // You can mix operators: pipeline with fallback at a step.

    println!("\n--- Part 4: Fallback Inside Pipeline ---");

    let mixed = AgentBuilder::new("preprocessor").instruction("Clean input")
        >> (AgentBuilder::new("primary_analyzer").instruction("Analyze deeply")
            / AgentBuilder::new("backup_analyzer").instruction("Analyze simply"))
        >> AgentBuilder::new("formatter").instruction("Format output");

    match &mixed {
        Composable::Pipeline(p) => {
            println!("  Pipeline has {} steps:", p.steps.len());
            for (i, step) in p.steps.iter().enumerate() {
                match step {
                    Composable::Agent(a) => println!("    Step {}: Agent({})", i + 1, a.name()),
                    Composable::Fallback(f) => println!(
                        "    Step {}: Fallback({} candidates)",
                        i + 1,
                        f.candidates.len()
                    ),
                    _ => println!("    Step {}: Other", i + 1),
                }
            }
        }
        _ => unreachable!(),
    }

    println!("\nDone.");
}
