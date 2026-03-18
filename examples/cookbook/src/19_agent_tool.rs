//! Walk Example 19: Agent-as-Tool — Using an Agent Inside Another Agent
//!
//! Demonstrates TextAgentTool, which wraps a TextAgent as a callable tool.
//! This enables hierarchical agent composition: a parent agent can dispatch
//! complex reasoning to specialist sub-agents via tool calls, with shared
//! state flowing bidirectionally.
//!
//! Features used:
//!   - TextAgentTool (wraps TextAgent as ToolFunction)
//!   - FnTextAgent (zero-cost mock agents)
//!   - SequentialTextAgent (pipeline composition)
//!   - State (shared bidirectional state)
//!   - ToolFunction trait (tool interface)
//!   - AgentBuilder with sub_agent (declarative sub-agent registration)

use std::sync::Arc;

use adk_rs_fluent::prelude::*;
use rs_adk::text_agent_tool::TextAgentTool;
use rs_adk::tool::ToolFunction;

#[tokio::main]
async fn main() {
    println!("=== Walk 19: Agent-as-Tool ===\n");

    // ── Part 1: Basic TextAgentTool ──────────────────────────────────────
    // Wrap a simple agent as a tool and call it directly.

    println!("--- Part 1: Basic Agent Tool ---");

    // Create a specialist agent that performs identity verification
    let verifier_agent = FnTextAgent::new("identity_verifier", |state| {
        let request = state
            .get::<String>("input")
            .unwrap_or_else(|| "no request".into());

        // Simulate verification logic
        let name = state
            .get::<String>("customer_name")
            .unwrap_or_else(|| "Unknown".into());

        // Write verification result back to state (bidirectional)
        state.set("verified", true);
        state.set("verification_method", "knowledge-based");

        Ok(format!(
            "Identity verified for {name}. Method: knowledge-based authentication. \
             Request: {request}"
        ))
    });

    // Wrap the agent as a tool
    let shared_state = State::new();
    shared_state.set("customer_name", "Alice Johnson");

    let verify_tool = TextAgentTool::new(
        "verify_identity",
        "Verify a customer's identity using knowledge-based authentication",
        verifier_agent,
        shared_state.clone(),
    );

    // Inspect tool metadata
    println!("  Tool name: {}", verify_tool.name());
    println!("  Tool description: {}", verify_tool.description());
    println!("  Has parameters: {}", verify_tool.parameters().is_some());

    // Call the tool as if the parent model dispatched it
    let result = verify_tool
        .call(serde_json::json!({"request": "Verify Alice for account access"}))
        .await
        .unwrap();
    println!("  Result: {}", result["result"]);

    // Check bidirectional state flow
    println!(
        "  State 'verified': {:?}",
        shared_state.get::<bool>("verified")
    );
    println!(
        "  State 'verification_method': {:?}",
        shared_state.get::<String>("verification_method")
    );

    // ── Part 2: Pipeline Agent as Tool ───────────────────────────────────
    // Wrap a multi-step pipeline as a single tool invocation.

    println!("\n--- Part 2: Pipeline Agent as Tool ---");

    // Build a 3-step pipeline: research -> analyze -> recommend
    let researcher: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("researcher", |state| {
        let topic = state
            .get::<String>("input")
            .unwrap_or_else(|| "general".into());
        let findings = format!("Found 3 relevant papers on: {topic}");
        state.set("research_findings", &findings);
        Ok(findings)
    }));

    let analyzer: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("analyzer", |state| {
        let findings = state
            .get::<String>("research_findings")
            .unwrap_or_default();
        let analysis = format!("Analysis: {findings} -- Key insight: topic is well-studied.");
        state.set("analysis_result", &analysis);
        Ok(analysis)
    }));

    let recommender: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("recommender", |state| {
        let analysis = state
            .get::<String>("analysis_result")
            .unwrap_or_default();
        Ok(format!(
            "Recommendation based on: {analysis}\n\
             -> Proceed with implementation using established patterns."
        ))
    }));

    let pipeline = SequentialTextAgent::new(
        "research_pipeline",
        vec![researcher, analyzer, recommender],
    );

    // Wrap the entire pipeline as a tool
    let pipeline_state = State::new();
    let research_tool = TextAgentTool::new(
        "deep_research",
        "Conduct deep research: find papers, analyze, and recommend",
        pipeline,
        pipeline_state.clone(),
    );

    let result = research_tool
        .call(serde_json::json!({"request": "Rust ownership patterns"}))
        .await
        .unwrap();
    println!("  Pipeline result: {}", result["result"]);

    // Each pipeline step's state is visible
    println!(
        "  Research findings: {:?}",
        pipeline_state.get::<String>("research_findings")
    );
    println!(
        "  Analysis result: {:?}",
        pipeline_state.get::<String>("analysis_result")
    );

    // ── Part 3: Multiple Agent Tools ─────────────────────────────────────
    // Register several specialist agents as tools.

    println!("\n--- Part 3: Multiple Specialist Tools ---");

    let tool_state = State::new();
    tool_state.set("customer_tier", "premium");

    // Specialist 1: Payment calculator
    let payment_tool = TextAgentTool::new(
        "calculate_payment",
        "Calculate monthly payment plans",
        FnTextAgent::new("payment_calc", |state| {
            let args = state
                .get::<serde_json::Value>("agent_tool_args")
                .unwrap_or(serde_json::json!({}));
            let amount = args["request"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(1000.0);
            let monthly = amount / 12.0;
            state.set("last_calculation", monthly);
            Ok(format!(
                "Payment plan: ${amount:.2} total = ${monthly:.2}/month for 12 months"
            ))
        }),
        tool_state.clone(),
    );

    // Specialist 2: Account lookup
    let account_tool = TextAgentTool::new(
        "lookup_account",
        "Look up customer account details",
        FnTextAgent::new("account_lookup", |state| {
            let tier = state
                .get::<String>("customer_tier")
                .unwrap_or_else(|| "basic".into());
            state.set("account_found", true);
            Ok(format!(
                "Account found. Tier: {tier}. Balance: $5,230.00. \
                 Status: Active."
            ))
        }),
        tool_state.clone(),
    );

    // Call both tools
    let payment_result = payment_tool
        .call(serde_json::json!({"request": "2400"}))
        .await
        .unwrap();
    println!("  Payment: {}", payment_result["result"]);

    let account_result = account_tool
        .call(serde_json::json!({"request": "lookup"}))
        .await
        .unwrap();
    println!("  Account: {}", account_result["result"]);

    // Both tools share state
    println!(
        "  Shared state 'last_calculation': {:?}",
        tool_state.get::<f64>("last_calculation")
    );
    println!(
        "  Shared state 'account_found': {:?}",
        tool_state.get::<bool>("account_found")
    );

    // ── Part 4: Custom Tool Parameters ───────────────────────────────────

    println!("\n--- Part 4: Custom Parameters ---");

    let custom_tool = TextAgentTool::new(
        "search_knowledge_base",
        "Search the internal knowledge base",
        FnTextAgent::new("kb_search", |state| {
            let args = state
                .get::<serde_json::Value>("agent_tool_args")
                .unwrap_or(serde_json::json!({}));
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .unwrap_or("no query");
            let limit = args
                .get("limit")
                .and_then(|l| l.as_u64())
                .unwrap_or(5);
            Ok(format!("Found {limit} results for: {query}"))
        }),
        State::new(),
    )
    .with_parameters(serde_json::json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Search query"
            },
            "limit": {
                "type": "integer",
                "description": "Maximum results to return"
            }
        },
        "required": ["query"]
    }));

    println!("  Custom params schema:");
    if let Some(params) = custom_tool.parameters() {
        println!("    {}", serde_json::to_string_pretty(&params).unwrap());
    }

    let result = custom_tool
        .call(serde_json::json!({"query": "Rust ownership", "limit": 3}))
        .await
        .unwrap();
    println!("  Result: {}", result["result"]);

    // ── Part 5: AgentBuilder Sub-Agent Pattern ───────────────────────────
    // Declarative sub-agent registration using AgentBuilder.

    println!("\n--- Part 5: Declarative Sub-Agents ---");

    let parent = AgentBuilder::new("orchestrator")
        .instruction("You are a customer service orchestrator")
        .sub_agent(
            AgentBuilder::new("billing_specialist")
                .instruction("Handle billing inquiries")
                .description("Specialist for billing and payment questions"),
        )
        .sub_agent(
            AgentBuilder::new("tech_specialist")
                .instruction("Handle technical issues")
                .description("Specialist for technical troubleshooting"),
        );

    println!("  Parent: {}", parent.name());
    println!("  Sub-agents: {}", parent.get_sub_agents().len());
    for sub in parent.get_sub_agents() {
        println!(
            "    - {} ({})",
            sub.name(),
            sub.get_description().unwrap_or("no description")
        );
    }

    println!("\nDone.");
}
