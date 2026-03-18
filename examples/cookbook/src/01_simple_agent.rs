//! # 01 — Simple Agent
//!
//! The most basic example: create an agent with a name, model, instruction,
//! and sampling parameters using `AgentBuilder`.
//!
//! Demonstrates:
//! - `AgentBuilder::new()` — create a named agent
//! - `.model()` — set the Gemini model
//! - `.instruction()` — set the system instruction
//! - `.temperature()` — control randomness
//! - `.thinking()` — enable thinking budget
//! - Copy-on-write semantics (cloning a builder leaves the original unchanged)

use adk_rs_fluent::prelude::*;

fn main() {
    println!("=== 01: Simple Agent ===\n");

    // ── Basic agent construction ──
    // AgentBuilder uses copy-on-write: each setter returns a new builder.
    let agent = AgentBuilder::new("analyst")
        .model(GeminiModel::Gemini2_0FlashLive)
        .instruction("Analyze the given topic and provide key insights.")
        .temperature(0.3);

    println!("Agent name:        {}", agent.name());
    println!("Model:             {:?}", agent.get_model());
    println!("Temperature:       {:?}", agent.get_temperature());
    println!("Instruction:       {:?}", agent.get_instruction());

    // ── Copy-on-write templates ──
    // Clone the builder and modify it — the original stays unchanged.
    let creative = agent.clone().temperature(0.95);
    let precise = agent.clone().temperature(0.1);

    assert_eq!(agent.get_temperature(), Some(0.3)); // original unchanged
    assert_eq!(creative.get_temperature(), Some(0.95));
    assert_eq!(precise.get_temperature(), Some(0.1));
    println!("\nCopy-on-write verified: original temp={:?}, creative={:?}, precise={:?}",
        agent.get_temperature(),
        creative.get_temperature(),
        precise.get_temperature(),
    );

    // ── Additional sampling parameters ──
    let detailed = AgentBuilder::new("detailed-analyst")
        .model(GeminiModel::Gemini2_0FlashLive)
        .instruction("Provide thorough analysis with citations.")
        .temperature(0.5)
        .top_p(0.9)
        .top_k(40)
        .max_output_tokens(4096)
        .thinking(2048);

    println!("\nDetailed agent:");
    println!("  top_p:             {:?}", detailed.get_top_p());
    println!("  top_k:             {:?}", detailed.get_top_k());
    println!("  max_output_tokens: {:?}", detailed.get_max_output_tokens());
    println!("  thinking_budget:   {:?}", detailed.get_thinking_budget());

    // ── Text-only mode ──
    let text_agent = AgentBuilder::new("writer")
        .instruction("Write clear prose.")
        .text_only();

    println!("\nWriter is text_only: {}", text_agent.is_text_only());

    // ── Data contracts: reads/writes ──
    // Declare which state keys an agent reads and writes.
    // Used by `check_contracts()` to find wiring bugs at build time.
    let researcher = AgentBuilder::new("researcher")
        .instruction("Research the topic.")
        .writes("findings")
        .writes("sources");

    let writer = AgentBuilder::new("writer")
        .instruction("Write an article from findings.")
        .reads("findings")
        .writes("draft");

    println!("\nResearcher writes: {:?}", researcher.get_writes());
    println!("Writer reads:      {:?}", writer.get_reads());
    println!("Writer writes:     {:?}", writer.get_writes());

    println!("\nDone.");
}
