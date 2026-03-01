//! Minimal voice chat — connect, send text, listen for responses.

use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize telemetry
    TelemetryConfig {
        logging_enabled: true,
        log_filter: "info".to_string(),
        json_logs: false,
        ..Default::default()
    }
    .init()?;

    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("Set GEMINI_API_KEY environment variable");

    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Aoede)
        .system_instruction("You are a friendly voice assistant. Keep responses concise.")
        .enable_input_transcription()
        .enable_output_transcription();

    let session = connect(config, TransportConfig::default()).await?;
    session.wait_for_phase(SessionPhase::Active).await;
    println!("Connected! Type messages or press Ctrl+C to quit.\n");

    // Event listener task
    let mut events = session.subscribe();
    let event_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::TextDelta(t) => print!("{t}"),
                SessionEvent::TextComplete(t) => println!("\n[Complete]: {t}"),
                SessionEvent::InputTranscription(t) => println!("[You said]: {t}"),
                SessionEvent::OutputTranscription(t) => println!("[Bot said]: {t}"),
                SessionEvent::TurnComplete => println!("\n---"),
                SessionEvent::Interrupted => println!("[Interrupted]"),
                SessionEvent::Disconnected(r) => {
                    println!("[Disconnected: {r:?}]");
                    break;
                }
                SessionEvent::Error(e) => eprintln!("[Error]: {e}"),
                _ => {}
            }
        }
    });

    // Text input loop
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
