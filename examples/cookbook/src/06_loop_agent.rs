//! # 06 — Loop Agent (* operator)
//!
//! Demonstrates repeating an agent a fixed number of times using the `*`
//! operator, or until a condition is met using `until()`.
//!
//! Key concepts:
//! - `agent * N` — run the agent N times (fixed loop)
//! - `agent * until(predicate)` — run until the predicate returns true
//! - `Composable::Loop` — the underlying type
//! - `review_loop()` — pre-built write/review loop pattern
//! - `supervised()` — pre-built supervised iteration pattern

use gemini_adk_fluent_rs::prelude::*;

fn main() {
    println!("=== 06: Loop Agent (*) ===\n");

    // ── Fixed loop ──
    // Run a refiner agent exactly 3 times to polish output.
    let refiner = AgentBuilder::new("refiner")
        .instruction("Improve the draft. Each pass should fix remaining issues.")
        .text_only()
        .temperature(0.4);

    let polished = refiner.clone() * 3;
    println!("Fixed loop: refiner * 3");

    match &polished {
        Composable::Loop(l) => {
            println!("  Max iterations: {}", l.max);
            println!("  Has predicate:  {}", l.until.is_some());
        }
        _ => println!("  (unexpected composable variant)"),
    }

    // ── Conditional loop with until() ──
    // Run until the state indicates convergence.
    let iterator = AgentBuilder::new("iterator")
        .instruction("Iterate on the solution. Set 'converged' to true when done.")
        .text_only()
        .writes("converged");

    let converging = iterator.clone()
        * until(|state| {
            state
                .get("converged")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });
    println!("\nConditional loop: iterator * until(converged == true)");

    if let Composable::Loop(l) = &converging {
        println!("  Max iterations: {} (safety cap)", l.max);
        println!("  Has predicate:  {}", l.until.is_some());
    }

    // ── Combining loop with pipeline ──
    // research >> (refine * 3) >> final edit
    let researcher = AgentBuilder::new("researcher")
        .instruction("Research the topic.")
        .writes("findings");

    let editor = AgentBuilder::new("editor")
        .instruction("Final polish.")
        .reads("findings")
        .writes("article");

    let full = researcher.clone() >> (refiner.clone() * 3) >> editor.clone();
    println!("\nPipeline with loop: researcher >> (refiner * 3) >> editor");
    if let Composable::Pipeline(p) = &full {
        println!("  Pipeline steps: {}", p.steps.len());
    }

    // ── Pre-built pattern: review_loop ──
    let writer = AgentBuilder::new("writer")
        .instruction("Write a draft.")
        .writes("draft");

    let reviewer = AgentBuilder::new("reviewer")
        .instruction("Review the draft. Set quality to 'good' when satisfied.")
        .reads("draft")
        .writes("quality");

    let _reviewed = review_loop(writer.clone(), reviewer.clone(), 5);
    println!("\nreview_loop(writer, reviewer, max=5)");

    // ── Pre-built pattern: supervised ──
    let _supervised = supervised(writer.clone(), reviewer.clone(), 3);
    println!("supervised(writer, reviewer, max=3)");

    println!("\nDone.");
}
