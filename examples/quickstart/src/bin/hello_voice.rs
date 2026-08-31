use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    Live::builder()
        .instruction("You are a helpful concierge.")
        .greeting("Greet the caller and ask how you can help.")
        .connect_from_env() // GEMINI_API_KEY, or the Vertex AI env vars
        .await?
        .talk() // microphone in, speakers out, barge-in handled
        .await?;
    Ok(())
}
