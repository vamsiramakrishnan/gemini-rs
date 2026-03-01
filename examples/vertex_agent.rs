//! Vertex AI agent — demonstrates connecting via Vertex AI instead of Google AI.
//!
//! Set environment variables before running:
//!   GOOGLE_CLOUD_PROJECT=my-project-123
//!   GOOGLE_CLOUD_LOCATION=us-central1
//!   GOOGLE_ACCESS_TOKEN=$(gcloud auth print-access-token)
//!
//! ```sh
//! cargo run --example vertex_agent
//! ```

use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project = std::env::var("GOOGLE_CLOUD_PROJECT")
        .expect("GOOGLE_CLOUD_PROJECT env var required");
    let location = std::env::var("GOOGLE_CLOUD_LOCATION")
        .unwrap_or_else(|_| "us-central1".to_string());
    let access_token = std::env::var("GOOGLE_ACCESS_TOKEN")
        .expect("GOOGLE_ACCESS_TOKEN env var required (run: gcloud auth print-access-token)");

    // ---------- Build agent via Vertex AI ----------

    let agent = GeminiAgent::builder()
        .vertex(&project, &location, &access_token)
        .model(GeminiModel::Custom(
            "gemini-live-2.5-flash-native-audio".to_string(),
        ))
        .voice(Voice::Kore)
        .system_instruction("You are a helpful voice assistant running on Vertex AI.")
        .input_transcription()
        .output_transcription()
        .on_text(|t| async move { print!("{t}") })
        .on_turn_complete(|_| async move { println!("\n--- turn complete ---") })
        .on_input_transcription(|t| async move {
            println!("[user] {t}");
        })
        .on_error(|e| async move {
            eprintln!("ERROR: {e}");
        })
        .build()
        .await?;

    println!(
        "Connected to Vertex AI (project={project}, location={location})"
    );
    println!("Session ID: {}", agent.session().session_id());

    // Send a test message
    agent.send_text("Hello from Vertex AI!").await?;

    // Wait until session ends
    agent.wait_until_done().await;
    Ok(())
}
