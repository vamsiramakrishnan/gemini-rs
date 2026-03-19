//! Fluent pipeline example — using gemini-adk-fluent operator algebra.
//!
//! Demonstrates the builder API, operator composition, composition modules,
//! pre-built patterns, and contract validation.

use gemini_adk_fluent::prelude::*;

fn main() {
    // ── 1. AgentBuilder — copy-on-write fluent construction ──

    let researcher = AgentBuilder::new("researcher")
        .instruction("Find comprehensive information about the topic.")
        .google_search()
        .url_context()
        .temperature(0.3)
        .writes("findings")
        .writes("sources");

    let writer = AgentBuilder::new("writer")
        .instruction("Write a well-structured article based on the findings.")
        .text_only()
        .temperature(0.7)
        .reads("findings")
        .writes("draft");

    let reviewer = AgentBuilder::new("reviewer")
        .instruction("Review the draft for accuracy, clarity, and completeness.")
        .text_only()
        .thinking(2048)
        .reads("draft")
        .writes("quality")
        .writes("feedback");

    // ── 2. Copy-on-write: templates are safe to share ──

    let fast_researcher = researcher.clone().temperature(0.1);
    let creative_writer = writer.clone().temperature(0.9);

    // Original unchanged
    assert_eq!(researcher.get_temperature(), Some(0.3));
    assert_eq!(fast_researcher.get_temperature(), Some(0.1));
    println!("Copy-on-write: originals unchanged after cloning");

    // ── 3. Operator algebra ──

    // Sequential pipeline with >>
    let _simple_pipeline = researcher.clone() >> writer.clone();

    // Parallel fan-out with |
    let _parallel = researcher.clone() | fast_researcher.clone();

    // Fixed loop with *
    let _retry = reviewer.clone() * 3;

    // Fallback with /
    let _fallback = researcher.clone() / fast_researcher.clone();

    // Complex composition
    let _deep_research = researcher.clone() >> writer.clone() >> (reviewer.clone() * 3);

    println!("Operators: >>, |, *, / all working");

    // ── 4. Composition modules ──

    // S — State transforms
    let transform = S::pick(&["findings"]) >> S::rename(&[("findings", "research_data")]);
    let mut state = serde_json::json!({"findings": "data", "noise": "ignore"});
    transform.apply(&mut state);
    assert_eq!(state, serde_json::json!({"research_data": "data"}));

    // P — Prompt composition
    let prompt = P::role("technical writer")
        + P::task("Write a blog post about Rust async programming")
        + P::constraint("Keep it under 1000 words")
        + P::format("Markdown");
    println!("\nComposed prompt:\n{}", prompt.render());

    // M — Middleware composition
    let _middleware = M::log() | M::latency();

    // T — Tool composition
    let _tools = T::google_search() | T::url_context();

    // ── 5. Pre-built patterns ──

    let _review = review_loop(writer.clone(), reviewer.clone(), 5);

    let _cascade = cascade(vec![researcher.clone(), fast_researcher.clone()]);

    let _parallel = fan_out_merge(
        vec![researcher.clone(), creative_writer.clone()],
        writer.clone(),
    );

    let _supervised = supervised(writer.clone(), reviewer.clone(), 3);

    println!("Patterns: review_loop, cascade, fan_out_merge, supervised all working");

    // ── 6. Contract validation ──

    let violations = check_contracts(&[researcher.clone(), writer.clone(), reviewer.clone()]);

    println!("\nContract validation ({} violations):", violations.len());
    for v in &violations {
        match v {
            ContractViolation::UnproducedKey { consumer, key } => {
                println!("  - {consumer} reads '{key}' but nobody produces it");
            }
            ContractViolation::DuplicateWrite { agents, key } => {
                println!("  - Multiple agents write '{key}': {:?}", agents);
            }
            ContractViolation::OrphanedOutput { producer, key } => {
                println!("  - {producer} writes '{key}' but nobody reads it");
            }
        }
    }

    println!("\nAll examples completed successfully!");
}
