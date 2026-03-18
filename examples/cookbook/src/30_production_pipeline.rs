//! Cookbook #30 — Production Pipeline: End-to-End
//!
//! Combines everything into a production-ready pipeline:
//!   - State transforms (S) for input normalization and validation
//!   - Prompt composition (P) for instruction engineering
//!   - Tool composition (T) for capability wiring
//!   - Guards (G) for output safety
//!   - Evaluation (E) for quality gates
//!   - Artifacts (A) for I/O schema documentation
//!   - Contract validation for pipeline integrity
//!   - Operator algebra for agent composition
//!   - Patterns for common workflows
//!
//! This example models a loan application processing pipeline.

use gemini_adk_fluent::prelude::*;
#[allow(unused_imports)]
use serde_json::json;
use std::sync::Arc;
#[allow(unused_imports)]
use std::time::Duration;

// Mock LLM for demonstration.
struct MockLlm;

#[async_trait::async_trait]
impl BaseLlm for MockLlm {
    fn model_id(&self) -> &str {
        "mock-production"
    }
    async fn generate(
        &self,
        req: gemini_adk::llm::LlmRequest,
    ) -> Result<gemini_adk::llm::LlmResponse, gemini_adk::llm::LlmError> {
        let instruction = req.system_instruction.unwrap_or_default();
        let response = if instruction.contains("risk") {
            json!({
                "risk_score": 0.35,
                "risk_level": "medium",
                "factors": ["income_ratio", "credit_history"],
                "approved": true
            })
            .to_string()
        } else if instruction.contains("compliance") {
            "COMPLIANT: All regulatory requirements met.".to_string()
        } else {
            format!("[{}]", instruction.chars().take(50).collect::<String>())
        };
        Ok(gemini_adk::llm::LlmResponse {
            content: gemini_live::prelude::Content {
                role: Some(gemini_live::prelude::Role::Model),
                parts: vec![gemini_live::prelude::Part::Text { text: response }],
            },
            finish_reason: Some("STOP".into()),
            usage: None,
        })
    }
}

