//! Walk Example 13: Review Loop — Author + Reviewer Iterative Refinement
//!
//! Demonstrates the review_loop pattern where an author agent produces drafts
//! and a reviewer agent evaluates them. The loop repeats until the reviewer
//! sets "approved" to true in state, or until max_rounds is reached.
//!
//! Features used:
//!   - review_loop() pattern (author + reviewer)
//!   - review_loop_keyed() (custom quality key + target value)
//!   - LoopTextAgent (L1 runtime)
//!   - FnTextAgent (mock agents)
//!   - State (shared state for inter-agent communication)
//!   - `>>` operator (sequential pipeline)
//!   - `*` operator with until() predicate

use std::sync::Arc;

use gemini_adk_fluent::prelude::*;

#[tokio::main]
async fn main() {
    println!("=== Walk 13: Review Loop — Iterative Refinement ===\n");

    // ── Part 1: Manual review loop with FnTextAgent ──────────────────────
    // Build the pattern from scratch to understand the mechanics.

    println!("--- Part 1: Manual Review Loop ---");

    // Author agent: writes a draft, improving each iteration
    let author: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("author", |state| {
        let iteration = state.modify("draft_count", 0u32, |n| n + 1);
        let feedback = state
            .get::<String>("feedback")
            .unwrap_or_else(|| "none yet".into());

        let draft = format!(
            "Draft v{iteration}: A comprehensive guide to Rust's ownership system. \
             [Incorporating feedback: {feedback}]"
        );
        state.set("current_draft", &draft);
        println!("  Author wrote: {draft}");
        Ok(draft)
    }));

    // Reviewer agent: evaluates the draft and either approves or gives feedback
    let reviewer: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("reviewer", |state| {
        let draft_count: u32 = state.get("draft_count").unwrap_or(0);
        let _draft = state.get::<String>("current_draft").unwrap_or_default();

        // Simulate improving quality — approve after 3 iterations
        if draft_count >= 3 {
            state.set("approved", true);
            let review =
                format!("APPROVED: Draft meets quality standards after {draft_count} revisions.");
            println!("  Reviewer: {review}");
            Ok(review)
        } else {
            state.set("approved", false);
            let feedback =
                format!("Needs more detail on borrowing rules (iteration {draft_count})");
            state.set("feedback", &feedback);
            let review = format!("REVISION NEEDED: {feedback}");
            println!("  Reviewer: {review}");
            Ok(review)
        }
    }));

    // Create a loop that runs author >> reviewer until approved
    let pipeline = SequentialTextAgent::new("author_reviewer", vec![author, reviewer]);

    let loop_agent = LoopTextAgent::new("review_loop", Arc::new(pipeline), 5)
        .until(|state| state.get::<bool>("approved").unwrap_or(false));

    let state = State::new();
    let result = loop_agent.run(&state).await.unwrap();
    println!("  Final result: {result}");
    println!(
        "  Total drafts: {}",
        state.get::<u32>("draft_count").unwrap_or(0)
    );
    println!(
        "  Approved: {}",
        state.get::<bool>("approved").unwrap_or(false)
    );

    // ── Part 2: Using review_loop() pattern helper ───────────────────────
    // The pattern helper creates the same structure declaratively.

    println!("\n--- Part 2: review_loop() Pattern ---");

    let workflow = review_loop(
        AgentBuilder::new("essay_writer").instruction("Write an essay on climate change"),
        AgentBuilder::new("editor").instruction(
            "Review the essay. Set approved=true if publication-ready, \
             otherwise provide specific feedback.",
        ),
        4, // max 4 rounds
    );

    // Inspect the structure
    match &workflow {
        Composable::Loop(l) => {
            println!("  Max iterations: {}", l.max);
            println!("  Has termination predicate: {}", l.until.is_some());
            if let Composable::Pipeline(p) = &*l.body {
                println!("  Pipeline steps: {}", p.steps.len());
                for step in &p.steps {
                    if let Composable::Agent(a) = step {
                        println!("    - {}", a.name());
                    }
                }
            }
        }
        _ => unreachable!(),
    }

    // Test the predicate
    if let Composable::Loop(l) = &workflow {
        if let Some(pred) = &l.until {
            println!(
                "  Predicate(approved=false): {}",
                pred.check(&serde_json::json!({"approved": false}))
            );
            println!(
                "  Predicate(approved=true):  {}",
                pred.check(&serde_json::json!({"approved": true}))
            );
        }
    }

    // ── Part 3: review_loop_keyed() with custom quality key ──────────────

    println!("\n--- Part 3: review_loop_keyed() ---");

    let keyed_workflow = review_loop_keyed(
        AgentBuilder::new("coder").instruction("Write a sorting algorithm"),
        AgentBuilder::new("qa_engineer")
            .instruction("Review the code. Set quality='production' when ready."),
        "quality",    // custom state key
        "production", // target value
        5,
    );

    if let Composable::Loop(l) = &keyed_workflow {
        if let Some(pred) = &l.until {
            println!(
                "  quality='draft':      {}",
                pred.check(&serde_json::json!({"quality": "draft"}))
            );
            println!(
                "  quality='production': {}",
                pred.check(&serde_json::json!({"quality": "production"}))
            );
        }
    }

    // ── Part 4: Using the * operator with until() ────────────────────────

    println!("\n--- Part 4: * Operator with until() ---");

    let refiner = AgentBuilder::new("refiner").instruction("Polish the text");
    let converge = refiner
        * until(|v| {
            v.get("score")
                .and_then(|s| s.as_f64())
                .map(|s| s >= 0.95)
                .unwrap_or(false)
        });

    match &converge {
        Composable::Loop(l) => {
            println!("  Max iterations: {} (u32::MAX = unbounded)", l.max);
            println!("  Converges when score >= 0.95");
            if let Some(pred) = &l.until {
                println!(
                    "  score=0.5: {}",
                    pred.check(&serde_json::json!({"score": 0.5}))
                );
                println!(
                    "  score=0.99: {}",
                    pred.check(&serde_json::json!({"score": 0.99}))
                );
            }
        }
        _ => unreachable!(),
    }

    // ── Part 5: Fixed loop with * n ──────────────────────────────────────

    println!("\n--- Part 5: Fixed Loop ---");

    let polisher = AgentBuilder::new("polisher").instruction("Polish the draft") * 3;

    match &polisher {
        Composable::Loop(l) => {
            println!("  Fixed loop: {} iterations", l.max);
            println!("  No early-exit predicate: {}", l.until.is_none());
        }
        _ => unreachable!(),
    }

    println!("\nDone.");
}
