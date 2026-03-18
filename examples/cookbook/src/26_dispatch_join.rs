//! Cookbook #26 — DispatchTextAgent + JoinTextAgent
//!
//! Demonstrates fire-and-forget async background dispatch with a shared
//! task registry and join for collecting results.
//!
//! Pattern:
//!   1. DispatchTextAgent spawns background tasks with semaphore budget
//!   2. Main pipeline continues without waiting
//!   3. JoinTextAgent collects results when needed (with optional timeout)

use adk_rs_fluent::prelude::*;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

// A mock LLM that echoes its instruction as the response.
struct EchoLlm;

#[async_trait::async_trait]
impl BaseLlm for EchoLlm {
    fn model_id(&self) -> &str { "echo" }
    async fn generate(
        &self,
        req: rs_adk::llm::LlmRequest,
    ) -> Result<rs_adk::llm::LlmResponse, rs_adk::llm::LlmError> {
        let text = req.system_instruction.unwrap_or_else(|| "no-instruction".into());
        // Simulate some work
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(rs_adk::llm::LlmResponse {
            content: rs_genai::prelude::Content {
                role: Some(rs_genai::prelude::Role::Model),
                parts: vec![rs_genai::prelude::Part::Text { text }],
            },
            finish_reason: Some("STOP".into()),
            usage: None,
        })
    }
}