#[tokio::main]
async fn main() {
    println!("=== Cookbook #30: Production Pipeline ===\n");

    let llm: Arc<dyn BaseLlm> = Arc::new(MockLlm);

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 1: Input Validation and Normalization
    // ════════════════════════════════════════════════════════════════════════

    println!("=== STAGE 1: Input Validation ===\n");

    // State transform pipeline for input normalization
    let input_validator = S::require(&["applicant_name", "income", "loan_amount"])
        >> S::validate(json!({
            "required": ["applicant_name", "income", "loan_amount"],
            "properties": {
                "applicant_name": {"type": "string"},
                "income": {"type": "number"},
                "loan_amount": {"type": "number"},
                "credit_score": {"type": "number"},
                "employment_years": {"type": "number"}
            }
        }))
        >> S::defaults(json!({
            "credit_score": 650,
            "employment_years": 0,
            "loan_type": "personal",
            "status": "pending"
        }))
        >> S::compute("debt_to_income", |s| {
            let income = s.get("income").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let loan = s.get("loan_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            json!((loan / income * 100.0).round() / 100.0)
        })
        >> S::branch(
            |s| s.get("loan_amount").and_then(|v| v.as_f64()).unwrap_or(0.0) > 100_000.0,
            S::set("requires_manual_review", json!(true)),
            S::set("requires_manual_review", json!(false)),
        );

    let mut application = json!({
        "applicant_name": "Alice Johnson",
        "income": 85000.0,
        "loan_amount": 25000.0,
        "credit_score": 720,
        "employment_years": 5
    });

    input_validator.apply(&mut application);
    println!("Validated application:");
    println!("{}", serde_json::to_string_pretty(&application).unwrap());

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 2: Agent Definitions with Full Configuration
    // ════════════════════════════════════════════════════════════════════════

    println!("\n=== STAGE 2: Agent Definitions ===\n");

    // Risk assessment agent
    let risk_assessor = AgentBuilder::new("risk-assessor")
        .instruction(
            "Assess the loan application risk. Consider: credit score, \
             debt-to-income ratio, employment history, and loan amount. \
             Output a risk score (0-1), risk level, and key risk factors.",
        )
        .model(GeminiModel::Gemini2_0FlashLive)
        .temperature(0.1)
        .thinking(4096)
        .output_schema(json!({
            "type": "object",
            "properties": {
                "risk_score": {"type": "number"},
                "risk_level": {"type": "string", "enum": ["low", "medium", "high"]},
                "factors": {"type": "array", "items": {"type": "string"}},
                "approved": {"type": "boolean"}
            },
            "required": ["risk_score", "risk_level", "approved"]
        }))
        .reads("applicant_name")
        .reads("income")
        .reads("loan_amount")
        .reads("credit_score")
        .reads("debt_to_income")
        .writes("risk_score")
        .writes("risk_level")
        .writes("risk_approved");

    // Compliance checker
    let compliance_checker = AgentBuilder::new("compliance-checker")
        .instruction(
            "Check the loan application against regulatory compliance rules: \
             KYC, AML, fair lending, and consumer protection regulations.",
        )
        .model(GeminiModel::Gemini2_0FlashLive)
        .temperature(0.0)
        .reads("applicant_name")
        .reads("loan_amount")
        .reads("loan_type")
        .writes("compliance_status")
        .writes("compliance_flags");

    // Fraud detection agent
    let fraud_detector = AgentBuilder::new("fraud-detector")
        .instruction(
            "Analyze the application for fraud indicators. Check for: \
             identity inconsistencies, suspicious patterns, and known fraud signals.",
        )
        .temperature(0.0)
        .reads("applicant_name")
        .reads("income")
        .reads("employment_years")
        .writes("fraud_score")
        .writes("fraud_flags");

    // Decision agent
    let decision_maker = AgentBuilder::new("decision-maker")
        .instruction(
            "Make a final loan decision based on risk assessment, compliance check, \
             and fraud detection results. Set approved=true if all checks pass \
             and risk is acceptable.",
        )
        .temperature(0.2)
        .reads("risk_score")
        .reads("risk_level")
        .reads("risk_approved")
        .reads("compliance_status")
        .reads("fraud_score")
        .reads("requires_manual_review")
        .writes("decision")
        .writes("approved")
        .writes("conditions");

    // Communication agent
    let communicator = AgentBuilder::new("communicator")
        .instruction(
            "Draft a professional notification to the applicant about the loan decision. \
             Be clear about the outcome, any conditions, and next steps.",
        )
        .temperature(0.5)
        .reads("applicant_name")
        .reads("decision")
        .reads("approved")
        .reads("conditions")
        .writes("notification");

    let all_agents = [
        risk_assessor.clone(),
        compliance_checker.clone(),
        fraud_detector.clone(),
        decision_maker.clone(),
        communicator.clone(),
    ];

    for agent in &all_agents {
        println!("{}\n", diagnose(agent));
    }

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 3: Pipeline Composition
    // ════════════════════════════════════════════════════════════════════════

    println!("=== STAGE 3: Pipeline Composition ===\n");

    // Parallel assessment: risk + compliance + fraud
    let _assessment_fanout =
        risk_assessor.clone() | compliance_checker.clone() | fraud_detector.clone();

    println!("Assessment fan-out: 3 parallel branches");

    // Fan-out-merge: parallel assessment then decision
    let assessment_merge = fan_out_merge(
        vec![
            risk_assessor.clone(),
            compliance_checker.clone(),
            fraud_detector.clone(),
        ],
        decision_maker.clone(),
    );

    println!("Fan-out-merge: (risk | compliance | fraud) >> decision");

    // Full pipeline with fallback
    let fallback_decision = AgentBuilder::new("manual-review")
        .instruction("Flag for manual review: automated decision was inconclusive")
        .reads("risk_score")
        .writes("decision")
        .writes("approved");

    let full_pipeline = assessment_merge >> (communicator.clone() / fallback_decision.clone());

    println!("Full pipeline: assessment_merge >> (communicator / manual_review)");
    if let Composable::Pipeline(p) = &full_pipeline {
        println!("  Top-level: {} steps\n", p.steps.len());
    }

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 4: Guards and Safety
    // ════════════════════════════════════════════════════════════════════════

    println!("=== STAGE 4: Output Guards ===\n");

    let output_guards = G::length(20, 5000)
        | G::pii()
        | G::topic(&["race", "gender", "religion", "national_origin"])
        | G::custom(|output| {
            // Ensure decision contains approval status
            let lower = output.to_lowercase();
            if lower.contains("approved")
                || lower.contains("denied")
                || lower.contains("pending")
                || lower.contains("review")
            {
                Ok(())
            } else {
                Err("Decision output must contain approval status".into())
            }
        })
        | G::budget(500);

    println!("Guards configured: {} validators", output_guards.len());

    // Test guards
    let good_output = "Your loan application has been approved with the following conditions: \
                        maintain employment and provide updated income documentation in 6 months.";
    let bad_output = "user@email.com denied because of race";

    println!(
        "  Good output: {} violations",
        output_guards.check_all(good_output).len()
    );
    let bad_violations = output_guards.check_all(bad_output);
    println!("  Bad output: {} violations", bad_violations.len());
    for v in &bad_violations {
        println!("    - {}", v);
    }

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 5: Evaluation
    // ════════════════════════════════════════════════════════════════════════

    println!("\n=== STAGE 5: Evaluation Suite ===\n");

    let eval_criteria = E::custom("decision_present", |output, _| {
        let lower = output.to_lowercase();
        if lower.contains("approved") || lower.contains("denied") {
            1.0
        } else {
            0.0
        }
    }) | E::custom("professional_tone", |output, _| {
        let formal_words = ["application", "conditions", "documentation", "review"];
        let count = formal_words
            .iter()
            .filter(|w| output.to_lowercase().contains(*w))
            .count();
        (count as f64 / formal_words.len() as f64).min(1.0)
    }) | E::safety()
        | E::contains_match();

    let eval_suite = E::suite()
        .case("Low risk, good credit application", "approved")
        .case("High risk, poor credit application", "denied")
        .case("Borderline application", "pending review")
        .criteria(&["decision_present", "professional_tone", "safety"]);

    println!(
        "Eval suite: {} cases, {} criteria",
        eval_suite.len(),
        eval_suite.criteria_names.len()
    );

    // Score a sample output
    let sample = "After careful review, your loan application has been approved. \
                  Please provide documentation within 30 days.";
    let scores = eval_criteria.score_all(sample, "approved");
    println!("\nSample scores:");
    for (name, score) in &scores {
        println!("  {}: {:.2}", name, score);
    }

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 6: Contract Validation
    // ════════════════════════════════════════════════════════════════════════

    println!("\n=== STAGE 6: Contract Validation ===\n");

    let full_agents = [
        risk_assessor.clone(),
        compliance_checker.clone(),
        fraud_detector.clone(),
        decision_maker.clone(),
        communicator.clone(),
    ];

    let violations = check_contracts(&full_agents);
    let by_type: std::collections::HashMap<&str, Vec<_>> =
        violations
            .iter()
            .fold(std::collections::HashMap::new(), |mut acc, v| {
                let key = match v {
                    ContractViolation::UnproducedKey { .. } => "unproduced",
                    ContractViolation::DuplicateWrite { .. } => "duplicate",
                    ContractViolation::OrphanedOutput { .. } => "orphaned",
                };
                acc.entry(key).or_default().push(v);
                acc
            });

    println!("Contract analysis:");
    println!("  Total violations: {}", violations.len());
    for (category, items) in &by_type {
        println!("  {}: {}", category, items.len());
        for v in items {
            match v {
                ContractViolation::UnproducedKey { consumer, key } => {
                    println!("    {} reads '{}' (unproduced)", consumer, key);
                }
                ContractViolation::DuplicateWrite { agents, key } => {
                    println!("    '{}' written by {:?}", key, agents);
                }
                ContractViolation::OrphanedOutput { producer, key } => {
                    println!("    {} writes '{}' (orphaned)", producer, key);
                }
            }
        }
    }

    // Data flow graph
    let edges = infer_data_flow(&full_agents);
    println!("\nData flow ({} edges):", edges.len());
    for edge in &edges {
        println!("  {} --[{}]--> {}", edge.producer, edge.key, edge.consumer);
    }

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 7: Artifact Declarations
    // ════════════════════════════════════════════════════════════════════════

    println!("\n=== STAGE 7: Artifact Declarations ===\n");

    let artifacts = A::json_input("application", "Loan application form data")
        + A::json_output("risk_report", "Risk assessment report")
        + A::json_output("decision", "Loan decision with conditions")
        + A::text_output("notification", "Applicant notification letter");

    println!("Pipeline artifacts:");
    println!("  Inputs:");
    for input in artifacts.all_inputs() {
        println!(
            "    - {} ({}): {}",
            input.name, input.mime_type, input.description
        );
    }
    println!("  Outputs:");
    for output in artifacts.all_outputs() {
        println!(
            "    - {} ({}): {}",
            output.name, output.mime_type, output.description
        );
    }

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 8: Runtime Execution
    // ════════════════════════════════════════════════════════════════════════

    println!("\n=== STAGE 8: Runtime Execution ===\n");

    // Compile and run the pipeline
    let compiled = full_pipeline.compile(llm.clone());
    println!("Pipeline compiled. Name: {}", compiled.name());

    // Set up state with the application data
    let state = State::new();
    state.set("applicant_name", "Alice Johnson");
    state.set("income", 85000.0_f64);
    state.set("loan_amount", 25000.0_f64);
    state.set("credit_score", 720_u32);
    state.set("employment_years", 5_u32);
    state.set("debt_to_income", 0.29_f64);
    state.set("loan_type", "personal");
    state.set("requires_manual_review", false);
    state.set("input", "Process loan application for Alice Johnson");

    println!("Running pipeline...");
    match compiled.run(&state).await {
        Ok(result) => {
            println!("Pipeline completed successfully.");
            println!("Result: {}", &result[..result.len().min(200)]);

            // Check output against guards
            let guard_violations = output_guards.check_all(&result);
            if guard_violations.is_empty() {
                println!("Output passed all {} guard checks.", output_guards.len());
            } else {
                println!("Guard violations: {:?}", guard_violations);
            }
        }
        Err(e) => {
            println!("Pipeline error: {}", e);
        }
    }

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 9: State Inspection
    // ════════════════════════════════════════════════════════════════════════

    println!("\n=== STAGE 9: Post-Run State ===\n");

    // Inspect state after pipeline run
    let keys = [
        "applicant_name",
        "income",
        "loan_amount",
        "credit_score",
        "debt_to_income",
        "requires_manual_review",
        "output",
    ];

    for key in keys {
        if let Some(val) = state.get_raw(key) {
            let display = match &val {
                serde_json::Value::String(s) if s.len() > 60 => {
                    format!("\"{}...\"", &s[..57])
                }
                other => format!("{}", other),
            };
            println!("  {}: {}", key, display);
        }
    }

    // ════════════════════════════════════════════════════════════════════════
    //  STAGE 10: Prompt Composition Summary
    // ════════════════════════════════════════════════════════════════════════

    println!("\n=== STAGE 10: Prompt Summary ===\n");

    let production_prompt = P::role("senior loan underwriter at a financial institution")
        + P::task("Process and evaluate loan applications according to institutional policy")
        + P::constraint("Never approve loans with debt-to-income ratio above 0.45")
        + P::constraint("Always verify employment before approval")
        + P::constraint("Flag applications over $100k for manual review")
        + P::format("Structured decision with clear reasoning and conditions")
        + P::guidelines(&[
            "Prioritize risk management while maintaining fair lending practices",
            "Document every decision factor for audit trail",
            "Consider compensating factors for borderline applications",
            "Ensure regulatory compliance at every step",
        ])
        + P::context("Operating under current CFPB and OCC regulatory framework")
        + P::example(
            "Application: $25k personal loan, 720 credit, $85k income",
            "APPROVED with conditions: maintain employment, income reverification at 6 months",
        );

    println!(
        "Production prompt ({} sections):",
        production_prompt.sections.len()
    );
    println!("{}", production_prompt.render());

    println!("\n=== Production Pipeline Complete ===");
    println!("\nThis example demonstrated:");
    println!("  1. S -- State transforms for input validation");
    println!("  2. P -- Prompt composition for instruction engineering");
    println!("  3. T -- Tool composition for capabilities");
    println!("  4. G -- Guards for output safety");
    println!("  5. E -- Evaluation for quality gates");
    println!("  6. A -- Artifacts for I/O documentation");
    println!("  7. Contract validation for pipeline integrity");
    println!("  8. Operator algebra (>>, |, /, * until) for composition");
    println!("  9. Patterns (fan_out_merge, fallback) for common workflows");
    println!(" 10. Runtime execution with compiled pipeline");
}
