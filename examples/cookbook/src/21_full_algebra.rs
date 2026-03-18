//! Cookbook #21 — Full Composition Algebra
//!
//! Demonstrates every operator in the composition algebra in a single pipeline:
//! - `>>` Sequential pipeline
//! - `|` Parallel fan-out
//! - `*` Fixed loop
//! - `/` Fallback chain
//! - `* until(pred)` Conditional loop
//!
//! Also shows how these compose with S, C, P, T, A, E, G modules.

use gemini_adk_fluent::prelude::*;
use serde_json::json;

fn main() {
    println!("=== Cookbook #21: Full Composition Algebra ===\n");

    // ── Base agents ──

    let researcher = AgentBuilder::new("researcher")
        .instruction("Research the topic using web search")
        .google_search()
        .temperature(0.3)
        .writes("findings");

    let analyst = AgentBuilder::new("analyst")
        .instruction("Analyze the research findings for key insights")
        .temperature(0.2)
        .reads("findings")
        .writes("analysis");

    let writer = AgentBuilder::new("writer")
        .instruction("Write a clear report from the analysis")
        .temperature(0.7)
        .reads("analysis")
        .writes("draft");

    let reviewer = AgentBuilder::new("reviewer")
        .instruction("Review the draft. Set approved=true when quality is sufficient.")
        .thinking(2048)
        .reads("draft")
        .writes("approved")
        .writes("feedback");

    let editor = AgentBuilder::new("editor")
        .instruction("Polish the final draft based on feedback")
        .reads("draft")
        .reads("feedback")
        .writes("final_report");

    let fallback_writer = AgentBuilder::new("fallback-writer")
        .instruction("Write a simpler report if the main writer fails")
        .temperature(0.5)
        .reads("analysis")
        .writes("draft");

    // ── 1. Sequential pipeline (>>) ──
    // Research flows into analysis
    let research_then_analyze = researcher.clone() >> analyst.clone();
    println!("1. Sequential (>>): researcher >> analyst");
    if let Composable::Pipeline(p) = &research_then_analyze {
        println!("   Pipeline with {} steps", p.steps.len());
    }

    // ── 2. Parallel fan-out (|) ──
    // Run researcher and a fast variant concurrently
    let fast_researcher = researcher.clone().temperature(0.1);
    let parallel_research = researcher.clone() | fast_researcher.clone();
    println!("2. Fan-out (|): researcher | fast_researcher");
    if let Composable::FanOut(f) = &parallel_research {
        println!("   FanOut with {} branches", f.branches.len());
    }

    // ── 3. Fixed loop (*) ──
    // Polish the draft 3 times
    let polished = editor.clone() * 3;
    println!("3. Fixed loop (* 3): editor runs 3 times");
    if let Composable::Loop(l) = &polished {
        println!("   Loop max={}, has_predicate={}", l.max, l.until.is_some());
    }

    // ── 4. Fallback chain (/) ──
    // Try main writer, fall back to simpler writer
    let robust_writer = writer.clone() / fallback_writer.clone();
    println!("4. Fallback (/): writer / fallback_writer");
    if let Composable::Fallback(f) = &robust_writer {
        println!("   Fallback with {} candidates", f.candidates.len());
    }

    // ── 5. Conditional loop (* until) ──
    // Write+review loop until approved
    let review_cycle = (writer.clone() >> reviewer.clone())
        * until(|state| {
            state
                .get("approved")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });
    println!("5. Conditional loop (* until): write >> review, until approved=true");
    if let Composable::Loop(l) = &review_cycle {
        println!("   Loop max={}, has_predicate={}", l.max, l.until.is_some());
    }

    // ── 6. Combine all operators in one mega-pipeline ──
    //
    // Architecture:
    //   parallel research (|)
    //   >> analyst
    //   >> write-review loop (* until)
    //   >> editor polish (* 3)
    //   with fallback (/) on the writer
    //
    let full_pipeline = (researcher.clone() | fast_researcher.clone())  // fan-out
        >> analyst.clone()                                                // sequential
        >> (((writer.clone() / fallback_writer.clone())                   // fallback writer
            >> reviewer.clone())                                         // then review
            * until(|s| s.get("approved").and_then(|v| v.as_bool()).unwrap_or(false)))  // loop
        >> (editor.clone() * 3); // polish loop

    println!("\n6. Full pipeline combining all operators:");
    println!("   (researcher | fast_researcher) >> analyst >> (writer/fallback >> reviewer)*until >> editor*3");
    if let Composable::Pipeline(p) = &full_pipeline {
        println!("   Top-level pipeline with {} steps", p.steps.len());
    }

    // ── 7. S module: State transforms ──
    println!("\n--- S Module: State Transforms ---");

    let transform = S::pick(&["findings", "analysis"])
        >> S::rename(&[("findings", "research_data")])
        >> S::defaults(json!({"confidence": 0.5}))
        >> S::set("pipeline_version", json!("v2"));

    let mut state = json!({
        "findings": "quantum computing breakthroughs",
        "analysis": "significant progress in error correction",
        "noise": "should be removed"
    });
    transform.apply(&mut state);
    println!(
        "   After transform: {}",
        serde_json::to_string_pretty(&state).unwrap()
    );

    // Advanced state transforms
    let advanced = S::merge(&["research_data", "analysis"], "combined")
        >> S::compute("word_count", |s| {
            let text = s
                .get("combined")
                .and_then(|v| v.as_object())
                .map(|o| {
                    o.values()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            json!(text.split_whitespace().count())
        })
        >> S::counter("iteration", 1);

    advanced.apply(&mut state);
    println!(
        "   After advanced: {}",
        serde_json::to_string_pretty(&state).unwrap()
    );

    // ── 8. P module: Prompt composition ──
    println!("\n--- P Module: Prompt Composition ---");

    let prompt = P::role("senior research analyst")
        + P::task("Synthesize multi-source research into an executive briefing")
        + P::constraint("Maximum 500 words")
        + P::constraint("Cite all sources")
        + P::format("Markdown with executive summary, key findings, and recommendations")
        + P::example(
            "Topic: AI Safety",
            "# Executive Briefing: AI Safety\n\n## Summary\nRecent advances...",
        )
        + P::context("This briefing is for C-level executives")
        + P::guidelines(&[
            "Lead with the most impactful finding",
            "Use data points where available",
            "Flag areas of uncertainty",
        ]);

    println!("   Composed {} prompt sections", prompt.sections.len());
    println!("   Rendered prompt:\n{}", prompt.render());

    // ── 9. T module: Tool composition ──
    println!("\n--- T Module: Tool Composition ---");

    let tools = T::google_search()
        | T::code_execution()
        | T::url_context()
        | T::simple("summarize", "Summarize a document", |args| async move {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            Ok(json!({"summary": format!("Summary of {}", url)}))
        })
        | T::mock("calculator", "Perform calculations", json!({"result": 42}));

    println!("   Composed {} tools", tools.len());

    // ── 10. C module: Context engineering ──
    println!("\n--- C Module: Context Engineering ---");
    let context = C::window(10) + C::user_only();
    println!(
        "   Context policy: window(10) + user_only ({} policies)",
        context.policies.len()
    );

    // ── 11. G module: Guards ──
    println!("\n--- G Module: Guard Composition ---");
    let guards =
        G::length(10, 5000) | G::json() | G::pii() | G::topic(&["classified", "restricted"]);
    println!("   Composed {} guards", guards.len());

    // Test guard validation
    let good_output = r#"{"status": "Report generated successfully"}"#;
    let violations = guards.check_all(good_output);
    println!("   Good output violations: {}", violations.len());

    let bad_output = "This classified email@test.com data is restricted";
    let violations = guards.check_all(bad_output);
    println!(
        "   Bad output violations: {} ({:?})",
        violations.len(),
        violations
    );

    // ── 12. E module: Evaluation ──
    println!("\n--- E Module: Evaluation Criteria ---");
    let eval = E::response_match() | E::contains_match() | E::safety();
    let scores = eval.score_all("The answer is 42", "42");
    println!("   Evaluation scores:");
    for (name, score) in &scores {
        println!("     {}: {:.2}", name, score);
    }

    // Build an eval suite
    let suite = E::suite()
        .case("What is 2+2?", "4")
        .case("Capital of France?", "Paris")
        .case(
            "Summarize quantum computing",
            "Quantum computing uses qubits",
        )
        .criteria(&["response_match", "contains_match", "safety"]);
    println!(
        "   Eval suite: {} cases, {} criteria",
        suite.len(),
        suite.criteria_names.len()
    );

    // ── 13. A module: Artifact schemas ──
    println!("\n--- A Module: Artifact Schemas ---");
    let artifacts = A::json_output("report", "Analysis report in JSON")
        + A::text_input("source", "Source document to analyze")
        + A::text_output("summary", "Executive summary");
    println!(
        "   {} artifact transforms, {} inputs, {} outputs",
        artifacts.len(),
        artifacts.all_inputs().len(),
        artifacts.all_outputs().len()
    );

    // ── 14. Pre-built patterns ──
    println!("\n--- Pre-built Patterns ---");

    // Review loop pattern
    let _review = review_loop(writer.clone(), reviewer.clone(), 5);
    println!("   review_loop: writer + reviewer, max 5 rounds");

    // Keyed review loop
    let _keyed = review_loop_keyed(writer.clone(), reviewer.clone(), "quality", "excellent", 3);
    println!("   review_loop_keyed: quality must reach 'excellent'");

    // Cascade pattern (fallback chain)
    let _cascade = cascade(vec![
        researcher.clone(),
        fast_researcher.clone(),
        fallback_writer.clone(),
    ]);
    println!("   cascade: 3 agents, first success wins");

    // Fan-out-merge pattern
    let _merge = fan_out_merge(
        vec![researcher.clone(), fast_researcher.clone()],
        analyst.clone(),
    );
    println!("   fan_out_merge: 2 researchers in parallel, then analyst merges");

    // Supervised pattern
    let _supervised = supervised(writer.clone(), reviewer.clone(), 5);
    println!("   supervised: writer supervised by reviewer, max 5 rounds");

    // Chain pattern
    let _chain = chain(vec![
        researcher.clone(),
        analyst.clone(),
        writer.clone(),
        editor.clone(),
    ]);
    println!("   chain: 4 agents in sequence");

    // Map-over pattern
    let _map = map_over(writer.clone(), 4);
    println!("   map_over: apply writer to items, concurrency=4");

    // Map-reduce pattern
    let _mr = map_reduce(researcher.clone(), analyst.clone(), 8);
    println!("   map_reduce: researcher maps, analyst reduces, concurrency=8");

    // ── 15. Contract validation on the full pipeline ──
    println!("\n--- Contract Validation ---");
    let all_agents = [
        researcher,
        analyst,
        writer,
        reviewer,
        editor,
        fallback_writer,
        fast_researcher,
    ];

    let violations = check_contracts(&all_agents);
    println!("   {} contract violation(s):", violations.len());
    for v in &violations {
        match v {
            ContractViolation::UnproducedKey { consumer, key } => {
                println!(
                    "     UNPRODUCED: '{}' reads '{}' -- nobody writes it",
                    consumer, key
                );
            }
            ContractViolation::DuplicateWrite { agents, key } => {
                println!("     DUPLICATE: '{}' written by {:?}", key, agents);
            }
            ContractViolation::OrphanedOutput { producer, key } => {
                println!(
                    "     ORPHANED: '{}' writes '{}' -- nobody reads it",
                    producer, key
                );
            }
        }
    }

    // Data flow analysis
    let edges = infer_data_flow(&all_agents);
    println!("\n   Data flow edges:");
    for edge in &edges {
        println!(
            "     {} --[{}]--> {}",
            edge.producer, edge.key, edge.consumer
        );
    }

    println!("\nAll algebra examples completed successfully!");
}
