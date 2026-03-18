//! Research pipeline example — demonstrates the fluent DX layer.
//!
//! Shows how to:
//! - Build agents with AgentBuilder
//! - Compose pipelines with operators (>>, |, *, /)
//! - Use S, P, M, T composition modules
//! - Apply pre-built patterns (review_loop, cascade, supervised)
//! - Validate contracts with check_contracts

use adk_rs_fluent::prelude::*;

fn main() {
    println!("=== Research Pipeline Example ===\n");

    // ── Step 1: Define agents with the builder ──

    let researcher = AgentBuilder::new("researcher")
        .instruction("Research the given topic thoroughly. Cite sources.")
        .google_search()
        .url_context()
        .temperature(0.3)
        .writes("findings")
        .writes("sources");

    let writer = AgentBuilder::new("writer")
        .instruction("Write a clear, well-structured article from the findings.")
        .text_only()
        .temperature(0.7)
        .reads("findings")
        .writes("draft");

    let reviewer = AgentBuilder::new("reviewer")
        .instruction("Review the draft. Set quality to 'good' when satisfied.")
        .text_only()
        .thinking(2048)
        .reads("draft")
        .reads("sources")
        .writes("quality")
        .writes("feedback");

    let editor = AgentBuilder::new("editor")
        .instruction("Polish the final draft for publication.")
        .text_only()
        .reads("draft")
        .reads("feedback")
        .writes("final_article");

    println!("Defined 4 agents:");
    println!(
        "  - {} (temp={:?}, tools={})",
        researcher.name(),
        researcher.get_temperature(),
        researcher.tool_count()
    );
    println!(
        "  - {} (temp={:?}, text_only={})",
        writer.name(),
        writer.get_temperature(),
        writer.is_text_only()
    );
    println!(
        "  - {} (thinking={:?})",
        reviewer.name(),
        reviewer.get_thinking_budget()
    );
    println!("  - {} (reads feedback)", editor.name());

    // ── Step 2: Template reuse (copy-on-write) ──

    let fast_researcher = researcher.clone().temperature(0.1);
    let _creative_writer = writer.clone().temperature(0.95);

    assert_eq!(researcher.get_temperature(), Some(0.3)); // Original unchanged
    assert_eq!(fast_researcher.get_temperature(), Some(0.1));
    println!("\nCopy-on-write templates working correctly.");

    // ── Step 3: Compose with operators ──

    // Simple pipeline: research → write
    let _simple = researcher.clone() >> writer.clone();

    // Review loop: write → review → repeat up to 3 times
    let write_review = review_loop(writer.clone(), reviewer.clone(), 3);

    // Full pipeline: research → write/review loop → edit
    let _full_pipeline = researcher.clone() >> write_review >> editor.clone();

    // Parallel research
    let _parallel_research = researcher.clone() | fast_researcher.clone();

    // Fallback: try main researcher, fall back to fast researcher
    let _with_fallback = researcher.clone() / fast_researcher.clone();

    println!("Composed pipelines with >>, |, *, / operators.");

    // ── Step 4: Composition modules ──

    // S — State transforms
    let pre_write = S::pick(&["findings"]) >> S::rename(&[("findings", "research_data")]);
    let mut state = serde_json::json!({"findings": "quantum data", "noise": "ignore"});
    pre_write.apply(&mut state);
    println!("\nState transform: {:?}", state);

    // P — Prompt composition
    let system_prompt = P::role("senior technical writer")
        + P::task("Write a blog post about Rust async patterns")
        + P::constraint("Maximum 1500 words")
        + P::constraint("Include code examples")
        + P::format("Markdown with headers and code blocks")
        + P::example(
            "Topic: Error handling",
            "# Error Handling in Rust\n\nRust's error handling...",
        );
    println!(
        "\nComposed prompt ({} sections):",
        system_prompt.sections.len()
    );
    println!("{}", system_prompt.render());

    // M — Middleware
    let _middleware = M::log() | M::latency();
    println!("\nMiddleware composed: log + latency");

    // T — Tool composition
    let _tools = T::google_search() | T::url_context();
    println!("Tools composed: google_search + url_context");

    // ── Step 5: Pre-built patterns ──

    let _cascade = cascade(vec![researcher.clone(), fast_researcher.clone()]);
    println!("\nCascade: try researcher, then fast_researcher");

    let _fan_out = fan_out_merge(
        vec![researcher.clone(), fast_researcher.clone()],
        editor.clone(),
    );
    println!("Fan-out: run both researchers in parallel");

    let _supervised = supervised(writer.clone(), reviewer.clone(), 5);
    println!("Supervised: writer supervised by reviewer (max 5 revisions)");

    let _map = map_over(writer.clone(), 4);
    println!("Map-over: apply writer to items (concurrency=4)");

    // ── Step 6: Contract validation ──

    println!("\n--- Contract Validation ---");
    let all_agents = [
        researcher.clone(),
        writer.clone(),
        reviewer.clone(),
        editor.clone(),
    ];

    let violations = check_contracts(&all_agents);
    if violations.is_empty() {
        println!("No contract violations found.");
    } else {
        println!("{} violation(s) found:", violations.len());
        for v in &violations {
            match v {
                ContractViolation::UnproducedKey { consumer, key } => {
                    println!("  UNPRODUCED: '{consumer}' reads '{key}' — nobody writes it");
                }
                ContractViolation::DuplicateWrite { agents, key } => {
                    println!("  DUPLICATE: '{key}' written by {:?}", agents);
                }
                ContractViolation::OrphanedOutput { producer, key } => {
                    println!("  ORPHANED: '{producer}' writes '{key}' — nobody reads it");
                }
            }
        }
    }

    println!("\nAll examples completed successfully!");
}
