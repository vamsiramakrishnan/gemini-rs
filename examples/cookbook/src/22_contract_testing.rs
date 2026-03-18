//! Cookbook #22 — Contract Testing & Pipeline Validation
//!
//! Demonstrates using check_contracts, AgentHarness, infer_data_flow,
//! and diagnose to validate agent pipelines before deployment.
//!
//! This is a static analysis approach: no LLM calls needed.

use adk_rs_fluent::prelude::*;
use serde_json::json;

fn main() {
    println!("=== Cookbook #22: Contract Testing & Pipeline Validation ===\n");

    // ── 1. Define a multi-agent pipeline with declared reads/writes ──

    let intake = AgentBuilder::new("intake")
        .instruction("Collect customer details: name, account_id, issue_type")
        .writes("customer_name")
        .writes("account_id")
        .writes("issue_type");

    let classifier = AgentBuilder::new("classifier")
        .instruction("Classify the issue into a category and priority")
        .reads("issue_type")
        .writes("category")
        .writes("priority");

    let billing_agent = AgentBuilder::new("billing")
        .instruction("Handle billing inquiries")
        .reads("account_id")
        .reads("category")
        .reads("billing_history")  // This key is unproduced -- intentional bug
        .writes("resolution");

    let tech_agent = AgentBuilder::new("technical")
        .instruction("Handle technical support issues")
        .reads("account_id")
        .reads("category")
        .writes("resolution")       // Duplicate write with billing -- intentional
        .writes("diagnostic_log");

    let closer = AgentBuilder::new("closer")
        .instruction("Summarize the resolution and close the ticket")
        .reads("customer_name")
        .reads("resolution")
        .writes("ticket_summary")
        .writes("satisfaction_score");

    let all_agents = [
        intake.clone(),
        classifier.clone(),
        billing_agent.clone(),
        tech_agent.clone(),
        closer.clone(),
    ];

    // ── 2. Run contract validation ──
    println!("--- Contract Validation ---\n");

    let violations = check_contracts(&all_agents);

    if violations.is_empty() {
        println!("No violations found.");
    } else {
        println!("Found {} violation(s):\n", violations.len());

        let unproduced: Vec<_> = violations.iter().filter(|v| {
            matches!(v, ContractViolation::UnproducedKey { .. })
        }).collect();

        let duplicates: Vec<_> = violations.iter().filter(|v| {
            matches!(v, ContractViolation::DuplicateWrite { .. })
        }).collect();

        let orphans: Vec<_> = violations.iter().filter(|v| {
            matches!(v, ContractViolation::OrphanedOutput { .. })
        }).collect();

        if !unproduced.is_empty() {
            println!("  UNPRODUCED KEYS ({}):", unproduced.len());
            println!("  These keys are read by an agent but never written by any agent.");
            for v in &unproduced {
                if let ContractViolation::UnproducedKey { consumer, key } = v {
                    println!("    - '{}' reads '{}', but no agent writes it", consumer, key);
                }
            }
            println!();
        }

        if !duplicates.is_empty() {
            println!("  DUPLICATE WRITES ({}):", duplicates.len());
            println!("  Multiple agents write the same key, creating a race condition risk.");
            for v in &duplicates {
                if let ContractViolation::DuplicateWrite { agents, key } = v {
                    println!("    - '{}' written by: {:?}", key, agents);
                }
            }
            println!();
        }

        if !orphans.is_empty() {
            println!("  ORPHANED OUTPUTS ({}):", orphans.len());
            println!("  These keys are written by an agent but never read by any agent.");
            for v in &orphans {
                if let ContractViolation::OrphanedOutput { producer, key } = v {
                    println!("    - '{}' writes '{}', but nobody reads it", producer, key);
                }
            }
            println!();
        }
    }

    // ── 3. Infer data flow ──
    println!("--- Data Flow Analysis ---\n");

    let edges = infer_data_flow(&all_agents);
    println!("Data flow graph ({} edges):\n", edges.len());

    // Group by producer
    let mut by_producer: std::collections::HashMap<&str, Vec<&DataFlowEdge>> =
        std::collections::HashMap::new();
    for edge in &edges {
        by_producer.entry(&edge.producer).or_default().push(edge);
    }

    for (producer, edges) in &by_producer {
        println!("  {} -->", producer);
        for edge in edges {
            println!("    --[{}]--> {}", edge.key, edge.consumer);
        }
    }

    // ── 4. Diagnose individual agents ──
    println!("\n--- Agent Diagnostics ---\n");

    for agent in &all_agents {
        let diag = diagnose(agent);
        println!("{}\n", diag);
    }

    // ── 5. AgentHarness for state setup ──
    println!("--- AgentHarness Demo ---\n");

    let harness = AgentHarness::new()
        .set("customer_name", "Alice Johnson")
        .set("account_id", "ACCT-12345")
        .set("issue_type", "billing_dispute")
        .set("category", "billing")
        .set("priority", "high");

    println!("Harness state initialized:");
    let state = harness.state();
    println!("  customer_name: {:?}", state.get::<String>("customer_name"));
    println!("  account_id: {:?}", state.get::<String>("account_id"));
    println!("  issue_type: {:?}", state.get::<String>("issue_type"));
    println!("  category: {:?}", state.get::<String>("category"));
    println!("  priority: {:?}", state.get::<String>("priority"));

    // ── 6. Fix violations and re-validate ──
    println!("\n--- Fixing Violations ---\n");

    // Fix: add a billing_history producer
    let data_loader = AgentBuilder::new("data-loader")
        .instruction("Load billing history from the database")
        .reads("account_id")
        .writes("billing_history");

    // Fix: separate resolution keys to avoid duplicate writes
    let billing_fixed = AgentBuilder::new("billing")
        .instruction("Handle billing inquiries")
        .reads("account_id")
        .reads("category")
        .reads("billing_history")
        .writes("billing_resolution");

    let tech_fixed = AgentBuilder::new("technical")
        .instruction("Handle technical support issues")
        .reads("account_id")
        .reads("category")
        .writes("tech_resolution");

    // Fix: closer reads the specific resolution keys
    let closer_fixed = AgentBuilder::new("closer")
        .instruction("Summarize and close the ticket")
        .reads("customer_name")
        .reads("billing_resolution")
        .reads("tech_resolution")
        .writes("ticket_summary");

    let fixed_agents = [
        intake.clone(),
        classifier.clone(),
        data_loader.clone(),
        billing_fixed.clone(),
        tech_fixed.clone(),
        closer_fixed.clone(),
    ];

    let fixed_violations = check_contracts(&fixed_agents);

    // We still expect some orphans (priority, diagnostic_log removed, etc.)
    let unproduced_fixed: Vec<_> = fixed_violations.iter().filter(|v| {
        matches!(v, ContractViolation::UnproducedKey { .. })
    }).collect();

    let duplicates_fixed: Vec<_> = fixed_violations.iter().filter(|v| {
        matches!(v, ContractViolation::DuplicateWrite { .. })
    }).collect();

    println!("After fixes:");
    println!("  Unproduced keys: {} (was {})",
        unproduced_fixed.len(),
        violations.iter().filter(|v| matches!(v, ContractViolation::UnproducedKey { .. })).count()
    );
    println!("  Duplicate writes: {} (was {})",
        duplicates_fixed.len(),
        violations.iter().filter(|v| matches!(v, ContractViolation::DuplicateWrite { .. })).count()
    );

    // ── 7. State validation with S::validate and S::require ──
    println!("\n--- State Validation with S Module ---\n");

    let validator = S::validate(json!({
        "required": ["customer_name", "account_id", "issue_type"],
        "properties": {
            "customer_name": {"type": "string"},
            "account_id": {"type": "string"},
            "issue_type": {"type": "string"},
            "priority": {"type": "string"}
        }
    }));

    let mut valid_state = json!({
        "customer_name": "Alice",
        "account_id": "ACCT-123",
        "issue_type": "billing",
        "priority": "high"
    });
    validator.apply(&mut valid_state);
    println!("  Valid state passed validation.");

    // State guard
    let guard = S::guard(
        |s| s.get("priority").and_then(|v| v.as_str()).is_some(),
        "Priority must be set before routing",
    );
    guard.apply(&mut valid_state);
    println!("  Guard check passed: priority is set.");

    // S::require
    let require = S::require(&["customer_name", "account_id"]);
    require.apply(&mut valid_state);
    println!("  Required keys present.");

    // ── 8. Build a validated pipeline composition ──
    println!("\n--- Validated Pipeline Composition ---\n");

    // Compose the fixed pipeline using operators
    let pipeline = intake.clone()
        >> classifier.clone()
        >> data_loader.clone()
        >> (billing_fixed.clone() | tech_fixed.clone())
        >> closer_fixed.clone();

    match &pipeline {
        Composable::Pipeline(p) => {
            println!("  Pipeline structure: {} steps", p.steps.len());
            for (i, step) in p.steps.iter().enumerate() {
                match step {
                    Composable::Agent(a) => println!("    Step {}: Agent({})", i, a.name()),
                    Composable::FanOut(f) => println!("    Step {}: FanOut({} branches)", i, f.branches.len()),
                    Composable::Pipeline(p) => println!("    Step {}: Pipeline({} steps)", i, p.steps.len()),
                    Composable::Loop(l) => println!("    Step {}: Loop(max={})", i, l.max),
                    Composable::Fallback(f) => println!("    Step {}: Fallback({} candidates)", i, f.candidates.len()),
                }
            }
        }
        _ => {}
    }

    println!("\nAll contract testing examples completed successfully!");
}
