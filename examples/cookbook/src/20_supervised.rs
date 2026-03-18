//! Walk Example 20: Supervised — Worker + Supervisor Pattern
//!
//! Demonstrates the supervised pattern where a worker agent performs tasks under
//! the oversight of a supervisor agent. The supervisor reviews the worker's output
//! and either approves it or sends it back for revision, up to a maximum number
//! of rounds.
//!
//! Features used:
//!   - supervised() pattern (worker + supervisor loop)
//!   - supervised_keyed() (custom approval key)
//!   - FnTextAgent (mock worker/supervisor agents)
//!   - LoopTextAgent (underlying loop mechanics)
//!   - SequentialTextAgent (worker >> supervisor pipeline)
//!   - State (inter-agent communication)
//!   - S::is_true (state predicate for transitions)
//!   - fan_out_merge() (parallel workers + merger)

use std::sync::Arc;

use adk_rs_fluent::prelude::*;

#[tokio::main]
async fn main() {
    println!("=== Walk 20: Supervised — Worker + Supervisor ===\n");

    // ── Part 1: Manual Supervised Pattern ─────────────────────────────────
    // Build the worker-supervisor loop from scratch.

    println!("--- Part 1: Manual Supervised Loop ---");

    // Worker: writes code, incorporating supervisor feedback
    let coder: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("coder", |state| {
        let iteration = state.modify("revision", 0u32, |n| n + 1);
        let feedback = state
            .get::<String>("supervisor_feedback")
            .unwrap_or_else(|| "initial submission".into());

        let code = format!(
            "// Revision {iteration}\n\
             fn sort(data: &mut [i32]) {{\n\
             {}    data.sort();\n\
             }}",
            if iteration >= 3 {
                "    // Added bounds check per review\n    if data.is_empty() {{ return; }}\n"
            } else if iteration >= 2 {
                "    // Added documentation per review\n"
            } else {
                ""
            }
        );

        state.set("submitted_code", &code);
        println!("  [Coder] Revision {iteration} (feedback: {feedback})");
        println!("  [Coder] Submitted:\n{code}");
        Ok(code)
    }));

    // Supervisor: reviews code and either approves or requests changes
    let lead: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("tech_lead", |state| {
        let revision: u32 = state.get("revision").unwrap_or(0);
        let _code = state.get::<String>("submitted_code").unwrap_or_default();

        // Simulate progressive quality improvement
        if revision >= 3 {
            state.set("approved", true);
            let review = "APPROVED: Code meets all quality standards. Ready to merge.".to_string();
            println!("  [Lead] {review}");
            Ok(review)
        } else if revision >= 2 {
            state.set("approved", false);
            let feedback = "Add bounds checking for empty arrays".to_string();
            state.set("supervisor_feedback", &feedback);
            println!("  [Lead] REVISION NEEDED: {feedback}");
            Ok(format!("REVISION NEEDED: {feedback}"))
        } else {
            state.set("approved", false);
            let feedback = "Add documentation comments".to_string();
            state.set("supervisor_feedback", &feedback);
            println!("  [Lead] REVISION NEEDED: {feedback}");
            Ok(format!("REVISION NEEDED: {feedback}"))
        }
    }));

    // Build the supervised loop manually
    let work_review = SequentialTextAgent::new("work_review", vec![coder, lead]);
    let supervised_loop = LoopTextAgent::new("supervised", Arc::new(work_review), 5)
        .until(|state| state.get::<bool>("approved").unwrap_or(false));

    let state = State::new();
    let result = supervised_loop.run(&state).await.unwrap();

    println!("\n  Final result: {result}");
    println!(
        "  Total revisions: {}",
        state.get::<u32>("revision").unwrap_or(0)
    );
    println!(
        "  Approved: {}",
        state.get::<bool>("approved").unwrap_or(false)
    );

    // ── Part 2: Using supervised() Pattern Helper ────────────────────────

    println!("\n--- Part 2: supervised() Pattern ---");

    let managed = supervised(
        AgentBuilder::new("developer")
            .instruction("Implement the feature according to the spec")
            .writes("code")
            .writes("tests"),
        AgentBuilder::new("reviewer")
            .instruction(
                "Code review. Check for correctness, style, and test coverage. \
                 Set approved=true when ready to merge.",
            )
            .reads("code")
            .reads("tests")
            .writes("approved"),
        5, // max 5 revision rounds
    );

    // Inspect the structure
    match &managed {
        Composable::Loop(l) => {
            println!("  Max rounds: {}", l.max);
            println!("  Has approval predicate: {}", l.until.is_some());
            if let Composable::Pipeline(p) = &*l.body {
                println!("  Pipeline steps:");
                for step in &p.steps {
                    if let Composable::Agent(a) = step {
                        println!("    - {} (writes: {:?})", a.name(), a.get_writes());
                    }
                }
            }
        }
        _ => unreachable!(),
    }

    // Test the predicate
    if let Composable::Loop(l) = &managed {
        if let Some(pred) = &l.until {
            println!(
                "  approved=false -> continue: {}",
                !pred.check(&serde_json::json!({"approved": false}))
            );
            println!(
                "  approved=true  -> stop:     {}",
                pred.check(&serde_json::json!({"approved": true}))
            );
        }
    }

    // ── Part 3: supervised_keyed() with Custom Key ───────────────────────

    println!("\n--- Part 3: supervised_keyed() ---");

    let qa_loop = supervised_keyed(
        AgentBuilder::new("test_writer").instruction("Write unit tests for the sorting module"),
        AgentBuilder::new("qa_manager")
            .instruction("Review test coverage. Set qa_approved=true when coverage is adequate."),
        "qa_approved", // custom approval key
        4,             // max revisions
    );

    if let Composable::Loop(l) = &qa_loop {
        println!("  Custom key: 'qa_approved'");
        println!("  Max revisions: {}", l.max);
        if let Some(pred) = &l.until {
            println!(
                "  qa_approved=false: {}",
                pred.check(&serde_json::json!({"qa_approved": false}))
            );
            println!(
                "  qa_approved=true:  {}",
                pred.check(&serde_json::json!({"qa_approved": true}))
            );
        }
    }

    // ── Part 4: Supervised Team (Fan-Out + Supervisor) ───────────────────
    // Multiple workers in parallel, then a supervisor reviews all outputs.

    println!("\n--- Part 4: Supervised Team ---");

    // Multiple workers produce artifacts in parallel, supervisor merges
    let team_pipeline = fan_out_merge(
        vec![
            AgentBuilder::new("frontend_dev")
                .instruction("Implement the UI component")
                .writes("frontend_code"),
            AgentBuilder::new("backend_dev")
                .instruction("Implement the API endpoint")
                .writes("backend_code"),
            AgentBuilder::new("test_engineer")
                .instruction("Write integration tests")
                .writes("test_code"),
        ],
        AgentBuilder::new("tech_lead")
            .instruction(
                "Review all submissions. Ensure frontend and backend are compatible. \
                 Set approved=true when the feature is complete.",
            )
            .reads("frontend_code")
            .reads("backend_code")
            .reads("test_code"),
    );

    match &team_pipeline {
        Composable::Pipeline(p) => {
            println!("  Team pipeline: {} steps", p.steps.len());
            if let Composable::FanOut(f) = &p.steps[0] {
                println!(
                    "  Step 1: FanOut with {} parallel workers",
                    f.branches.len()
                );
            }
            if let Composable::Agent(a) = &p.steps[1] {
                println!("  Step 2: Supervisor '{}'", a.name());
            }
        }
        _ => unreachable!(),
    }

    // ── Part 5: Hierarchical Supervision ─────────────────────────────────
    // Nested supervised loops: team lead supervises coders,
    // engineering manager supervises team leads.

    println!("\n--- Part 5: Hierarchical Supervision ---");

    let inner_loop = supervised(
        AgentBuilder::new("junior_dev").instruction("Implement the assigned task"),
        AgentBuilder::new("senior_dev")
            .instruction("Review junior dev's work. Set approved=true when acceptable."),
        3,
    );

    let outer_loop = supervised(
        AgentBuilder::new("team").instruction("Complete the sprint tasks"),
        AgentBuilder::new("engineering_manager").instruction(
            "Review sprint deliverables. Set approved=true when sprint goals are met.",
        ),
        2,
    );

    fn describe_composable(c: &Composable, depth: usize) {
        let indent = "  ".repeat(depth);
        match c {
            Composable::Agent(a) => println!("{indent}Agent({})", a.name()),
            Composable::Pipeline(p) => {
                println!("{indent}Pipeline({} steps):", p.steps.len());
                for step in &p.steps {
                    describe_composable(step, depth + 1);
                }
            }
            Composable::Loop(l) => {
                println!("{indent}Loop(max={}):", l.max);
                describe_composable(&l.body, depth + 1);
            }
            Composable::FanOut(f) => {
                println!("{indent}FanOut({} branches):", f.branches.len());
                for branch in &f.branches {
                    describe_composable(branch, depth + 1);
                }
            }
            Composable::Fallback(f) => {
                println!("{indent}Fallback({} candidates)", f.candidates.len());
            }
        }
    }

    println!("  Inner loop (dev team):");
    describe_composable(&inner_loop, 2);

    println!("  Outer loop (management):");
    describe_composable(&outer_loop, 2);

    // ── Part 6: State Predicate Helpers ──────────────────────────────────

    println!("\n--- Part 6: State Predicates for Supervision ---");

    let supervision_state = State::new();
    supervision_state.set("approved", false);
    supervision_state.set("quality_score", "high");
    supervision_state.set("status", "review");

    let is_approved = S::is_true("approved");
    let is_high_quality = S::eq("quality_score", "high");
    let is_actionable = S::one_of("status", &["review", "revision", "approved"]);

    println!("  is_approved:      {}", is_approved(&supervision_state));
    println!(
        "  is_high_quality:  {}",
        is_high_quality(&supervision_state)
    );
    println!("  is_actionable:    {}", is_actionable(&supervision_state));

    // Update state and re-check
    supervision_state.set("approved", true);
    println!(
        "  After approval -> is_approved: {}",
        is_approved(&supervision_state)
    );

    println!("\nDone.");
}
