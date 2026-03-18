//! Walk Example 14: MapOverTextAgent — Applying an Agent to Collections
//!
//! Demonstrates MapOverTextAgent, which iterates a single agent over each item
//! in a state list. This is the agent equivalent of a map() operation: take a
//! list of inputs, process each one with the same agent, and collect results.
//!
//! Features used:
//!   - MapOverTextAgent (iterate agent over list items)
//!   - map_over() pattern helper
//!   - map_reduce() pattern helper
//!   - FnTextAgent (mock processing agents)
//!   - State (list storage and retrieval)
//!   - SequentialTextAgent (pipeline composition)

use std::sync::Arc;

use adk_rs_fluent::prelude::*;

#[tokio::main]
async fn main() {
    println!("=== Walk 14: MapOverTextAgent — Agent Over Collections ===\n");

    // ── Part 1: Basic MapOver ────────────────────────────────────────────
    // Process a list of items, applying the same agent to each one.

    println!("--- Part 1: Basic MapOver ---");

    // Agent that analyzes a single item
    let analyzer: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("sentiment_analyzer", |state| {
        let item = state
            .get::<String>("_item")
            .unwrap_or_else(|| "unknown".into());

        // Simulate sentiment analysis
        let sentiment = if item.contains("great") || item.contains("love") {
            "POSITIVE"
        } else if item.contains("terrible") || item.contains("hate") {
            "NEGATIVE"
        } else {
            "NEUTRAL"
        };

        Ok(format!("{sentiment}: \"{item}\""))
    }));

    // Create a MapOverTextAgent that processes a list of reviews
    let map_agent = MapOverTextAgent::new("review_processor", analyzer.clone(), "reviews")
        .item_key("_item")
        .output_key("analysis_results");

    // Set up state with a list of reviews
    let state = State::new();
    state.set(
        "reviews",
        vec![
            "This product is great!",
            "Terrible customer service",
            "It works as expected",
            "I love the new features",
            "The packaging was okay",
        ],
    );

    let result = map_agent.run(&state).await.unwrap();
    println!("  Processed {} reviews:", 5);
    for line in result.lines() {
        println!("    {line}");
    }

    // Retrieve structured results from state
    let results: Vec<String> = state.get("analysis_results").unwrap_or_default();
    println!("  Results stored in state: {} entries", results.len());

    // ── Part 2: MapOver in a Pipeline ────────────────────────────────────
    // Combine MapOver with other agents in a sequential pipeline.

    println!("\n--- Part 2: MapOver in Pipeline ---");

    // Step 1: Prepare data (simulate splitting a document into sections)
    let splitter: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("document_splitter", |state| {
        let sections = vec![
            "Introduction: Rust is a systems programming language.",
            "Ownership: Every value has a single owner.",
            "Borrowing: References allow temporary access.",
            "Lifetimes: The compiler tracks reference validity.",
        ];
        state.set("sections", &sections);
        Ok(format!("Split document into {} sections", sections.len()))
    }));

    // Step 2: MapOver to summarize each section
    let summarizer: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("summarizer", |state| {
        let section = state
            .get::<String>("_item")
            .unwrap_or_else(|| "empty".into());
        // Extract the topic (before the colon)
        let topic = section.split(':').next().unwrap_or("Unknown");
        Ok(format!("Summary of [{topic}]: Key concept covered."))
    }));

    let map_summarize =
        MapOverTextAgent::new("section_summarizer", summarizer, "sections").output_key("summaries");

    // Step 3: Combine summaries
    let combiner: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("combiner", |state| {
        let summaries: Vec<String> = state.get("summaries").unwrap_or_default();
        let combined = format!(
            "Document Overview ({} sections):\n{}",
            summaries.len(),
            summaries
                .iter()
                .enumerate()
                .map(|(i, s)| format!("  {}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n")
        );
        Ok(combined)
    }));

    // Build the pipeline: split → map-summarize → combine
    let pipeline = SequentialTextAgent::new(
        "document_pipeline",
        vec![splitter, Arc::new(map_summarize), combiner],
    );

    let state = State::new();
    let result = pipeline.run(&state).await.unwrap();
    println!("{result}");

    // ── Part 3: map_over() Pattern Helper ────────────────────────────────

    println!("\n--- Part 3: map_over() Pattern Helper ---");

    let map_workflow = map_over(
        AgentBuilder::new("translator").instruction("Translate the given text to French"),
        4, // concurrency limit
    );

    println!("  Agent: {}", map_workflow.agent.name());
    println!("  Concurrency: {}", map_workflow.concurrency);
    println!("  Instruction: {:?}", map_workflow.agent.get_instruction());

    // ── Part 4: map_reduce() Pattern ─────────────────────────────────────

    println!("\n--- Part 4: map_reduce() Pattern ---");

    let mr_workflow = map_reduce(
        AgentBuilder::new("chunk_analyzer").instruction("Analyze this data chunk for anomalies"),
        AgentBuilder::new("anomaly_aggregator")
            .instruction("Combine anomaly reports into a summary"),
        8, // concurrency limit for map phase
    );

    println!("  Mapper: {}", mr_workflow.mapper.name());
    println!("  Reducer: {}", mr_workflow.reducer.name());
    println!("  Concurrency: {}", mr_workflow.concurrency);

    // ── Part 5: Custom item and output keys ──────────────────────────────

    println!("\n--- Part 5: Custom Keys ---");

    let processor: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("price_calculator", |state| {
        let item = state
            .get::<serde_json::Value>("current_product")
            .unwrap_or(serde_json::json!({}));
        let name = item["name"].as_str().unwrap_or("unknown");
        let base_price = item["price"].as_f64().unwrap_or(0.0);
        let discounted = base_price * 0.9; // 10% discount
        Ok(format!("{name}: ${base_price:.2} -> ${discounted:.2}"))
    }));

    let discount_mapper = MapOverTextAgent::new("discount_mapper", processor, "products")
        .item_key("current_product")
        .output_key("discounted_prices");

    let state = State::new();
    state.set(
        "products",
        vec![
            serde_json::json!({"name": "Widget A", "price": 29.99}),
            serde_json::json!({"name": "Widget B", "price": 49.99}),
            serde_json::json!({"name": "Widget C", "price": 99.99}),
        ],
    );

    let result = discount_mapper.run(&state).await.unwrap();
    println!("  Discount results:");
    for line in result.lines() {
        println!("    {line}");
    }

    println!("\nDone.");
}
