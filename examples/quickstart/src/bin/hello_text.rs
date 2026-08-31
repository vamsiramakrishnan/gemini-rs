use gemini_adk_fluent_rs::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Reads GEMINI_API_KEY (Google AI) or the Vertex AI env vars.
    let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));

    let agent = AgentBuilder::new("assistant")
        .instruction("You are a concise assistant.")
        .build(llm);

    let state = State::new();
    state.set("input", "In one sentence: what is the Gemini Live API?")?;
    println!("{}", agent.run(&state).await?);
    Ok(())
}
