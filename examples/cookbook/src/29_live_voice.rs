//! Cookbook #29 — Live Voice Session with Phases, Tools, and Extraction
//!
//! Shows the full Live::builder() API for voice sessions.
//! This example builds the session configuration but does NOT connect
//! (no API key required). It demonstrates the builder pattern for:
//!
//!   - Phase machine with transitions and guards
//!   - Tool composition for live sessions
//!   - Turn extraction with typed schemas
//!   - Watchers and temporal patterns
//!   - Control plane features (steering, repair, persistence)
//!   - Callbacks (audio, text, thought, interruption)

use gemini_adk_fluent_rs::prelude::*;
use serde_json::json;

fn main() {
    println!("=== Cookbook #29: Live Voice Session ===\n");

    // ── 1. Basic Live session builder ──
    println!("--- 1. Basic Voice Session ---\n");

    // This shows the builder chain without actually connecting.
    // In production, you would call .connect_google_ai(key) or
    // .connect_vertex(project, location, token) at the end.
    println!("Live::builder()");
    println!("    .model(GeminiModel::Gemini2_0FlashLive)");
    println!("    .voice(Voice::Kore)");
    println!("    .instruction(\"You are a helpful assistant\")");
    println!("    .greeting(\"Hello! How can I help you today?\")");
    println!("    .transcription(true, true)");
    println!("    .on_audio(|data| {{ /* play audio */ }})");
    println!("    .on_text(|t| print!(\"{{t}}\"))");
    println!("    .connect_google_ai(api_key)  // not called in demo");

    // ── 2. Phase machine configuration ──
    println!("\n--- 2. Phase Machine ---\n");

    // Demonstrate the phase builder pattern (no actual connection)
    println!("Phase machine configuration:");
    println!("  greeting -> identification -> resolution -> farewell\n");

    // Show phase definitions using the builder syntax
    println!("  .phase(\"greeting\")");
    println!("      .instruction(\"Welcome the caller warmly\")");
    println!("      .transition(\"identification\", S::is_true(\"greeted\"))");
    println!("      .prompt_on_enter(true)");
    println!("      .done()");
    println!("  .phase(\"identification\")");
    println!("      .instruction(\"Verify the caller's identity\")");
    println!("      .needs(&[\"customer_id\", \"verified\"])");
    println!("      .transition(\"resolution\", S::is_true(\"verified\"))");
    println!("      .guard(S::is_set(\"customer_id\"))");
    println!("      .done()");
    println!("  .phase(\"resolution\")");
    println!("      .dynamic_instruction(|s| {{");
    println!("          let issue = s.get::<String>(\"issue_type\").unwrap_or_default();");
    println!("          format!(\"Resolve the {{}} issue\", issue)");
    println!("      }})");
    println!("      .tools(vec![\"lookup_account\", \"process_refund\"])");
    println!("      .transition(\"farewell\", S::is_true(\"resolved\"))");
    println!("      .done()");
    println!("  .phase(\"farewell\")");
    println!("      .instruction(\"Thank the customer and offer follow-up\")");
    println!("      .terminal()");
    println!("      .done()");
    println!("  .initial_phase(\"greeting\")");

    // ── 3. Tool composition for live sessions ──
    println!("\n--- 3. Live Session Tools ---\n");

    let tools = T::simple(
        "lookup_account",
        "Look up customer account details",
        |args| async move {
            let id = args
                .get("account_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            Ok(json!({
                "account_id": id,
                "name": "Alice Johnson",
                "balance": 1250.00,
                "status": "active"
            }))
        },
    ) | T::simple(
        "process_refund",
        "Process a refund for the customer",
        |args| async move {
            let amount = args.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            Ok(json!({
                "refund_id": "REF-12345",
                "amount": amount,
                "status": "processed"
            }))
        },
    ) | T::simple(
        "check_order_status",
        "Check the status of an order",
        |args| async move {
            let order_id = args
                .get("order_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            Ok(json!({
                "order_id": order_id,
                "status": "shipped",
                "eta": "2024-03-20"
            }))
        },
    ) | T::google_search();

    println!("Live session tools: {} total", tools.len());
    println!("  - lookup_account (custom)");
    println!("  - process_refund (custom)");
    println!("  - check_order_status (custom)");
    println!("  - google_search (built-in)");

    // ── 4. Prompt composition for phases ──
    println!("\n--- 4. Phase Prompts ---\n");

    let greeting_prompt = P::role("friendly customer support agent")
        + P::task("Welcome the caller and establish rapport")
        + P::guidelines(&[
            "Use the caller's name if known",
            "Ask how you can help today",
            "Be warm but professional",
        ]);

    let resolution_prompt = P::role("knowledgeable support specialist")
        + P::task("Resolve the customer's issue efficiently")
        + P::constraint("Always verify account before making changes")
        + P::constraint("Never share internal policy details")
        + P::format("Explain each step clearly to the customer");

    println!(
        "Greeting prompt: {} sections",
        greeting_prompt.sections.len()
    );
    println!(
        "Resolution prompt: {} sections",
        resolution_prompt.sections.len()
    );

    // ── 5. State predicates for transitions ──
    println!("\n--- 5. State Predicates ---\n");

    let state = State::new();
    state.set("greeted", true);
    state.set("customer_id", "CUST-42");
    state.set("verified", false);
    state.set("issue_type", "billing");
    state.set("resolved", false);

    // S module predicates used in phase transitions
    let greeted_check = S::is_true("greeted");
    let verified_check = S::is_true("verified");
    let resolved_check = S::is_true("resolved");
    let has_customer = S::is_set("customer_id");
    let is_billing = S::eq("issue_type", "billing");

    println!("Current state:");
    println!(
        "  greeted:    {} (transition to identification)",
        greeted_check(&state)
    );
    println!(
        "  customer_id set: {} (guard for identification)",
        has_customer(&state)
    );
    println!(
        "  verified:   {} (transition to resolution)",
        verified_check(&state)
    );
    println!(
        "  is_billing: {} (determines resolution path)",
        is_billing(&state)
    );
    println!(
        "  resolved:   {} (transition to farewell)",
        resolved_check(&state)
    );

    // ── 6. Extraction schema ──
    println!("\n--- 6. Turn Extraction ---\n");

    // In a real live session, you would use:
    //   .extract_turns::<ConversationState>(llm, "Extract the current state")
    // The schema would be auto-generated from the struct via schemars.

    println!("Extraction schema (what extract_turns would use):");
    let extraction_schema = json!({
        "type": "object",
        "properties": {
            "customer_name": {"type": "string", "description": "Customer's full name"},
            "account_id": {"type": "string", "description": "Customer account identifier"},
            "issue_type": {
                "type": "string",
                "enum": ["billing", "technical", "account", "general"]
            },
            "sentiment": {
                "type": "string",
                "enum": ["positive", "neutral", "frustrated", "angry"]
            },
            "resolved": {"type": "boolean"},
            "action_items": {
                "type": "array",
                "items": {"type": "string"}
            }
        }
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&extraction_schema).unwrap()
    );

    // ── 7. Watchers and temporal patterns ──
    println!("\n--- 7. Watchers & Temporal Patterns ---\n");

    println!("Watcher configuration:");
    println!("  .watch(\"app:sentiment\")");
    println!("      .changed_to(json!(\"angry\"))");
    println!("      .then(|old, new, state| async {{ state.set(\"escalate\", true); }})");
    println!("  .watch(\"app:satisfaction_score\")");
    println!("      .crossed_above(0.8)");
    println!("      .then(|old, new, state| async {{ state.set(\"upsell_eligible\", true); }})");
    println!();
    println!("Temporal patterns:");
    println!("  .when_sustained(\"confused\",");
    println!("      |s| s.get::<bool>(\"confused\").unwrap_or(false),");
    println!("      Duration::from_secs(30),");
    println!("      |state, writer| async {{ /* offer help */ }})");
    println!("  .when_turns(\"stuck\",");
    println!("      |s| s.get::<bool>(\"repeating\").unwrap_or(false),");
    println!("      3,");
    println!("      |state, writer| async {{ /* break loop */ }})");

    // ── 8. Control plane features ──
    println!("\n--- 8. Control Plane ---\n");

    println!("Steering modes:");
    println!("  SteeringMode::ContextInjection   -- base instruction once, phase via context");
    println!("  SteeringMode::InstructionUpdate   -- full instruction replacement on transition");
    println!("  SteeringMode::Hybrid              -- both instruction + context\n");

    println!("Context delivery:");
    println!("  ContextDelivery::Immediate  -- send during TurnComplete");
    println!("  ContextDelivery::Deferred   -- queue, flush before next user send\n");

    println!("Repair configuration:");
    println!("  RepairConfig::default()  -- nudge after 3 turns, escalate after 6");
    println!("  RepairConfig::new().nudge_after(2).escalate_after(5)\n");

    println!("Other features:");
    println!("  .soft_turn_timeout(Duration::from_secs(2))  -- proactive silence detection");
    println!("  .tool_advisory(true)                        -- signal tools on phase change");
    println!("  .thinking(1024)                             -- thinking budget (Google AI)");
    println!("  .include_thoughts()                         -- receive thought summaries");

    // ── 9. Session persistence ──
    println!("\n--- 9. Session Persistence ---\n");

    println!("Persistence backends:");
    println!("  FsPersistence::new(\"/tmp/sessions\")   -- filesystem-backed");
    println!("  MemoryPersistence::new()               -- in-memory (tests)\n");

    println!("Usage:");
    println!("  .persistence(Arc::new(FsPersistence::new(\"/tmp/sessions\")))");
    println!("  .session_id(\"user-123-session-456\")");
    println!("  // Session survives process restarts");

    // ── 10. Full production configuration ──
    println!("\n--- 10. Full Production Configuration ---\n");

    println!("// Full production Live session setup:");
    println!("let handle = Live::builder()");
    println!("    .model(GeminiModel::Gemini2_0FlashLive)");
    println!("    .voice(Voice::Kore)");
    println!("    .instruction(\"You are a customer support agent for Acme Corp.\")");
    println!("    .greeting(\"Hello! Welcome to Acme support. How can I help?\")");
    println!("    .tools(dispatcher)");
    println!("    .transcription(true, true)");
    println!("    .thinking(1024)");
    println!("    .include_thoughts()");
    println!("    // Phases");
    println!("    .phase(\"greeting\")");
    println!("        .instruction(\"Welcome the caller\")");
    println!("        .transition(\"identify\", S::is_true(\"greeted\"))");
    println!("        .prompt_on_enter(true)");
    println!("        .done()");
    println!("    .phase(\"identify\")");
    println!("        .instruction(\"Verify identity\")");
    println!("        .needs(&[\"customer_id\", \"verified\"])");
    println!("        .transition(\"resolve\", S::is_true(\"verified\"))");
    println!("        .done()");
    println!("    .phase(\"resolve\")");
    println!("        .instruction(\"Resolve the issue\")");
    println!("        .transition(\"farewell\", S::is_true(\"resolved\"))");
    println!("        .done()");
    println!("    .phase(\"farewell\")");
    println!("        .instruction(\"Thank and close\")");
    println!("        .terminal()");
    println!("        .done()");
    println!("    .initial_phase(\"greeting\")");
    println!("    // Control plane");
    println!("    .steering_mode(SteeringMode::ContextInjection)");
    println!("    .context_delivery(ContextDelivery::Deferred)");
    println!("    .repair(RepairConfig::default())");
    println!("    .soft_turn_timeout(Duration::from_secs(2))");
    println!("    .tool_advisory(true)");
    println!("    // Extraction");
    println!("    .extract_turns::<ConversationState>(llm, \"Extract state\")");
    println!("    .on_extracted(|name, value| async {{ println!(\"Extracted: {{}}\", name); }})");
    println!("    // Callbacks");
    println!("    .on_audio(|data| playback_tx.send(data).ok())");
    println!("    .on_text(|t| print!(\"{{t}}\"))");
    println!("    .on_thought(|t| println!(\"[Thought] {{t}}\"))");
    println!("    .on_interrupted(|| async {{ playback.flush().await }})");
    println!("    .on_turn_complete(|| async {{ println!(\"Turn done\") }})");
    println!("    // Persistence");
    println!("    .persistence(Arc::new(FsPersistence::new(\"/tmp/sessions\")))");
    println!("    .session_id(\"session-abc\")");
    println!("    // Connect");
    println!("    .connect_vertex(project, location, token)");
    println!("    .await?;");
    println!();
    println!("// Use the handle:");
    println!("handle.send_audio(pcm_bytes).await?;");
    println!("handle.send_text(\"Hello\").await?;");
    println!("let extraction: Option<T> = handle.extracted(\"ConversationState\");");
    println!("handle.disconnect().await?;");

    println!("\nLive voice session example completed successfully!");
}
