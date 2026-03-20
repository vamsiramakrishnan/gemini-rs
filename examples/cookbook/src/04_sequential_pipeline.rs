//! # 04 — Sequential Pipeline (>> operator)
//!
//! Demonstrates composing agents into a sequential pipeline using the `>>`
//! operator. Each agent runs in order, with state flowing between them.
//!
//! Key concepts:
//! - `agent_a >> agent_b` — run a then b sequentially
//! - `agent_a >> agent_b >> agent_c` — chains of arbitrary length
//! - `Composable::Pipeline` — the underlying type produced by `>>`
//! - `review_loop()` — a pre-built pattern that uses sequential + loop

use gemini_adk_fluent_rs::prelude::*;

fn main() {
    println!("=== 04: Sequential Pipeline (>>) ===\n");

    // ── Define individual agents ──
    let researcher = AgentBuilder::new("researcher")
        .instruction("Research the topic thoroughly. Cite sources.")
        .google_search()
        .temperature(0.3)
        .writes("findings")
        .writes("sources");

    let writer = AgentBuilder::new("writer")
        .instruction("Write a well-structured article from the findings.")
        .text_only()
        .temperature(0.7)
        .reads("findings")
        .writes("draft");

    let editor = AgentBuilder::new("editor")
        .instruction("Polish the draft for publication. Fix grammar and flow.")
        .text_only()
        .reads("draft")
        .writes("final_article");

    // ── Simple two-step pipeline ──
    // The >> operator creates a Composable::Pipeline.
    let _simple = researcher.clone() >> writer.clone();
    println!("Two-step pipeline: researcher >> writer");

    // ── Three-step pipeline ──
    let pipeline = researcher.clone() >> writer.clone() >> editor.clone();
    println!("Three-step pipeline: researcher >> writer >> editor");

    // Inspect the resulting Composable structure.
    match &pipeline {
        Composable::Pipeline(p) => {
            println!("  Pipeline has {} steps", p.steps.len());
            for (i, step) in p.steps.iter().enumerate() {
                if let Composable::Agent(a) = step {
                    println!("    Step {}: {}", i + 1, a.name());
                }
            }
        }
        _ => println!("  (unexpected composable variant)"),
    }

    // ── Pre-built pattern: review_loop ──
    // review_loop(worker, reviewer, max_rounds) creates:
    //   worker >> reviewer, looping until reviewer is satisfied or max_rounds.
    let reviewer = AgentBuilder::new("reviewer")
        .instruction("Review the draft. Set quality to 'good' when satisfied.")
        .text_only()
        .thinking(2048)
        .reads("draft")
        .writes("quality")
        .writes("feedback");

    let reviewed_pipeline =
        researcher.clone() >> review_loop(writer.clone(), reviewer.clone(), 3) >> editor.clone();

    println!("\nReview pipeline: researcher >> review_loop(writer, reviewer, 3) >> editor");
    if let Composable::Pipeline(p) = &reviewed_pipeline {
        println!("  Total steps: {}", p.steps.len());
    }

    // ── Contract validation ──
    // check_contracts() verifies that reads/writes are wired up correctly.
    let all_agents = [researcher, writer, reviewer, editor];
    let violations = check_contracts(&all_agents);
    println!("\nContract validation: {} violation(s)", violations.len());
    for v in &violations {
        match v {
            ContractViolation::UnproducedKey { consumer, key } => {
                println!(
                    "  UNPRODUCED: '{}' reads '{}' but nobody writes it",
                    consumer, key
                );
            }
            ContractViolation::DuplicateWrite { agents, key } => {
                println!("  DUPLICATE: '{}' written by {:?}", key, agents);
            }
            ContractViolation::OrphanedOutput { producer, key } => {
                println!(
                    "  ORPHANED: '{}' writes '{}' but nobody reads it",
                    producer, key
                );
            }
        }
    }

    println!("\nDone.");
}
