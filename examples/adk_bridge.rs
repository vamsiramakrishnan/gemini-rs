//! ADK integration example — tool calls dispatched to a Google ADK agent backend.

use gemini_live_rs::prelude::*;
use gemini_live_rs::agent::{AdkBridge, AdkConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    TelemetryConfig::default().init()?;

    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("Set GEMINI_API_KEY environment variable");

    let adk_endpoint = std::env::var("ADK_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());

    let adk = AdkBridge::new(AdkConfig {
        endpoint: adk_endpoint,
        agent_name: "financial_advisor".to_string(),
        session_id: Some(uuid::Uuid::new_v4().to_string()),
        timeout_secs: 30,
    });

    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Kore)
        .system_instruction("You are a financial advisor. Use tools to help users.");

    let session = connect(config, TransportConfig::default()).await?;
    session.wait_for_phase(SessionPhase::Active).await;
    println!("ADK bridge active!\n");

    // Attach ADK bridge — automatically dispatches all tool calls
    let bridge_handle = adk.attach(&session);

    // Event display
    let mut events = session.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::TextDelta(t) => print!("{t}"),
                SessionEvent::TurnComplete => println!("\n---"),
                SessionEvent::Disconnected(_) => break,
                _ => {}
            }
        }
    });

    session.send_text("What investment strategies do you recommend for retirement?").await?;

    tokio::signal::ctrl_c().await?;
    session.disconnect().await?;
    bridge_handle.abort();
    Ok(())
}
