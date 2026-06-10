//! # 02 — Combinators: Sequential, Parallel & Loop
//!
//! The operator algebra for composing agents into topologies:
//!
//! - `a >> b` — **sequential** pipeline (state flows a → b).
//! - `a | b` — **parallel** fan-out (branches run concurrently, results merge).
//! - `a * N` / `a * until(pred)` — **loop** (fixed count or until a predicate).
//!
//! Plus the pre-built patterns (`review_loop`, `fan_out_merge`, `supervised`)
//! and build-time contract validation (`check_contracts`).
//!
//! Construction-only (no network), so it runs without credentials.

use gemini_adk_fluent_rs::prelude::*;

fn main() {
    println!("=== 02: Combinators ===\n");
    sequential();
    parallel();
    loops();
    contracts();
    println!("\nDone.");
}

// ─────────────────────────────────────────────────────────────────────────────
// Sequential — the >> operator
// ─────────────────────────────────────────────────────────────────────────────
fn sequential() {
    println!("── Sequential (>>) ────────────────────────────────────\n");

    let researcher = AgentBuilder::new("researcher")
        .instruction("Research the topic thoroughly. Cite sources.")
        .google_search()
        .temperature(0.3)
        .writes("findings");
    let writer = AgentBuilder::new("writer")
        .instruction("Write a well-structured article from the findings.")
        .text_only()
        .reads("findings")
        .writes("draft");
    let editor = AgentBuilder::new("editor")
        .instruction("Polish the draft for publication.")
        .text_only()
        .reads("draft")
        .writes("final_article");

    // The >> operator creates a Composable::Pipeline.
    let pipeline = researcher.clone() >> writer.clone() >> editor.clone();
    println!("Pipeline: researcher >> writer >> editor");
    if let Composable::Pipeline(p) = &pipeline {
        println!("  {} steps", p.steps.len());
        for (i, step) in p.steps.iter().enumerate() {
            if let Composable::Agent(a) = step {
                println!("    Step {}: {}", i + 1, a.name());
            }
        }
    }

    // review_loop(worker, reviewer, max) nests a write/review loop in a pipeline.
    let reviewer = AgentBuilder::new("reviewer")
        .instruction("Review the draft. Set quality to 'good' when satisfied.")
        .text_only()
        .reads("draft")
        .writes("quality");
    let reviewed =
        researcher.clone() >> review_loop(writer.clone(), reviewer.clone(), 3) >> editor.clone();
    println!("\nWith review loop: researcher >> review_loop(writer, reviewer, 3) >> editor");
    if let Composable::Pipeline(p) = &reviewed {
        println!("  {} steps\n", p.steps.len());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Parallel — the | operator
// ─────────────────────────────────────────────────────────────────────────────
fn parallel() {
    println!("── Parallel fan-out (|) ───────────────────────────────\n");

    let technical = AgentBuilder::new("technical-researcher")
        .instruction("Research technical aspects.")
        .google_search()
        .writes("technical_findings");
    let market = AgentBuilder::new("market-researcher")
        .instruction("Research market trends.")
        .google_search()
        .writes("market_findings");
    let social = AgentBuilder::new("social-researcher")
        .instruction("Research social impact.")
        .writes("social_findings");

    // The | operator creates a Composable::FanOut; branches run concurrently.
    let fan_out = technical.clone() | market.clone() | social.clone();
    println!("Fan-out: technical | market | social");
    if let Composable::FanOut(f) = &fan_out {
        println!("  {} branches", f.branches.len());
    }

    // Fan-out then reduce: feed all branch outputs into a synthesizer.
    let synthesizer = AgentBuilder::new("synthesizer")
        .instruction("Combine all findings into a report.")
        .text_only()
        .reads("technical_findings")
        .reads("market_findings")
        .reads("social_findings")
        .writes("report");
    let research = (technical.clone() | market.clone() | social.clone()) >> synthesizer.clone();
    println!("\nFan-out >> reduce: (tech | market | social) >> synthesizer");
    if let Composable::Pipeline(p) = &research {
        println!("  {} pipeline steps", p.steps.len());
    }

    // fan_out_merge(branches, reducer) is shorthand for the same shape.
    let _merged = fan_out_merge(vec![technical, market, social], synthesizer);
    println!("fan_out_merge: 3 researchers -> synthesizer\n");
}

// ─────────────────────────────────────────────────────────────────────────────
// Loops — the * operator
// ─────────────────────────────────────────────────────────────────────────────
fn loops() {
    println!("── Loops (*) ──────────────────────────────────────────\n");

    let refiner = AgentBuilder::new("refiner")
        .instruction("Improve the draft. Each pass fixes remaining issues.")
        .text_only()
        .temperature(0.4);

    // Fixed loop: run exactly N times.
    let polished = refiner.clone() * 3;
    println!("Fixed loop: refiner * 3");
    if let Composable::Loop(l) = &polished {
        println!("  max={}, predicate={}", l.max, l.until.is_some());
    }

    // Conditional loop: run until a state predicate holds (with a safety cap).
    let iterator = AgentBuilder::new("iterator")
        .instruction("Iterate. Set 'converged' to true when done.")
        .text_only()
        .writes("converged");
    let converging = iterator
        * until(|s| {
            s.get("converged")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });
    println!("\nConditional loop: iterator * until(converged == true)");
    if let Composable::Loop(l) = &converging {
        println!(
            "  max={} (safety cap), predicate={}",
            l.max,
            l.until.is_some()
        );
    }

    // Loops compose with pipelines.
    let researcher = AgentBuilder::new("researcher")
        .instruction("Research the topic.")
        .writes("findings");
    let editor = AgentBuilder::new("editor")
        .instruction("Final polish.")
        .reads("findings")
        .writes("article");
    let full = researcher >> (refiner.clone() * 3) >> editor;
    println!("\nPipeline with loop: researcher >> (refiner * 3) >> editor");
    if let Composable::Pipeline(p) = &full {
        println!("  {} steps", p.steps.len());
    }

    // Pre-built supervised-iteration patterns.
    let writer = AgentBuilder::new("writer")
        .instruction("Write a draft.")
        .writes("draft");
    let reviewer = AgentBuilder::new("reviewer")
        .instruction("Review. Set quality to 'good' when satisfied.")
        .reads("draft")
        .writes("quality");
    let _reviewed = review_loop(writer.clone(), reviewer.clone(), 5);
    let _supervised = supervised(writer, reviewer, 3);
    println!("\nPatterns: review_loop(writer, reviewer, 5), supervised(writer, reviewer, 3)\n");
}

// ─────────────────────────────────────────────────────────────────────────────
// Contracts — build-time reads/writes validation
// ─────────────────────────────────────────────────────────────────────────────
fn contracts() {
    println!("── Contract validation ────────────────────────────────\n");

    let researcher = AgentBuilder::new("researcher")
        .instruction("Research.")
        .writes("findings");
    let writer = AgentBuilder::new("writer")
        .instruction("Write.")
        .reads("findings")
        .writes("draft");
    let editor = AgentBuilder::new("editor")
        .instruction("Edit.")
        .reads("draft")
        .writes("final");

    // check_contracts() verifies reads/writes are wired up correctly.
    let violations = check_contracts(&[researcher, writer, editor]);
    println!("{} violation(s)", violations.len());
    for v in &violations {
        match v {
            ContractViolation::UnproducedKey { consumer, key } => {
                println!("  UNPRODUCED: '{consumer}' reads '{key}' but nobody writes it");
            }
            ContractViolation::DuplicateWrite { agents, key } => {
                println!("  DUPLICATE: '{key}' written by {agents:?}");
            }
            ContractViolation::OrphanedOutput { producer, key } => {
                println!("  ORPHANED: '{producer}' writes '{key}' but nobody reads it");
            }
        }
    }
}
