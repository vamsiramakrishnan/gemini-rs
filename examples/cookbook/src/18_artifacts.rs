//! Walk Example 18: Artifacts — A::json_output, A::text_input, A::publish, A::load
//!
//! Demonstrates the A namespace for declaring and managing artifacts. Artifacts
//! are typed, named data objects that agents produce and consume. The artifact
//! system provides schema declarations (compile-time contracts) and runtime
//! operations (publish, save, load, delete).
//!
//! Features used:
//!   - A::json_output / A::json_input (JSON artifact declarations)
//!   - A::text_output / A::text_input (text artifact declarations)
//!   - A::output / A::input (custom MIME type declarations)
//!   - A::publish (runtime publish operation)
//!   - A::save / A::load (persistence operations)
//!   - A::list / A::delete (management operations)
//!   - A::version (versioned access)
//!   - A::from_json / A::from_text (creation from data)
//!   - A::as_json / A::as_text (format conversion)
//!   - A::when (conditional operations)
//!   - `+` operator for artifact composition

use adk_rs_fluent::prelude::*;

fn main() {
    println!("=== Walk 18: Artifacts ===\n");

    // ── Part 1: Artifact Schema Declarations ─────────────────────────────
    // Schemas declare what artifacts an agent produces or consumes.
    // This is metadata — used for documentation, validation, and tooling.

    println!("--- Part 1: Schema Declarations ---");

    // JSON output artifact
    let report_schema = A::json_output("analysis_report", "Detailed analysis report in JSON format");
    println!("  JSON output:");
    for output in report_schema.all_outputs() {
        println!(
            "    name={}, mime={}, desc={}",
            output.name, output.mime_type, output.description
        );
    }

    // Text input artifact
    let source_schema = A::text_input("source_document", "The document to analyze");
    println!("  Text input:");
    for input in source_schema.all_inputs() {
        println!(
            "    name={}, mime={}, desc={}",
            input.name, input.mime_type, input.description
        );
    }

    // Custom MIME type
    let csv_schema = A::output("data_export", "text/csv", "Exported data in CSV format");
    for output in csv_schema.all_outputs() {
        println!("  Custom output: name={}, mime={}", output.name, output.mime_type);
    }

    // ── Part 2: Composing Artifact Schemas with `+` ──────────────────────

    println!("\n--- Part 2: Composing Schemas ---");

    let agent_artifacts = A::text_input("raw_text", "Input text to process")
        + A::json_output("entities", "Extracted named entities")
        + A::json_output("sentiment", "Sentiment analysis result")
        + A::text_output("summary", "Human-readable summary");

    println!(
        "  Agent artifact spec: {} transforms",
        agent_artifacts.len()
    );
    println!("  Inputs: {}", agent_artifacts.all_inputs().len());
    println!("  Outputs: {}", agent_artifacts.all_outputs().len());

    for input in agent_artifacts.all_inputs() {
        println!("    IN:  {} ({})", input.name, input.mime_type);
    }
    for output in agent_artifacts.all_outputs() {
        println!("    OUT: {} ({})", output.name, output.mime_type);
    }

    // ── Part 3: Pipeline Artifact Contracts ──────────────────────────────
    // Define what each stage of a pipeline produces and consumes.

    println!("\n--- Part 3: Pipeline Contracts ---");

    let extractor_artifacts =
        A::text_input("document", "Raw document") + A::json_output("extracted_data", "Structured data");

    let transformer_artifacts = A::json_input("extracted_data", "Data from extractor")
        + A::json_output("transformed_data", "Cleaned and normalized data");

    let loader_artifacts =
        A::json_input("transformed_data", "Data to load") + A::text_output("load_report", "Load status");

    // Combine all pipeline artifacts
    let pipeline_artifacts = extractor_artifacts + transformer_artifacts + loader_artifacts;

    println!(
        "  ETL pipeline: {} artifact transforms",
        pipeline_artifacts.len()
    );
    println!(
        "  Total inputs:  {}",
        pipeline_artifacts.all_inputs().len()
    );
    println!(
        "  Total outputs: {}",
        pipeline_artifacts.all_outputs().len()
    );

    // ── Part 4: Runtime Artifact Operations ──────────────────────────────

    println!("\n--- Part 4: Runtime Operations ---");

    // Publish an artifact
    let publish_op = A::publish("report", "application/json");
    println!("  Publish: name={:?}", publish_op.name());
    println!("  Should execute: {}", publish_op.should_execute());

    // Save and load
    let save_op = A::save("report");
    let load_op = A::load("report");
    println!("  Save: name={:?}", save_op.name());
    println!("  Load: name={:?}", load_op.name());

    // List and delete
    let list_op = A::list();
    let delete_op = A::delete("old_report");
    println!("  List: name={:?} (None = all)", list_op.name());
    println!("  Delete: name={:?}", delete_op.name());

    // Versioned access
    let version_op = A::version("report", 3);
    println!("  Version: name={:?}", version_op.name());

    // ── Part 5: Composing Operations with `+` ────────────────────────────

    println!("\n--- Part 5: Operation Pipelines ---");

    // Build an artifact processing pipeline
    let artifact_pipeline = A::load("raw_data")
        + A::as_json("raw_data")
        + A::publish("processed_data", "application/json")
        + A::save("processed_data");

    let ops = artifact_pipeline.flatten();
    println!("  Pipeline: {} operations", ops.len());
    for (i, op) in ops.iter().enumerate() {
        println!("    {}: {:?}", i + 1, op);
    }

    // ── Part 6: Creating Artifacts from Data ─────────────────────────────

    println!("\n--- Part 6: Creating Artifacts ---");

    let json_op = A::from_json("config", r#"{"model": "gemini-2.0-flash", "temperature": 0.7}"#);
    let text_op = A::from_text("readme", "# My Agent\n\nThis agent does amazing things.");

    println!("  From JSON: name={:?}", json_op.name());
    println!("  From text: name={:?}", text_op.name());

    // Convert formats
    let to_json = A::as_json("data");
    let to_text = A::as_text("data");
    println!("  As JSON: name={:?}", to_json.name());
    println!("  As text: name={:?}", to_text.name());

    // ── Part 7: Conditional Operations ───────────────────────────────────

    println!("\n--- Part 7: Conditional Operations ---");

    // Save only if a condition is met
    let conditional_save = A::when(|| true, A::save("important_data"));
    println!(
        "  Conditional save (true):  should_execute={}",
        conditional_save.should_execute()
    );

    let conditional_skip = A::when(|| false, A::save("unimportant_data"));
    println!(
        "  Conditional save (false): should_execute={}",
        conditional_skip.should_execute()
    );

    // ── Part 8: Full Agent Artifact Specification ────────────────────────

    println!("\n--- Part 8: Full Agent Spec ---");

    // A complete artifact specification for a research agent
    let research_agent_spec = A::text_input("query", "Research question")
        + A::text_input("context", "Background context")
        + A::json_output("findings", "Research findings with citations")
        + A::json_output("confidence", "Confidence scores per finding")
        + A::text_output("summary", "Executive summary")
        + A::output("bibliography", "text/x-bibtex", "Bibliography in BibTeX format");

    println!("  Research agent artifact specification:");
    println!("    Inputs:");
    for input in research_agent_spec.all_inputs() {
        println!("      - {} ({}): {}", input.name, input.mime_type, input.description);
    }
    println!("    Outputs:");
    for output in research_agent_spec.all_outputs() {
        println!("      - {} ({}): {}", output.name, output.mime_type, output.description);
    }

    // ── Part 9: Operation Pipeline for Batch Processing ──────────────────

    println!("\n--- Part 9: Batch Processing Pipeline ---");

    let batch_pipeline = A::load("batch_input")
        + A::as_json("batch_input")
        + A::from_json("results", "[]")
        + A::publish("results", "application/json")
        + A::save("results")
        + A::delete("batch_input");

    let ops = batch_pipeline.flatten();
    println!("  Batch pipeline: {} operations", ops.len());
    for op in &ops {
        match op.name() {
            Some(name) => println!("    {:?} -> {name}", op),
            None => println!("    {:?}", op),
        }
    }

    println!("\nDone.");
}
