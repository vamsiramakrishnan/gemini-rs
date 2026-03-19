//! Cookbook #25 — Automated Code Review Pipeline
//!
//! Fan-out pattern: lint, test analysis, and code review run in parallel,
//! then a merge agent combines results with conditional logic.
//!
//! Architecture:
//!   1. Fan-out: linter | test_analyzer | code_reviewer
//!   2. Merge: combine all findings
//!   3. Conditional: approve or request changes based on severity
//!   4. Review loop: iterate if changes requested

use gemini_adk_fluent_rs::prelude::*;
use serde_json::json;

fn main() {
    println!("=== Cookbook #25: Automated Code Review Pipeline ===\n");

    // ── 1. Define review agents ──

    let linter = AgentBuilder::new("linter")
        .instruction(
            "Analyze the code for style violations, naming conventions, \
             and formatting issues. Report each issue with file, line, and severity. \
             Severity levels: error, warning, info.",
        )
        .temperature(0.1)
        .code_execution()
        .writes("lint_results")
        .writes("lint_score");

    let test_analyzer = AgentBuilder::new("test-analyzer")
        .instruction(
            "Analyze test coverage and test quality. Check for: \
             1) Missing test cases for new code paths \
             2) Test assertions that are too weak \
             3) Missing edge case coverage \
             4) Test naming and organization",
        )
        .temperature(0.1)
        .thinking(2048)
        .writes("test_results")
        .writes("coverage_score");

    let code_reviewer = AgentBuilder::new("code-reviewer")
        .instruction(
            "Review the code for: \
             1) Security vulnerabilities (injection, auth bypass, data exposure) \
             2) Performance issues (N+1 queries, memory leaks, blocking calls) \
             3) Architecture concerns (coupling, SOLID violations) \
             4) Error handling completeness \
             Rate overall quality on a 1-10 scale.",
        )
        .temperature(0.2)
        .thinking(4096)
        .writes("review_results")
        .writes("quality_score");

    let security_scanner = AgentBuilder::new("security-scanner")
        .instruction(
            "Perform a focused security audit. Check for: \
             SQL injection, XSS, CSRF, insecure deserialization, \
             hardcoded secrets, and dependency vulnerabilities.",
        )
        .temperature(0.0)
        .writes("security_findings")
        .writes("security_score");

    println!("Review agents defined:");
    for agent in &[&linter, &test_analyzer, &code_reviewer, &security_scanner] {
        let diag = diagnose(agent);
        println!("{}\n", diag);
    }

    // ── 2. Define merge and decision agents ──

    let merge_agent = AgentBuilder::new("merge-reviewer")
        .instruction(
            "Combine findings from lint, test analysis, code review, and security scan. \
             Produce a unified review report with: \
             1) Critical issues (must fix before merge) \
             2) Suggestions (should fix but not blocking) \
             3) Nits (minor style/preference issues) \
             4) Overall verdict: APPROVE, REQUEST_CHANGES, or BLOCK \
             Set approved=true only if there are zero critical issues.",
        )
        .temperature(0.3)
        .reads("lint_results")
        .reads("lint_score")
        .reads("test_results")
        .reads("coverage_score")
        .reads("review_results")
        .reads("quality_score")
        .reads("security_findings")
        .reads("security_score")
        .writes("review_report")
        .writes("verdict")
        .writes("approved");

    let revision_advisor = AgentBuilder::new("revision-advisor")
        .instruction(
            "Based on the review report and verdict, generate specific, actionable \
             revision instructions for the developer. Group by file and priority. \
             Include code snippets where helpful.",
        )
        .reads("review_report")
        .reads("verdict")
        .writes("revision_instructions");

    println!("Decision agents:");
    println!("  - {} (reads all results)", merge_agent.name());
    println!(
        "  - {} (generates fix instructions)",
        revision_advisor.name()
    );

    // ── 3. Compose the fan-out pipeline ──
    println!("\n--- Pipeline Composition ---\n");

    // Fan-out: all reviewers run in parallel
    let review_fanout =
        linter.clone() | test_analyzer.clone() | code_reviewer.clone() | security_scanner.clone();

    println!(
        "Fan-out: {} parallel branches",
        match &review_fanout {
            Composable::FanOut(f) => f.branches.len(),
            _ => 0,
        }
    );

    // Fan-out merge pattern
    let review_with_merge = fan_out_merge(
        vec![
            linter.clone(),
            test_analyzer.clone(),
            code_reviewer.clone(),
            security_scanner.clone(),
        ],
        merge_agent.clone(),
    );

    println!("Fan-out-merge: 4 reviewers >> merge");

    // Supervised review loop: revise until approved
    let review_loop = supervised(revision_advisor.clone(), merge_agent.clone(), 3);

    println!("Supervised loop: revision_advisor supervised by merge_reviewer (max 3)");

    // Full pipeline
    let _full_pipeline = review_with_merge >> review_loop;

    println!("\nFull pipeline: fan_out_merge >> supervised_loop");

    // ── 4. State transforms for score computation ──
    println!("\n--- Score Aggregation ---\n");

    let compute_aggregate = S::compute("aggregate_score", |s| {
        let lint = s.get("lint_score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let coverage = s
            .get("coverage_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let quality = s
            .get("quality_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let security = s
            .get("security_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        // Weighted average: security most important
        let weighted = (lint * 0.15 + coverage * 0.20 + quality * 0.30 + security * 0.35) * 10.0;
        json!(weighted)
    }) >> S::branch(
        |s| {
            s.get("aggregate_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
                >= 7.0
        },
        S::set("auto_verdict", json!("APPROVE")),
        S::set("auto_verdict", json!("REQUEST_CHANGES")),
    );

    let mut state = json!({
        "lint_score": 0.85,
        "coverage_score": 0.72,
        "quality_score": 0.90,
        "security_score": 0.95
    });
    compute_aggregate.apply(&mut state);
    println!("Score aggregation:");
    println!(
        "  lint={}, coverage={}, quality={}, security={}",
        state["lint_score"],
        state["coverage_score"],
        state["quality_score"],
        state["security_score"]
    );
    println!("  aggregate_score: {:.2}", state["aggregate_score"]);
    println!("  auto_verdict: {}", state["auto_verdict"]);

    // Low scores trigger REQUEST_CHANGES
    let mut low_state = json!({
        "lint_score": 0.40,
        "coverage_score": 0.30,
        "quality_score": 0.50,
        "security_score": 0.60
    });
    compute_aggregate.apply(&mut low_state);
    println!("\nLow-score scenario:");
    println!("  aggregate_score: {:.2}", low_state["aggregate_score"]);
    println!("  auto_verdict: {}", low_state["auto_verdict"]);

    // ── 5. Guards for review output ──
    println!("\n--- Output Guards ---\n");

    let review_guards = G::length(50, 10000)
        | G::custom(|output| {
            // Must contain a verdict
            if output.contains("APPROVE")
                || output.contains("REQUEST_CHANGES")
                || output.contains("BLOCK")
            {
                Ok(())
            } else {
                Err("Review must contain a verdict: APPROVE, REQUEST_CHANGES, or BLOCK".into())
            }
        })
        | G::topic(&["personal_attack", "hostile"]);

    println!("Review guards: {} validators", review_guards.len());

    let good_review =
        "After thorough analysis, the code is well-structured. APPROVE with minor nits.";
    let bad_review = "This is terrible code.";
    println!(
        "  Good review: {} violations",
        review_guards.check_all(good_review).len()
    );
    println!(
        "  Bad review: {} violations",
        review_guards.check_all(bad_review).len()
    );

    // ── 6. Contract validation ──
    println!("\n--- Contract Validation ---\n");

    let all_agents = [
        linter.clone(),
        test_analyzer.clone(),
        code_reviewer.clone(),
        security_scanner.clone(),
        merge_agent.clone(),
        revision_advisor.clone(),
    ];

    let violations = check_contracts(&all_agents);
    println!("{} contract violation(s):", violations.len());
    for v in &violations {
        match v {
            ContractViolation::UnproducedKey { consumer, key } => {
                println!("  UNPRODUCED: '{}' reads '{}'", consumer, key);
            }
            ContractViolation::DuplicateWrite { agents, key } => {
                // Expected: multiple reviewers write scores
                println!(
                    "  DUPLICATE: '{}' written by {:?} (expected for parallel reviewers)",
                    key, agents
                );
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

    // ── 7. Evaluation criteria ──
    println!("\n--- Evaluation ---\n");

    let eval = E::custom("verdict_present", |output, _expected| {
        if output.contains("APPROVE")
            || output.contains("REQUEST_CHANGES")
            || output.contains("BLOCK")
        {
            1.0
        } else {
            0.0
        }
    }) | E::custom("actionable", |output, _expected| {
        // Check that review contains specific file references or code suggestions
        let has_specifics = output.contains("line")
            || output.contains("file")
            || output.contains("function")
            || output.contains("```");
        if has_specifics {
            1.0
        } else {
            0.5
        }
    }) | E::safety();

    let test_review = "In file main.rs, line 42: function `process` should handle the error case. \
                        Consider adding a match arm for the Err variant. APPROVE with suggestions.";
    let scores = eval.score_all(test_review, "");
    println!("Review quality scores:");
    for (name, score) in &scores {
        println!("  {}: {:.2}", name, score);
    }

    println!("\nCode review pipeline example completed successfully!");
}
