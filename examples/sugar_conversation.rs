//! Minimal conversation using the GeminiAgent builder — 10 lines of application code.
//!
//! Compare with `simple_conversation.rs` (71 lines) to see the boilerplate reduction.

use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("Set GEMINI_API_KEY environment variable");

    let agent = GeminiAgent::builder()
        .api_key(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Aoede)
        .system_instruction("You are a friendly voice assistant. Keep responses concise.")
        .input_transcription()
        .output_transcription()
        .on_text(|t| async move { print!("{t}") })
        .on_text_complete(|t| async move { println!("\n[Complete]: {t}") })
        .on_input_transcription(|t| async move { println!("[You said]: {t}") })
        .on_output_transcription(|t| async move { println!("[Bot said]: {t}") })
        .on_turn_complete(|turn| async move {
            println!("--- (turn took {:?})", turn.duration())
        })
        .on_error(|e| async move { eprintln!("[Error]: {e}") })
        .build()
        .await?;

    println!("Connected! Type messages or press Ctrl+C to quit.\n");

    // Simple text input loop
    let session = agent.session().clone();
    tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            if text == "/quit" {
                let _ = session.disconnect().await;
                break;
            }
            let _ = session.send_text(text).await;
        }
    });

    agent.wait_until_done().await;
    Ok(())
}
