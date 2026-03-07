//! Wire-level example — raw session with rs-genai.
//!
//! Demonstrates the lowest-level API: connect to Gemini, send text,
//! and print responses. No agent abstraction, no tools — pure protocol.
//!
//! Uses `quick_connect()` for a minimal hello-world, `recv_event()` for
//! lag-safe event consumption, and `handle.join()` for clean shutdown.
//!
//! Usage:
//!   GEMINI_API_KEY=your-key cargo run -p rs-genai --example wire_raw_session

use rs_genai::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY").expect("Set GEMINI_API_KEY");

    let handle = quick_connect(&api_key, "gemini-2.0-flash-live-001").await?;
    let mut events = handle.subscribe();

    handle.send_text("What is the capital of France?").await?;

    while let Some(event) = recv_event(&mut events).await {
        match event {
            SessionEvent::TextDelta(text) => print!("{text}"),
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
