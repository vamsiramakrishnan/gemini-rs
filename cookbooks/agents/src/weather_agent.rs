//! Weather agent cookbook — self-contained CLI demo.
//!
//! Connects to Gemini Live, asks about weather, dispatches the tool call,
//! sends back the tool response, and prints the model's final answer.
//!
//! Usage:
//!   cargo run -p agents-cookbook --bin weather-agent

use std::sync::Arc;

use rs_adk::tool::{ToolDispatcher, TypedTool};
use rs_genai::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

/// Type-safe arguments for the weather tool.
#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// City name to get weather for
    city: String,
}

/// Type-safe arguments for the forecast tool.
#[derive(Deserialize, JsonSchema)]
struct ForecastArgs {
    /// City name to get the forecast for
    city: String,
}

fn create_dispatcher() -> ToolDispatcher {
    let mut dispatcher = ToolDispatcher::new().with_timeout(std::time::Duration::from_secs(10));

    dispatcher.register_function(Arc::new(TypedTool::new(
        "get_weather",
        "Get current weather for a city",
        |args: WeatherArgs| async move {
            Ok(serde_json::json!({
                "city": args.city,
                "temperature_celsius": 22,
                "condition": "Partly cloudy",
                "humidity": 65
            }))
        },
    )));

    dispatcher.register_function(Arc::new(TypedTool::new(
        "get_forecast",
        "Get 3-day weather forecast for a city",
        |args: ForecastArgs| async move {
            Ok(serde_json::json!({
                "city": args.city,
                "forecast": [
                    {"day": "Today", "high": 22, "low": 15, "condition": "Partly cloudy"},
                    {"day": "Tomorrow", "high": 25, "low": 17, "condition": "Sunny"},
                    {"day": "Day after", "high": 20, "low": 14, "condition": "Rain"}
                ]
            }))
        },
    )));

    dispatcher
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Weather Agent CLI Demo ===\n");

    let _ = dotenvy::dotenv();

    let use_vertex = std::env::var("GOOGLE_GENAI_USE_VERTEXAI")
        .map(|v| v.to_uppercase() == "TRUE" || v == "1")
        .unwrap_or(false);

    let base_config = if use_vertex {
        let project = std::env::var("GOOGLE_CLOUD_PROJECT")
            .expect("GOOGLE_CLOUD_PROJECT required for Vertex AI");
        let location =
            std::env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".to_string());
        println!(
            "Using Vertex AI (project: {}, location: {})",
            project, location
        );
        let token = String::from_utf8(
            std::process::Command::new("gcloud")
                .args(["auth", "print-access-token"])
                .output()
                .expect("gcloud CLI required for Vertex AI")
                .stdout,
        )?
        .trim()
        .to_string();
        SessionConfig::from_vertex(&project, &location, token)
    } else {
        let api_key =
            std::env::var("GEMINI_API_KEY").expect("Set GEMINI_API_KEY or enable Vertex AI");
        println!("Using Google AI Studio");
        SessionConfig::new(api_key)
    };

    // Set up tools
    let dispatcher = create_dispatcher();
    let tool_declarations = dispatcher.to_tool_declarations();
    println!("Registered {} tools", dispatcher.len());

    // Configure session with text model and tools
    let mut config = base_config
        .model(GeminiModel::Gemini2_0FlashLive)
        .text_only()
        .system_instruction(
            "You are a weather assistant. Use the get_weather and get_forecast tools \
             to answer questions about weather. Always use tools rather than guessing.",
        );

    for tool in tool_declarations {
        config = config.add_tool(tool);
    }

    // Connect
    println!("Connecting to Gemini Live...");
    let session = connect(config, TransportConfig::default()).await?;
    let mut events = session.subscribe();

    // Wait for active
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        session.wait_for_phase(SessionPhase::Active),
    )
    .await
    .map_err(|_| "Timed out waiting for session to become active")?;

    println!("Connected!\n");

    // Send a question
    let question = "What's the weather like in San Francisco and Tokyo?";
    println!("User: {}\n", question);
    session.send_text(question).await?;

    // Process events until turn complete
    let mut full_response = String::new();
    loop {
        match recv_event(&mut events).await {
            Some(SessionEvent::ToolCall(calls)) => {
                println!("[Tool calls received: {}]", calls.len());
                let mut responses = Vec::new();
                for call in &calls {
                    println!("  Calling {}({})", call.name, call.args);
                    let result = dispatcher
                        .call_function(&call.name, call.args.clone())
                        .await;
                    let response = ToolDispatcher::build_response(call, result);
                    println!("  Result: {}", response.response);
                    responses.push(response);
                }
                session.send_tool_response(responses).await?;
                println!();
            }
            Some(SessionEvent::TextDelta(text)) => {
                print!("{}", text);
                full_response.push_str(&text);
            }
            Some(SessionEvent::TurnComplete) => {
                println!("\n\n[Turn complete]");
                break;
            }
            Some(SessionEvent::Error(e)) => {
                eprintln!("\nError: {}", e);
                break;
            }
            None => {
                eprintln!("\nSession closed unexpectedly");
                break;
            }
            _ => {}
        }
    }

    // Clean up
    session.disconnect().await?;
    println!("\nDone.");
    Ok(())
}
