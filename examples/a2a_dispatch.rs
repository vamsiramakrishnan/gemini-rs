//! A2A multi-agent collaboration — tool calls delegate to remote specialist agents.

use gemini_live_rs::prelude::*;
use gemini_live_rs::agent::{A2AClient, A2AConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    TelemetryConfig::default().init()?;

    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("Set GEMINI_API_KEY environment variable");

    // Discover remote agents
    let mut weather_agent = A2AClient::new(A2AConfig {
        base_url: std::env::var("WEATHER_AGENT_URL")
            .unwrap_or_else(|_| "http://localhost:9001".to_string()),
        ..Default::default()
    });

    match weather_agent.discover().await {
        Ok(card) => println!("Discovered: {} - {}", card.name, card.description),
        Err(e) => eprintln!("Weather agent discovery failed: {e}"),
    }

    // Build function registry that delegates to remote agents
    let mut registry = FunctionRegistry::new();

    registry.register(
        "check_weather",
        "Check weather at a location via specialist weather agent",
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "City or location name" }
            },
            "required": ["location"]
        })),
        move |args| {
            let location = args["location"].as_str().unwrap_or("unknown").to_string();
            async move {
                // In production: use weather_agent.send_task() here
                Ok(serde_json::json!({
                    "location": location,
                    "temperature": 22,
                    "condition": "sunny",
                    "source": "weather-agent-a2a"
                }))
            }
        },
    );

    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Kore)
        .system_instruction("You are a personal assistant that can check weather.")
        .add_tool(registry.to_tool_declaration());

    let session = connect(config, TransportConfig::default()).await?;
    session.wait_for_phase(SessionPhase::Active).await;
    println!("A2A dispatch agent ready!\n");

    let mut events = session.subscribe();
    let cmd_tx = session.command_tx.clone();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::ToolCall(calls) => {
                    let responses = registry.execute_all(&calls).await;
                    let _ = cmd_tx.send(SessionCommand::SendToolResponse(responses)).await;
                }
                SessionEvent::TextDelta(t) => print!("{t}"),
                SessionEvent::TurnComplete => println!("\n---"),
                SessionEvent::Disconnected(_) => break,
                _ => {}
            }
        }
    });

    session.send_text("What's the weather like in Tokyo?").await?;

    tokio::signal::ctrl_c().await?;
    session.disconnect().await?;
    Ok(())
}