#[tokio::main]
async fn main() {
    println!("=== Cookbook #26: DispatchTextAgent + JoinTextAgent ===\n");

    let llm: Arc<dyn BaseLlm> = Arc::new(EchoLlm);

    // ── 1. Build agents for background tasks ──

    let email_agent = AgentBuilder::new("send-email")
        .instruction("Compose and send a confirmation email to the customer")
        .build(llm.clone());

    let log_agent = AgentBuilder::new("audit-log")
        .instruction("Write an audit log entry for the transaction")
        .build(llm.clone());

    let analytics_agent = AgentBuilder::new("analytics")
        .instruction("Update analytics dashboard with the new transaction data")
        .build(llm.clone());

    let notification_agent = AgentBuilder::new("push-notification")
        .instruction("Send a push notification to the customer's mobile app")
        .build(llm.clone());

    println!("Defined 4 background agents:");
    println!("  - send-email");
    println!("  - audit-log");
    println!("  - analytics");
    println!("  - push-notification");

    // ── 2. Create task registry and dispatch agent ──

    let registry = TaskRegistry::new();
    let budget = Arc::new(tokio::sync::Semaphore::new(3)); // Max 3 concurrent tasks

    let dispatcher = DispatchTextAgent::new(
        "background-dispatcher",
        vec![
            ("email".into(), email_agent),
            ("audit".into(), log_agent),
            ("analytics".into(), analytics_agent),
            ("notification".into(), notification_agent),
        ],
        registry.clone(),
        budget.clone(),
    );

    println!("\nDispatcher created with budget=3 concurrent tasks");

    // ── 3. Create join agent ──

    let joiner = JoinTextAgent::new("result-collector", registry.clone())
        .timeout(Duration::from_secs(5));

    println!("Joiner created with 5s timeout");

    // ── 4. Run the dispatch + join pipeline ──
    println!("\n--- Running Pipeline ---\n");

    let state = State::new();
    state.set("customer_id", "CUST-42");
    state.set("transaction_id", "TXN-12345");

    // Dispatch fires all tasks in background
    println!("Dispatching background tasks...");
    let dispatch_result = dispatcher.run(&state).await.unwrap();
    println!("  Dispatch returned immediately: '{}'", dispatch_result);
    println!("  Dispatch status: {:?}", state.get::<serde_json::Value>("_dispatch_status"));

    // Main pipeline work happens here while background tasks run
    println!("\n  [Main pipeline continues while tasks run in background]");

    // Join waits for all background tasks to complete
    println!("\nJoining background tasks...");
    let join_result = joiner.run(&state).await.unwrap();
    println!("  Join collected {} results", join_result.lines().count());

    // Individual results are stored in state
    println!("\n  Individual results in state:");
    if let Some(email_result) = state.get::<String>("_result_email") {
        println!("    email: {}...", &email_result[..email_result.len().min(60)]);
    }
    if let Some(audit_result) = state.get::<String>("_result_audit") {
        println!("    audit: {}...", &audit_result[..audit_result.len().min(60)]);
    }

    // ── 5. Selective join (wait for specific tasks only) ──
    println!("\n--- Selective Join ---\n");

    let registry2 = TaskRegistry::new();
    let budget2 = Arc::new(tokio::sync::Semaphore::new(2));

    let fast_task = AgentBuilder::new("fast-task")
        .instruction("Quick validation check")
        .build(llm.clone());

    let slow_task = AgentBuilder::new("slow-task")
        .instruction("Comprehensive background analysis")
        .build(llm.clone());

    let dispatcher2 = DispatchTextAgent::new(
        "selective-dispatcher",
        vec![
            ("critical".into(), fast_task),
            ("background".into(), slow_task),
        ],
        registry2.clone(),
        budget2,
    );

    // Only join the critical task, let background continue
    let critical_joiner = JoinTextAgent::new("critical-only", registry2.clone())
        .targets(vec!["critical".into()])
        .timeout(Duration::from_secs(2));

    let state2 = State::new();
    dispatcher2.run(&state2).await.unwrap();
    println!("Dispatched 2 tasks, joining only 'critical'...");

    let critical_result = critical_joiner.run(&state2).await.unwrap();
    println!("  Critical task result: {}...", &critical_result[..critical_result.len().min(50)]);
    println!("  Background task still running (not joined)");

    // ── 6. Compose dispatch/join in a sequential pipeline ──
    println!("\n--- Composable Pipeline with Dispatch/Join ---\n");

    // Build a pipeline: main agent >> dispatch side effects >> continue >> join results
    let main_agent = AgentBuilder::new("main-processor")
        .instruction("Process the customer request and determine next steps")
        .build(llm.clone());

    let summary_agent = AgentBuilder::new("summarizer")
        .instruction("Summarize all results from the pipeline")
        .build(llm.clone());

    // Using SequentialTextAgent to compose dispatch/join with other agents
    let pipeline = SequentialTextAgent::new(
        "dispatch-join-pipeline",
        vec![
            main_agent,
            Arc::new(dispatcher) as Arc<dyn TextAgent>,
            // ... other pipeline work happens here ...
            Arc::new(joiner) as Arc<dyn TextAgent>,
            summary_agent,
        ],
    );

    println!("Sequential pipeline: main >> dispatch >> join >> summarize");
    println!("Pipeline name: {}", pipeline.name());

    // ── 7. TapTextAgent for observation ──
    println!("\n--- TapTextAgent for Observation ---\n");

    let tap = TapTextAgent::new("observer", |state: &State| {
        let output: Option<String> = state.get("output");
        println!("  [TAP] Current output length: {}",
            output.map(|s| s.len()).unwrap_or(0));
    });

    let state3 = State::new();
    state3.set("output", "Some intermediate result");
    tap.run(&state3).await.unwrap();
    println!("  Tap agent ran (read-only observation, no mutation)");

    // ── 8. Budget management patterns ──
    println!("\n--- Budget Management ---\n");

    // Demonstrate how the semaphore limits concurrency
    let tight_budget = Arc::new(tokio::sync::Semaphore::new(1)); // Only 1 at a time
    println!("  Budget of 1: tasks execute one at a time (serialized)");

    let generous_budget = Arc::new(tokio::sync::Semaphore::new(100)); // Practically unlimited
    println!("  Budget of 100: tasks execute concurrently (fire-and-forget)");

    let production_budget = Arc::new(tokio::sync::Semaphore::new(5));
    println!("  Budget of 5: balanced concurrency for production use");

    println!("\nDispatch/Join pipeline example completed successfully!");
}
