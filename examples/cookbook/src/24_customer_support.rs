//! Cookbook #24 — Customer Support Pipeline
//!
//! Production pattern: triage routing to specialist agents with escalation.
//!
//! Architecture:
//!   1. Intake agent: collect customer info and classify issue
//!   2. Router (RouteTextAgent): dispatch to the right specialist
//!   3. Specialist agents: billing, technical, account
//!   4. Escalation fallback when confidence is low
//!   5. Closer agent: summarize resolution and generate follow-up

use adk_rs_fluent::prelude::*;
use serde_json::json;

fn main() {
    println!("=== Cookbook #24: Customer Support Pipeline ===\n");

    // ── 1. Define specialist agents ──

    let intake = AgentBuilder::new("intake")
        .instruction(
            "You are the intake agent. Collect the customer's name, account ID, \
             and issue description. Classify the issue as: billing, technical, \
             account, or general. Set the category and urgency (low/medium/high).",
        )
        .temperature(0.2)
        .writes("customer_name")
        .writes("account_id")
        .writes("issue_category")
        .writes("urgency");

    let billing_specialist = AgentBuilder::new("billing-specialist")
        .instruction(
            "You are a billing specialist. Handle billing disputes, payment issues, \
             refund requests, and subscription changes. Always verify the account \
             before making changes. Be empathetic but follow policy.",
        )
        .temperature(0.3)
        .reads("account_id")
        .reads("issue_category")
        .writes("resolution")
        .writes("action_taken");

    let tech_specialist = AgentBuilder::new("tech-specialist")
        .instruction(
            "You are a technical support specialist. Diagnose technical issues, \
             provide step-by-step troubleshooting, and escalate hardware failures. \
             Ask for error codes and system information.",
        )
        .temperature(0.2)
        .thinking(2048)
        .reads("account_id")
        .reads("issue_category")
        .writes("resolution")
        .writes("diagnostic_log");

    let account_specialist = AgentBuilder::new("account-specialist")
        .instruction(
            "You are an account specialist. Handle account access issues, \
             profile updates, security concerns, and account closures. \
             Verify identity before making any account changes.",
        )
        .temperature(0.2)
        .reads("account_id")
        .reads("issue_category")
        .writes("resolution")
        .writes("identity_verified");

    let general_agent = AgentBuilder::new("general-agent")
        .instruction(
            "You are a general support agent. Handle inquiries that don't fit \
             specific categories. Provide helpful information and escalate \
             if the issue requires specialist attention.",
        )
        .temperature(0.5)
        .reads("issue_category")
        .writes("resolution");

    let escalation_agent = AgentBuilder::new("escalation")
        .instruction(
            "You are the escalation handler. This issue requires supervisor review. \
             Summarize the situation, document what has been tried, and create \
             an escalation ticket with all relevant context.",
        )
        .temperature(0.2)
        .reads("customer_name")
        .reads("account_id")
        .reads("issue_category")
        .reads("urgency")
        .writes("escalation_ticket")
        .writes("resolution");

    let closer = AgentBuilder::new("closer")
        .instruction(
            "Summarize the resolution for the customer. Generate a follow-up plan \
             if needed. Ask for a satisfaction rating.",
        )
        .temperature(0.5)
        .reads("customer_name")
        .reads("resolution")
        .writes("ticket_summary")
        .writes("follow_up");

    println!("Defined {} agents:", 7);
    for agent in &[
        &intake,
        &billing_specialist,
        &tech_specialist,
        &account_specialist,
        &general_agent,
        &escalation_agent,
        &closer,
    ] {
        println!(
            "  - {} (temp={:?}, reads={:?}, writes={:?})",
            agent.name(),
            agent.get_temperature(),
            agent.get_reads(),
            agent.get_writes()
        );
    }

    // ── 2. Compose the routing pipeline ──
    println!("\n--- Pipeline Composition ---\n");

    // Prompt engineering for intake
    let intake_prompt = P::role("customer support intake specialist")
        + P::task("Classify the customer's issue and collect required information")
        + P::constraint("Always ask for account ID if not provided")
        + P::constraint("Classify urgency based on impact and customer tone")
        + P::format("Set state keys: customer_name, account_id, issue_category, urgency")
        + P::guidelines(&[
            "Be warm and professional",
            "Acknowledge the customer's frustration",
            "Collect minimum required info before routing",
        ]);

    println!("Intake prompt ({} sections):", intake_prompt.sections.len());
    println!("{}\n", intake_prompt.render());

    // Route based on category
    // In production, this would use RouteTextAgent with an LLM.
    // Here we show the pipeline structure declaratively.
    let billing_pipeline = billing_specialist.clone() / escalation_agent.clone(); // Fall back to escalation

    let tech_pipeline = tech_specialist.clone() / escalation_agent.clone();

    let account_pipeline = account_specialist.clone() / escalation_agent.clone();

    println!("Specialist pipelines with escalation fallback:");
    println!("  billing  -> billing_specialist / escalation");
    println!("  tech     -> tech_specialist / escalation");
    println!("  account  -> account_specialist / escalation");
    println!("  general  -> general_agent / escalation");

    // Full support pipeline
    let support_pipeline = intake.clone()
        >> (billing_pipeline
            | tech_pipeline
            | account_pipeline
            | (general_agent.clone() / escalation_agent.clone()))
        >> closer.clone();

    println!("\nFull pipeline: intake >> specialist_fanout >> closer");
    if let Composable::Pipeline(p) = &support_pipeline {
        println!("  {} top-level steps", p.steps.len());
    }

    // ── 3. State transforms for routing ──
    println!("\n--- State Transforms ---\n");

    // Pre-routing validation
    let pre_route = S::require(&["issue_category", "account_id"])
        >> S::defaults(json!({
            "urgency": "medium",
            "escalation_count": 0
        }))
        >> S::validate(json!({
            "required": ["issue_category", "account_id"],
            "properties": {
                "issue_category": {"type": "string"},
                "account_id": {"type": "string"},
                "urgency": {"type": "string"}
            }
        }));

    let mut state = json!({
        "customer_name": "Bob Smith",
        "account_id": "ACCT-98765",
        "issue_category": "billing",
    });
    pre_route.apply(&mut state);
    println!("Pre-route validation passed:");
    println!("  urgency defaulted to: {:?}", state.get("urgency"));

    // Post-resolution transforms
    let post_resolve = S::compute("resolution_summary", |s| {
        let category = s
            .get("issue_category")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let resolution = s
            .get("resolution")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        json!(format!("[{}] {}", category.to_uppercase(), resolution))
    }) >> S::counter("total_tickets", 1)
        >> S::history("resolution", 10);

    state["resolution"] = json!("Refund of $50 processed to original payment method");
    post_resolve.apply(&mut state);
    println!("\nPost-resolution:");
    println!(
        "  resolution_summary: {:?}",
        state.get("resolution_summary")
    );
    println!("  total_tickets: {:?}", state.get("total_tickets"));

    // ── 4. Guards for output safety ──
    println!("\n--- Output Guards ---\n");

    let support_guards = G::length(10, 2000)
        | G::pii()
        | G::topic(&["competitor_pricing", "internal_policy", "employee_names"])
        | G::custom(|output| {
            // Ensure resolution contains an action
            if output.contains("resolution")
                || output.contains("resolved")
                || output.contains("will")
                || output.contains("has been")
            {
                Ok(())
            } else {
                Err("Response should indicate an action taken or planned".into())
            }
        });

    println!("Support guards: {} validators", support_guards.len());

    let safe_response =
        "Your refund of $50 has been processed and will appear in 3-5 business days.";
    let violations = support_guards.check_all(safe_response);
    println!("  Safe response: {} violations", violations.len());

    // ── 5. Contract validation ──
    println!("\n--- Contract Validation ---\n");

    let all_agents = [
        intake.clone(),
        billing_specialist.clone(),
        tech_specialist.clone(),
        account_specialist.clone(),
        general_agent.clone(),
        escalation_agent.clone(),
        closer.clone(),
    ];

    let violations = check_contracts(&all_agents);
    let unproduced: Vec<_> = violations
        .iter()
        .filter(|v| matches!(v, ContractViolation::UnproducedKey { .. }))
        .collect();
    let duplicates: Vec<_> = violations
        .iter()
        .filter(|v| matches!(v, ContractViolation::DuplicateWrite { .. }))
        .collect();

    println!("Contract analysis:");
    println!("  Total violations: {}", violations.len());
    println!("  Unproduced keys: {}", unproduced.len());
    println!("  Duplicate writes: {}", duplicates.len());

    if !duplicates.is_empty() {
        println!("\n  Duplicate writes (expected -- routing selects one):");
        for v in &duplicates {
            if let ContractViolation::DuplicateWrite { agents, key } = v {
                println!("    '{}' written by: {:?}", key, agents);
            }
        }
    }

    // ── 6. Evaluation suite ──
    println!("\n--- Evaluation Suite ---\n");

    let eval_suite = E::suite()
        .case(
            "I was charged twice for my subscription",
            "duplicate charge refund processed",
        )
        .case(
            "My app keeps crashing on startup",
            "troubleshooting steps provided",
        )
        .case(
            "I need to close my account",
            "account closure process initiated",
        )
        .case(
            "Can you tell me about your pricing?",
            "pricing information provided",
        )
        .criteria(&["contains_match", "safety"]);

    println!(
        "Eval suite: {} test cases, {} criteria",
        eval_suite.len(),
        eval_suite.criteria_names.len()
    );

    let eval_criteria = E::contains_match() | E::safety();
    for case in &eval_suite.cases {
        let scores = eval_criteria.score_all(&case.expected, &case.expected);
        println!(
            "  Case '{}...' -> scores: {:?}",
            &case.prompt[..case.prompt.len().min(40)],
            scores
                .iter()
                .map(|(n, s)| format!("{}={:.1}", n, s))
                .collect::<Vec<_>>()
        );
    }

    // ── 7. S module predicates for routing ──
    println!("\n--- S Module Predicates ---\n");

    // These predicates would be used with RouteTextAgent or phase transitions
    let is_billing = S::eq("issue_category", "billing");
    let is_technical = S::eq("issue_category", "technical");
    let is_high_urgency = S::eq("urgency", "high");
    let needs_escalation = S::one_of("issue_category", &["legal", "security_breach"]);

    // Test with harness state
    let harness = AgentHarness::new()
        .set("issue_category", "billing")
        .set("urgency", "high");

    let s = harness.state();
    println!("  is_billing: {}", is_billing(s));
    println!("  is_technical: {}", is_technical(s));
    println!("  is_high_urgency: {}", is_high_urgency(s));
    println!("  needs_escalation: {}", needs_escalation(s));

    println!("\nCustomer support pipeline example completed successfully!");
}
