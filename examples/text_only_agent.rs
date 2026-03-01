//! Text-mode agent for testing — no audio, just text in/out.

use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    TelemetryConfig::default().init()?;

    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("Set GEMINI_API_KEY environment variable");

    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .text_only()
        .system_instruction("You are a helpful assistant. Respond concisely.");

    let session = connect(config, TransportConfig::default()).await?;
    session.wait_for_phase(SessionPhase::Active).await;
    println!("Text-only session active. Type your messages:\n");

    let mut events = session.subscribe();
    let event_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::TextDelta(t) => print!("{t}"),
                SessionEvent::TurnComplete => println!("\n"),
                SessionEvent::Disconnected(reason) => {
                    println!("[Disconnected: {reason:?}]");
                    break;
                }
                SessionEvent::Error(e) => eprintln!("[Error: {e}]"),
                _ => {}
            }
        }
    });

    let mut line = String::new();
    loop {
        line.clear();
        if std::io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if text == "/quit" {
            break;
        }
        session.send_text(text).await?;
    }

    session.disconnect().await?;
    event_task.abort();
    Ok(())
}
