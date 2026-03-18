//! Cookbook #28 — Agent-to-Agent (A2A) Protocol
//!
//! Demonstrates RemoteAgent and A2AServer for inter-agent communication.
//!
//! Patterns shown:
//!   1. RemoteAgent: client-side reference to a remote agent
//!   2. A2AServer: publishing a local agent for remote invocation
//!   3. AgentRegistry: discovering remote agents
//!   4. SkillDeclaration: advertising agent capabilities
//!   5. T::a2a: using remote agents as tools

use adk_rs_fluent::prelude::*;
use serde_json::json;
use std::time::Duration;

fn main() {
    println!("=== Cookbook #28: Agent-to-Agent (A2A) Protocol ===\n");

    // ── 1. Define remote agent references ──
    println!("--- 1. Remote Agent References ---\n");

    // A verification agent running on another service
    let verifier = RemoteAgent::new("identity-verifier")
        .endpoint("https://verify.agents.example.com/a2a")
        .timeout(Duration::from_secs(30))
        .describe("Verifies caller identity using KBA questions and document checks")
        .streaming(false);

    println!("Remote agent: {}", verifier.name());
    println!("  Endpoint: {:?}", verifier.get_endpoint());
    println!("  Timeout: {:?}", verifier.get_timeout());

    // A payment processor agent
    let payment_agent = RemoteAgent::new("payment-processor")
        .endpoint("https://payments.agents.example.com/a2a")
        .timeout(Duration::from_secs(60))
        .describe("Processes payments, refunds, and payment plan setup")
        .streaming(true);

    println!("\nRemote agent: {}", payment_agent.name());
    println!("  Endpoint: {:?}", payment_agent.get_endpoint());
    println!("  Streaming: enabled");

    // A fraud detection agent
    let fraud_detector = RemoteAgent::new("fraud-detector")
        .endpoint("https://fraud.agents.example.com/a2a")
        .timeout(Duration::from_secs(10))
        .describe("Real-time fraud scoring and transaction risk assessment");

    println!("\nRemote agent: {}", fraud_detector.name());
    println!(
        "  Timeout: {:?} (fast -- real-time scoring)",
        fraud_detector.get_timeout()
    );

    // ── 2. Define skill declarations ──
    println!("\n--- 2. Skill Declarations ---\n");

    let verify_skill = SkillDeclaration::new("verify_identity", "Identity Verification").describe(
        "Verify caller identity through knowledge-based authentication. \
                   Requires customer_id and returns verification_status.",
    );

    let payment_skill = SkillDeclaration::new("process_payment", "Payment Processing").describe(
        "Process payments, refunds, and payment plans. \
                   Supports credit card, ACH, and wire transfer.",
    );

    let fraud_skill = SkillDeclaration::new("score_transaction", "Transaction Risk Scoring")
        .describe(
            "Score a transaction for fraud risk on a 0-100 scale. \
                   Returns risk_score, risk_factors, and recommendation.",
        );

    println!("Skill: {} -- {}", verify_skill.id, verify_skill.name);
    println!("  {:?}", verify_skill.description);
    println!("Skill: {} -- {}", payment_skill.id, payment_skill.name);
    println!("  {:?}", payment_skill.description);
    println!("Skill: {} -- {}", fraud_skill.id, fraud_skill.name);
    println!("  {:?}", fraud_skill.description);

    // ── 3. Agent Registry for discovery ──
    println!("\n--- 3. Agent Registry ---\n");

    let registry = AgentRegistry::new("https://registry.agents.example.com");
    println!("Registry: {}", registry.base_url());
    println!("  In production, agents register here for discovery");
    println!("  Other agents can query the registry to find capabilities");

    // ── 4. A2AServer: publishing a local agent ──
    println!("\n--- 4. A2A Server Setup ---\n");

    // Publish a local agent for remote invocation
    let support_server = A2AServer::new("customer-support")
        .host("0.0.0.0")
        .port(8080)
        .health_check("/health")
        .streaming(true);

    println!(
        "Server: {} on {}:{}",
        support_server.agent_name(),
        support_server.get_host(),
        support_server.get_port()
    );

    // Multiple servers for a microservices architecture
    let billing_server = A2AServer::new("billing-service")
        .host("0.0.0.0")
        .port(8081)
        .health_check("/healthz");

    let notification_server = A2AServer::new("notification-service")
        .host("0.0.0.0")
        .port(8082)
        .health_check("/health");

    println!("\nMicroservice architecture:");
    println!(
        "  {} -> :{}",
        support_server.agent_name(),
        support_server.get_port()
    );
    println!(
        "  {} -> :{}",
        billing_server.agent_name(),
        billing_server.get_port()
    );
    println!(
        "  {} -> :{}",
        notification_server.agent_name(),
        notification_server.get_port()
    );

    // ── 5. T::a2a: Remote agents as tools ──
    println!("\n--- 5. Remote Agents as Tools ---\n");

    // Compose remote agents as tools alongside local tools
    let tools = T::a2a("https://verify.agents.example.com/a2a", "verify_identity")
        | T::a2a("https://payments.agents.example.com/a2a", "process_payment")
        | T::a2a("https://fraud.agents.example.com/a2a", "score_transaction")
        | T::simple(
            "lookup_account",
            "Look up account details",
            |args| async move {
                let id = args
                    .get("account_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                Ok(json!({
                    "account_id": id,
                    "name": "Alice Johnson",
                    "status": "active",
                    "tier": "premium"
                }))
            },
        )
        | T::google_search();

    println!(
        "Tool composite: {} tools (3 remote A2A + 1 local + 1 built-in)",
        tools.len()
    );

    // Use in an agent builder
    let orchestrator = AgentBuilder::new("orchestrator")
        .instruction(
            "You are a customer support orchestrator. Use available tools to: \
             1. Look up the customer's account \
             2. Verify their identity \
             3. Score the transaction for fraud \
             4. Process any payments or refunds \
             Route issues to the appropriate specialist service.",
        )
        .tools(tools)
        .temperature(0.2);

    println!("\nOrchestrator agent:");
    println!("  Name: {}", orchestrator.name());
    println!("  Tools: {}", orchestrator.tool_count());
    println!("  Temperature: {:?}", orchestrator.get_temperature());

    // ── 6. Multi-service pipeline with A2A ──
    println!("\n--- 6. Multi-Service Pipeline ---\n");

    // Define the full pipeline using agent composition
    let intake = AgentBuilder::new("intake")
        .instruction("Collect customer info and classify the issue")
        .writes("customer_id")
        .writes("issue_type");

    let verifier_local = AgentBuilder::new("verify-proxy")
        .instruction("Call the remote identity verifier via A2A")
        .reads("customer_id")
        .writes("identity_verified");

    let fraud_check = AgentBuilder::new("fraud-check-proxy")
        .instruction("Call the remote fraud detector via A2A")
        .reads("customer_id")
        .writes("fraud_score");

    let resolver = AgentBuilder::new("resolver")
        .instruction("Resolve the issue using verified identity and fraud assessment")
        .reads("identity_verified")
        .reads("fraud_score")
        .reads("issue_type")
        .writes("resolution");

    let notifier = AgentBuilder::new("notifier-proxy")
        .instruction("Send resolution notification via A2A notification service")
        .reads("customer_id")
        .reads("resolution")
        .writes("notification_sent");

    // Pipeline: intake >> (verify | fraud_check) >> resolve >> notify
    let a2a_pipeline = intake.clone()
        >> (verifier_local.clone() | fraud_check.clone())
        >> resolver.clone()
        >> notifier.clone();

    println!("A2A pipeline: intake >> (verify | fraud) >> resolve >> notify");
    if let Composable::Pipeline(p) = &a2a_pipeline {
        println!("  {} steps", p.steps.len());
    }

    // Contract validation
    let all_agents = [intake, verifier_local, fraud_check, resolver, notifier];
    let violations = check_contracts(&all_agents);
    println!("\nContract validation: {} violations", violations.len());
    for v in &violations {
        match v {
            ContractViolation::OrphanedOutput { producer, key } => {
                println!(
                    "  ORPHANED: {} writes '{}' (OK -- consumed externally)",
                    producer, key
                );
            }
            _ => {
                println!("  {:?}", v);
            }
        }
    }

    // Data flow
    let edges = infer_data_flow(&all_agents);
    println!("\nData flow:");
    for edge in &edges {
        println!("  {} --[{}]--> {}", edge.producer, edge.key, edge.consumer);
    }

    // ── 7. T module: MCP and OpenAPI integrations ──
    println!("\n--- 7. MCP and OpenAPI Tools ---\n");

    let extended_tools = T::mcp("npx @modelcontextprotocol/server-filesystem /data")
        | T::openapi("crm-api", "https://api.example.com/openapi.json")
        | T::search("knowledge-base", "Search internal knowledge base")
        | T::a2a("https://agents.example.com/analytics", "run_report");

    println!("Extended tool composite: {} entries", extended_tools.len());
    println!("  Includes: MCP, OpenAPI, BM25 search, and A2A tools");

    // ── 8. Production A2A architecture ──
    println!("\n--- 8. Production Architecture ---\n");

    println!("Service mesh:");
    println!("  +-----------+     A2A     +------------------+");
    println!("  |  Gateway  | ---------> | Identity Verifier |");
    println!("  | (port 80) |            +------------------+");
    println!("  |           |     A2A     +------------------+");
    println!("  |           | ---------> | Fraud Detector    |");
    println!("  |           |            +------------------+");
    println!("  |           |     A2A     +------------------+");
    println!("  |           | ---------> | Payment Processor |");
    println!("  |           |            +------------------+");
    println!("  |           |     A2A     +------------------+");
    println!("  |           | ---------> | Notification Svc  |");
    println!("  +-----------+            +------------------+");
    println!("        |");
    println!("  +-----+-----+");
    println!("  |  Registry  |  Agent discovery and health monitoring");
    println!("  +-----------+");

    println!("\nA2A protocol example completed successfully!");
}
