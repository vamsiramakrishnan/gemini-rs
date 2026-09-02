//! Wire-level example — raw session with gemini-live.
//!
//! Demonstrates the lowest-level API: connect to Gemini, send text,
//! and print responses. No agent abstraction, no tools — pure protocol.
//!
//! Uses `connect()` with a default config (the platform's current native-audio
//! model, output transcription on so the answer is readable), `recv_event()`
//! for lag-safe event consumption, and `handle.disconnect()` for clean shutdown.
//!
//! Usage:
//!   GEMINI_API_KEY=your-key cargo run -p gemini-genai-rs --example wire_raw_session

use gemini_genai_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY").expect("Set GEMINI_API_KEY");

    let handle = connect(SessionConfig::new(&api_key).output_transcription(true)).await?;
    let mut events = handle.subscribe();

    handle.send_text("What is the capital of France?").await?;

    while let Some(event) = recv_event(&mut events).await {
        match event {
            SessionEvent::TextDelta(text) | SessionEvent::OutputTranscription(text) => {
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
            _ => {}
        }
    }

    handle.disconnect().await?;
    Ok(())
}
