//! Cookbook #23 — Deep Research Pipeline
//!
//! Real-world pattern: parallel multi-source research with iterative
//! refinement and typed structured output.
//!
//! Architecture:
//!   1. Parallel fan-out: web research | academic research | industry analysis
//!   2. Sequential synthesis: merge findings into a cohesive analysis
//!   3. Review loop: iterate until quality threshold is met
//!   4. Final formatting with structured output schema

use gemini_adk_fluent_rs::prelude::*;
use serde_json::json;

fn main() {
    println!("=== Cookbook #23: Deep Research Pipeline ===\n");

    // ── Phase 1: Define specialist research agents ──

    let web_researcher = AgentBuilder::new("web-researcher")
        .instruction(
            "Search the web for recent news, blog posts, and articles on the given topic. \
             Focus on developments from the last 6 months. Cite all sources with URLs.",
        )
        .google_search()
        .url_context()
        .temperature(0.2)
        .writes("web_findings");

    let academic_researcher = AgentBuilder::new("academic-researcher")
        .instruction(
            "Search for academic papers, preprints, and technical reports on the topic. \
             Focus on methodology, experimental results, and citations. \
             Note any consensus or disagreement in the field.",
        )
        .google_search()
        .temperature(0.1)
        .thinking(2048)
        .writes("academic_findings");

    let industry_analyst = AgentBuilder::new("industry-analyst")
        .instruction(
            "Analyze industry trends, market data, and competitive landscape for the topic. \
             Include market size estimates, key players, and growth projections.",
        )
        .google_search()
        .temperature(0.3)
        .writes("industry_findings");

    println!("Phase 1: Defined 3 specialist research agents");
    println!("  - {} (web search + URL context)", web_researcher.name());
    println!("  - {} (thinking enabled)", academic_researcher.name());
    println!("  - {} (market analysis)", industry_analyst.name());

    // ── Phase 2: Synthesis agent ──

    let synthesizer = AgentBuilder::new("synthesizer")
        .instruction(
            "You receive research from three sources: web, academic, and industry. \
             Synthesize these into a unified analysis. Identify: \
             1) Key findings that appear across multiple sources \
             2) Unique insights from each source \
             3) Contradictions or gaps in the research \
             4) Confidence level (high/medium/low) for each finding",
        )
        .temperature(0.4)
        .thinking(4096)
        .reads("web_findings")
        .reads("academic_findings")
        .reads("industry_findings")
        .writes("synthesis");

    println!("\nPhase 2: Synthesizer agent");
    println!(
        "  - {} (reads all 3 sources, thinking=4096)",
        synthesizer.name()
    );

    // ── Phase 3: Quality reviewer with iterative loop ──

    let quality_reviewer = AgentBuilder::new("quality-reviewer")
        .instruction(
            "Review the synthesis for: \
             1) Factual accuracy and source support \
             2) Logical coherence and flow \
             3) Completeness -- are key aspects of the topic covered? \
             4) Balance -- are multiple perspectives represented? \
             Set quality to 'excellent' when all criteria are met. \
             Otherwise, set quality to 'needs_improvement' with specific feedback.",
        )
        .thinking(2048)
        .reads("synthesis")
        .writes("quality")
        .writes("review_feedback");

    let reviser = AgentBuilder::new("reviser")
        .instruction(
            "Revise the synthesis based on the reviewer's feedback. \
             Address each point of feedback explicitly. \
             Maintain the structured format and all citations.",
        )
        .reads("synthesis")
        .reads("review_feedback")
        .writes("synthesis");

    println!("\nPhase 3: Quality loop");
    println!("  - {} (sets quality flag)", quality_reviewer.name());
    println!("  - {} (incorporates feedback)", reviser.name());

    // ── Phase 4: Final formatter with structured output ──

    let report_schema = json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "executive_summary": {"type": "string"},
            "key_findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "finding": {"type": "string"},
                        "confidence": {"type": "string"},
                        "sources": {"type": "array", "items": {"type": "string"}}
                    }
                }
            },
            "market_analysis": {"type": "string"},
            "risks_and_gaps": {"type": "array", "items": {"type": "string"}},
            "recommendations": {"type": "array", "items": {"type": "string"}},
            "methodology_note": {"type": "string"}
        },
        "required": ["title", "executive_summary", "key_findings"]
    });

    let formatter = AgentBuilder::new("formatter")
        .instruction(
            "Format the research synthesis into a structured research report. \
             Follow the output schema exactly. Be concise but thorough.",
        )
        .output_schema(report_schema.clone())
        .reads("synthesis")
        .writes("final_report");

    println!("\nPhase 4: Formatter with structured output schema");
    println!("  - {} (JSON schema output)", formatter.name());

    // ── Compose the full pipeline ──
    println!("\n--- Pipeline Composition ---\n");

    // Step 1: Parallel research fan-out
    let research_fanout =
        web_researcher.clone() | academic_researcher.clone() | industry_analyst.clone();

    println!("Step 1: Parallel research (3 branches)");

    // Step 2: Synthesize
    println!("Step 2: Synthesis");

    // Step 3: Quality review loop (using the pattern function)
    let review = review_loop_keyed(
        reviser.clone(),
        quality_reviewer.clone(),
        "quality",
        "excellent",
        5,
    );
    println!("Step 3: Quality review loop (max 5 rounds, until quality='excellent')");

    // Step 4: Format
    println!("Step 4: Structured formatting");

    // Full pipeline: fanout >> synthesize >> review loop >> format
    let full_pipeline = research_fanout >> synthesizer.clone() >> review >> formatter.clone();

    println!("\nFull pipeline assembled:");
    if let Composable::Pipeline(p) = &full_pipeline {
        println!("  Top-level pipeline: {} steps", p.steps.len());
    }

    // ── Prompt engineering for the synthesizer ──
    println!("\n--- Prompt Engineering ---\n");

    let synth_prompt = P::role("senior research analyst with expertise in technology trends")
        + P::task("Synthesize multi-source research into a cohesive analysis")
        + P::constraint("Cite every claim with its source")
        + P::constraint("Quantify findings where possible")
        + P::constraint("Flag low-confidence findings explicitly")
        + P::format("Structured sections: Overview, Findings, Analysis, Gaps")
        + P::guidelines(&[
            "Cross-reference findings across sources",
            "Prioritize recent data over older sources",
            "Note methodology differences between studies",
            "Include confidence intervals where available",
        ])
        + P::context("This research will inform a board-level strategic decision");

    println!(
        "Synthesizer prompt ({} sections):",
        synth_prompt.sections.len()
    );
    println!("{}", synth_prompt.render());

    // ── State transforms for data flow ──
    println!("\n--- State Transforms ---\n");

    // Pre-synthesis transform: normalize field names from fan-out
    let pre_synth = S::defaults(json!({
        "web_findings": "No web findings available",
        "academic_findings": "No academic findings available",
        "industry_findings": "No industry findings available"
    }));

    let mut state = json!({
        "web_findings": "Recent articles show growing adoption of LLMs...",
        "academic_findings": "Papers demonstrate 15% improvement in accuracy..."
    });
    pre_synth.apply(&mut state);
    println!("Pre-synthesis defaults applied:");
    println!("  industry_findings: {:?}", state.get("industry_findings"));

    // Post-review transform: compute quality metrics
    let post_review = S::compute("review_count", |s| {
        let feedback = s
            .get("review_feedback")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        json!(feedback.lines().count())
    }) >> S::history("quality", 5);

    state["quality"] = json!("needs_improvement");
    state["review_feedback"] = json!("Missing market data\nNeeds more citations");
    post_review.apply(&mut state);
    println!("\nPost-review metrics:");
    println!("  review_count: {:?}", state.get("review_count"));
    println!("  quality_history: {:?}", state.get("quality_history"));

    // ── Contract validation ──
    println!("\n--- Contract Validation ---\n");

    let all_agents = [
        web_researcher,
        academic_researcher,
        industry_analyst,
        synthesizer,
        quality_reviewer,
        reviser,
        formatter,
    ];

    let violations = check_contracts(&all_agents);
    println!("{} violation(s):", violations.len());
    for v in &violations {
        match v {
            ContractViolation::UnproducedKey { consumer, key } => {
                println!("  UNPRODUCED: '{}' reads '{}'", consumer, key);
            }
            ContractViolation::DuplicateWrite { agents, key } => {
                println!("  DUPLICATE: '{}' written by {:?}", key, agents);
            }
            ContractViolation::OrphanedOutput { producer, key } => {
                println!("  ORPHANED: '{}' writes '{}'", producer, key);
            }
        }
    }

    // Data flow
    let edges = infer_data_flow(&all_agents);
    println!("\nData flow ({} edges):", edges.len());
    for edge in &edges {
        println!("  {} --[{}]--> {}", edge.producer, edge.key, edge.consumer);
    }

    // ── Artifact declarations ──
    println!("\n--- Artifact Declarations ---\n");

    let artifacts = A::text_input("topic", "Research topic")
        + A::json_output("report", "Structured research report")
        + A::text_output("executive_summary", "One-page executive summary");

    println!(
        "Artifacts: {} inputs, {} outputs",
        artifacts.all_inputs().len(),
        artifacts.all_outputs().len()
    );

    // ── Evaluation suite ──
    println!("\n--- Evaluation Suite ---\n");

    let eval = E::suite()
        .case(
            "Research: quantum computing 2024",
            "Quantum computing research has seen significant advances in error correction",
        )
        .case(
            "Research: autonomous vehicles market",
            "The autonomous vehicle market is projected to reach $2T by 2030",
        )
        .criteria(&["contains_match", "safety", "semantic_match"]);

    println!(
        "Built eval suite: {} cases, {} criteria",
        eval.len(),
        eval.criteria_names.len()
    );

    println!("\nDeep research pipeline example completed successfully!");
}
