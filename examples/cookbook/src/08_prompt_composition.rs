//! # 08 — Prompt Composition (P:: module)
//!
//! Demonstrates the P:: composition module for building structured prompts
//! from semantic sections. Sections compose additively with `+`.
//!
//! Key concepts:
//! - `P::role()` — define the agent's role ("You are ...")
//! - `P::task()` — describe the task ("Your task: ...")
//! - `P::constraint()` — add behavioral constraints
//! - `P::format()` — specify output format
//! - `P::example()` — add input/output examples
//! - `P::context()` — background context
//! - `P::persona()` — personality description
//! - `P::guidelines()` — bulleted guideline list
//! - `+` operator — compose sections additively
//! - `.render()` — produce the final prompt string

use adk_rs_fluent::prelude::*;

fn main() {
    println!("=== 08: Prompt Composition (P::) ===\n");

    // ── Basic prompt composition ──
    // Each P:: factory creates a PromptSection. Combining with + creates
    // a PromptComposite that renders all sections in order.
    let prompt = P::role("senior technical writer")
        + P::task("Write a blog post about Rust async patterns")
        + P::constraint("Maximum 1500 words")
        + P::constraint("Include code examples")
        + P::format("Markdown with headers and code blocks");

    println!("Sections: {}", prompt.sections.len());
    println!("Rendered prompt:\n{}", prompt.render());

    // ── Adding examples ──
    let with_examples = P::role("code reviewer")
        + P::task("Review the provided Rust code for issues")
        + P::format("Bulleted list of findings")
        + P::example(
            "fn add(a: i32, b: i32) -> i32 { a + b }",
            "- No issues found. Clean implementation.",
        )
        + P::example(
            "fn divide(a: f64, b: f64) -> f64 { a / b }",
            "- Missing check for division by zero.\n- Consider returning Result<f64, Error>.",
        );

    println!("\n--- Code reviewer prompt ---");
    println!("Sections: {}", with_examples.sections.len());
    println!("{}", with_examples.render());

    // ── Context, persona, and guidelines ──
    let support_prompt = P::persona("friendly, patient, and empathetic")
        + P::role("customer support agent for a SaaS product")
        + P::task("Help the customer resolve their issue")
        + P::context("The customer is on the Enterprise plan with priority support")
        + P::guidelines(&[
            "Always greet the customer by name",
            "Acknowledge the issue before providing solutions",
            "Offer to escalate if unresolved after 3 exchanges",
            "Never share internal system details",
        ])
        + P::constraint("Keep responses under 200 words")
        + P::format("Conversational paragraphs");

    println!("\n--- Support agent prompt ---");
    println!("Sections: {}", support_prompt.sections.len());
    println!("{}", support_prompt.render());

    // ── Prompt reuse via Into<String> ──
    // PromptComposite implements Into<String>, so you can pass it directly
    // to AgentBuilder::instruction().
    let instruction: String = (P::role("analyst")
        + P::task("Analyze quarterly revenue data")
        + P::format("JSON with fields: trend, growth_rate, summary"))
    .into();

    let _agent = AgentBuilder::new("revenue-analyst")
        .instruction(&instruction)
        .temperature(0.2);

    println!("\n--- Prompt as agent instruction ---");
    println!("Instruction length: {} chars", instruction.len());

    // ── Filtering and reordering ──
    let full_prompt = P::role("researcher")
        + P::task("Find data on climate change")
        + P::constraint("Use peer-reviewed sources only")
        + P::format("APA citation format")
        + P::context("Focus on data from 2020-2025");

    // Keep only role and task sections by name.
    let minimal = full_prompt.clone().only_by_name(&["role", "task"]);
    println!("\n--- Filtered prompt (only role + task) ---");
    println!("Sections: {}", minimal.sections.len());
    println!("{}", minimal.render());

    // Reorder: put format first.
    let reordered =
        full_prompt.reorder_by_name(&["format", "role", "task", "constraint", "context"]);
    println!("\n--- Reordered prompt (format first) ---");
    for s in &reordered.sections {
        println!("  [{:?}] {}", s.name, s.render());
    }

    println!("\nDone.");
}
