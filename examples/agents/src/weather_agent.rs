//! Weather agent example — self-contained CLI demo.
//!
//! Connects to Gemini Live in text-only mode with two `#[tool]` functions,
//! asks about the weather, lets the runtime dispatch the tool calls the
//! model makes, and prints the model's final answer.
//!
//! Usage:
//!   cargo run -p example-agents --bin weather-agent

use gemini_adk_fluent_rs::prelude::*;
use serde_json::{Value, json};
use tokio::sync::mpsc;

#[tool("Get current weather for a city")]
async fn get_weather(city: String) -> Result<Value, ToolError> {
    Ok(json!({
        "city": city,
        "temperature_celsius": 22,
        "condition": "Partly cloudy",
        "humidity": 65
    }))
}

#[tool("Get 3-day weather forecast for a city")]
async fn get_forecast(city: String) -> Result<Value, ToolError> {
    Ok(json!({
        "city": city,
        "forecast": [
            {"day": "Today", "high": 22, "low": 15, "condition": "Partly cloudy"},
            {"day": "Tomorrow", "high": 25, "low": 17, "condition": "Sunny"},
            {"day": "Day after", "high": 20, "low": 14, "condition": "Rain"}
        ]
    }))
}

/// What the session reports back to `main` from its callbacks.
enum Event {
    Text(String),
    TurnComplete,
    Error(String),
    Disconnected(Option<String>),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Weather Agent CLI Demo ===\n");

    // GEMINI_API_KEY (Google AI) or the Vertex AI env vars; see
    // `Live::connect_from_env`.
    let _ = dotenvy::dotenv();

    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let (tx_text, tx_turn, tx_error, tx_disconnected) =
        (tx.clone(), tx.clone(), tx.clone(), tx.clone());

    println!("Connecting to Gemini Live...");
    let session = Live::builder()
        .text_only()
        .instruction(
            "You are a weather assistant. Use the get_weather and get_forecast tools \
             to answer questions about weather. Always use tools rather than guessing.",
        )
        .tool(get_weather())
        .tool(get_forecast())
        // Tool calls are dispatched by the runtime; these hooks only narrate.
        .on_tool_call(|calls, _state| {
            println!("[Tool calls received: {}]", calls.len());
            for call in &calls {
                println!("  Calling {}({})", call.name, call.args);
            }
            async { None }
        })
        .before_tool_response(|responses, _state| {
            for response in &responses {
                println!("  Result: {}", response.response);
            }
            println!();
            async move { responses }
        })
        .on_text(move |text| {
            let _ = tx_text.send(Event::Text(text.into()));
        })
        .on_turn_complete(move || {
            let _ = tx_turn.send(Event::TurnComplete);
            async {}
        })
        .on_error(move |message| {
            let _ = tx_error.send(Event::Error(message));
            async {}
        })
        .on_disconnected(move |reason| {
            let _ = tx_disconnected.send(Event::Disconnected(reason));
            async {}
        })
        // No `.model(..)`: connect picks the platform's current Live model
        // (override with GEMINI_LIVE_MODEL).
        .connect_from_env()
        .await?;
    println!("Connected!\n");

    let question = "What's the weather like in San Francisco and Tokyo?";
    println!("User: {question}\n");
    session.send_text(question).await?;

    // Print the streamed answer until the turn completes.
    while let Some(event) = rx.recv().await {
        match event {
            Event::Text(text) => print!("{text}"),
            Event::TurnComplete => {
                println!("\n\n[Turn complete]");
                break;
            }
            Event::Error(e) => {
                eprintln!("\nError: {e}");
                break;
            }
            Event::Disconnected(reason) => {
                eprintln!("\nSession closed: {}", reason.unwrap_or_default());
                break;
            }
        }
    }

    session.disconnect().await?;
    println!("\nDone.");
    Ok(())
}
