//! # 05 — Parallel Fan-Out (| operator)
//!
//! Demonstrates running agents in parallel using the `|` operator.
//! All branches execute concurrently and their results are merged.
//!
//! Key concepts:
//! - `agent_a | agent_b` — run both in parallel, merge results
//! - `Composable::FanOut` — the underlying type produced by `|`
//! - `fan_out_merge()` — pre-built pattern: fan out then merge with a reducer
//! - Combining `|` with `>>` for complex topologies

use adk_rs_fluent::prelude::*;

fn main() {
    println!("=== 05: Parallel Fan-Out (|) ===\n");

    // ── Define specialized agents ──
    let technical_researcher = AgentBuilder::new("technical-researcher")
        .instruction("Research technical aspects of the topic.")
        .google_search()
        .temperature(0.2)
        .writes("technical_findings");

    let market_researcher = AgentBuilder::new("market-researcher")
        .instruction("Research market trends and business aspects.")
        .google_search()
        .temperature(0.3)
        .writes("market_findings");

    let social_researcher = AgentBuilder::new("social-researcher")
        .instruction("Research social impact and public sentiment.")
        .temperature(0.4)
        .writes("social_findings");

    // ── Two-way fan-out ──
    let _two_way = technical_researcher.clone() | market_researcher.clone();
    println!("Two-way fan-out: technical | market");

    // ── Three-way fan-out ──
    let fan_out = technical_researcher.clone()
        | market_researcher.clone()
        | social_researcher.clone();

    println!("Three-way fan-out: technical | market | social");

    match &fan_out {
        Composable::FanOut(f) => {
            println!("  Branches: {}", f.branches.len());
            for (i, branch) in f.branches.iter().enumerate() {
                if let Composable::Agent(a) = branch {
                    println!("    Branch {}: {}", i + 1, a.name());
                }
            }
        }
        _ => println!("  (unexpected composable variant)"),
    }

    // ── Fan-out then reduce (>> after |) ──
    // Run parallel research, then feed all findings into a synthesizer.
    let synthesizer = AgentBuilder::new("synthesizer")
        .instruction("Combine all research findings into a comprehensive report.")
        .text_only()
        .temperature(0.5)
        .reads("technical_findings")
        .reads("market_findings")
        .reads("social_findings")
        .writes("report");

    let research_pipeline = (technical_researcher.clone()
        | market_researcher.clone()
        | social_researcher.clone())
        >> synthesizer.clone();

    println!("\nFan-out >> reduce: (tech | market | social) >> synthesizer");
    if let Composable::Pipeline(p) = &research_pipeline {
        println!("  Pipeline steps: {}", p.steps.len());
    }

    // ── Pre-built pattern: fan_out_merge ──
    // fan_out_merge(branches, reducer) is a shorthand for the above.
    let _merged = fan_out_merge(
        vec![
            technical_researcher.clone(),
            market_researcher.clone(),
            social_researcher.clone(),
        ],
        synthesizer.clone(),
    );
    println!("fan_out_merge: 3 researchers -> synthesizer");

    println!("\nDone.");
}
