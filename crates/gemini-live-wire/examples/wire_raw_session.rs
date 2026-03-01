//! Wire-level example — raw session with gemini-live-wire.
//!
//! Demonstrates the lowest-level API: connect to Gemini, send text,
//! and print responses. No agent abstraction, no tools — pure protocol.
//!
//! The native audio model (gemini-live-2.5-flash-native-audio) only
//! supports AUDIO output modality. For text-only responses, use
//! gemini-2.0-flash-live-001.
//!
//! Usage:
//!   GEMINI_API_KEY=your-key cargo run -p gemini-live-wire --example wire_raw_session

use gemini_live_wire::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY").expect("Set GEMINI_API_KEY");

    // Use text-compatible model with TEXT modality
    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .system_instruction("You are a helpful assistant. Be concise.")
        .response_modalities(vec![Modality::Text]);

    println!("Connecting to Gemini Live API...");

    // Connect via transport layer and get a session handle
    let transport = TransportConfig::default();
    let handle = gemini_live_wire::transport::connect(config, transport).await?;

    // Subscribe to events
    let mut events = handle.subscribe();

    // Send a text message
    handle.send_text("What is the capital of France?").await?;

    // Listen for events
    loop {
        match events.recv().await {
            Ok(event) => match event {
                SessionEvent::TextDelta(text) => {
                    print!("{text}");
                }
                SessionEvent::TurnComplete => {
                    println!("\n[Turn complete]");
                    break;
                }
                SessionEvent::Error(e) => {
                    eprintln!("Error: {e}");
                    break;
                }
                _ => {} // Audio, interruption, etc.
            },
            Err(e) => {
                eprintln!("Channel error: {e}");
                break;
            }
        }
    }

    // Gracefully disconnect
    let _ = handle.disconnect().await;

    Ok(())
}
