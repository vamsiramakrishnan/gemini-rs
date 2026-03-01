//! Engineering layers example — context, prompt, and state engineering.
//!
//! Demonstrates how to build a production-grade customer service agent using
//! all three engineering layers inspired by Google ADK:
//!
//! - **Prompt engineering**: Structured system prompt with role, task, constraints,
//!   format, and few-shot examples
//! - **Context engineering**: Compression threshold, sliding memory window,
//!   dynamic context injection, and session resumption
//! - **State engineering**: Event-driven state transforms, guards, and
//!   automatic turn/interruption counting

use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key =
        std::env::var("GEMINI_API_KEY").expect("Set GEMINI_API_KEY environment variable");

    // -----------------------------------------------------------------------
    // 1. Prompt engineering — structured system prompt
    // -----------------------------------------------------------------------
    let prompt = PromptStrategy::customer_service(
        "TechCorp",
        "You handle billing inquiries and technical support for cloud services.",
    )
    .context("TechCorp offers three tiers: Free, Pro ($29/mo), and Enterprise (custom pricing).")
    .constraint("If the customer requests a refund, collect their order ID first.")
    .constraint("For technical issues, ask for error messages before troubleshooting.")
    .example(
        "Customer: I was charged twice this month.",
        "I'm sorry to hear that. Let me look into this for you. Could you share your account email so I can pull up the billing history?",
    )
    .example(
        "Customer: My API calls are failing.",
        "I'd be happy to help troubleshoot. What error message are you seeing, and which API endpoint are you calling?",
    )
    .build();

    println!("=== Rendered System Prompt ===\n{}\n", prompt.render_static());

    // -----------------------------------------------------------------------
    // 2. Context engineering — memory, compression, injection
    // -----------------------------------------------------------------------
    let context = ContextPolicy::builder()
        // Server-side: compress when context exceeds 8000 tokens
        .compression_threshold(8000)
        // Client-side: keep last 20 turns in memory
        .memory(MemoryStrategy::window(20))
        // Inject static context when session connects
        .inject_on_connect("support_hours", "Mon-Fri 9am-6pm EST")
        // Inject dynamic template every 10 turns
        .inject_template_every(10, "Conversation summary: {turn_count} turns so far")
        // Enable session resumption for context preservation
        .enable_resumption()
        // Token budget: 30% system, 20% tools, 50% conversation
        .budget(
            ContextBudget::new(16000)
                .system(0.3)
                .tools(0.2)
                .conversation(0.5),
        )
        .build();

    println!("=== Context Policy ===\n{context:?}\n");

    // -----------------------------------------------------------------------
    // 3. State engineering — transforms, guards, initial values
    // -----------------------------------------------------------------------
    let state = StatePolicy::builder()
        // Initialize counters
        .initial("turn_count", serde_json::json!(0))
        .initial("interruption_count", serde_json::json!(0))
        .initial("tool_call_count", serde_json::json!(0))
        .initial("identity_verified", serde_json::json!(false))
        // Auto-increment on events
        .on_turn_complete(StateTransform::increment("turn_count", 1))
        .on_interrupted(StateTransform::increment("interruption_count", 1))
        .on_tool_call(StateTransform::increment("tool_call_count", 1))
        // Set status on connect/disconnect
        .on_connect(StateTransform::set(
            "status",
            serde_json::json!("active"),
        ))
        .on_disconnect(StateTransform::set(
            "status",
            serde_json::json!("ended"),
        ))
        // Guard: require identity verification before accessing account tools
        .guard_before_tool(
            Some("get_account_details".to_string()),
            StateGuard::new("identity_check", |state| {
                if state.get::<bool>("identity_verified").unwrap_or(false) {
                    Ok(())
                } else {
                    Err("Customer identity must be verified before accessing account details".into())
                }
            }),
        )
        .build();

    println!("=== State Policy ===\n{state:?}\n");

    // -----------------------------------------------------------------------
    // 4. Wire everything into the agent builder
    // -----------------------------------------------------------------------
    let agent = GeminiAgent::builder()
        .api_key(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Kore)
        // Engineering layers
        .prompt(prompt)
        .context_policy(context)
        .state_policy(state)
        // Transcription for analytics
        .input_transcription()
        .output_transcription()
        // Tools
        .tool(
            "get_account_details",
            "Retrieve customer account details by email",
            Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "email": {"type": "string", "description": "Customer email address"}
                },
                "required": ["email"]
            })),
            |args| async move {
                let email = args["email"].as_str().unwrap_or("unknown");
                Ok(serde_json::json!({
                    "name": "Jane Doe",
                    "email": email,
                    "plan": "Pro",
                    "balance": 29.00,
                    "next_billing": "2026-04-01"
                }))
            },
        )
        .tool(
            "verify_identity",
            "Verify customer identity by account email and last 4 digits of phone",
            Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "email": {"type": "string"},
                    "phone_last4": {"type": "string"}
                },
                "required": ["email", "phone_last4"]
            })),
            |_args| async move {
                // In production, verify against database
                Ok(serde_json::json!({"verified": true}))
            },
        )
        // Callbacks
        .on_text(|t| async move { print!("{t}") })
        .on_text_complete(|_| async move { println!() })
        .on_turn_complete(|turn| async move {
            println!("--- Turn {} complete ({:?}) ---", turn.id, turn.duration());
        })
        .on_input_transcription(|t| async move {
            println!("[Customer]: {t}");
        })
        .on_interrupted(|_| async move {
            println!("[Interrupted by customer]");
        })
        .on_error(|e| async move {
            eprintln!("[Error]: {e}");
        })
        .build()
        .await?;

    println!("\nAgent ready. Session: {}\n", agent.session_id());

    // Start the conversation
    agent
        .send_text("Hello! A customer just called about a billing issue.")
        .await?;

    // Wait for the session to end
    agent.wait_until_done().await;

    Ok(())
}
