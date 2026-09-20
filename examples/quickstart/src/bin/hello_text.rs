use gemini_adk_fluent_rs::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));

    let agent = AgentBuilder::new("assistant")
        .instruction("Answer in one sentence.")
        .build(llm)?;

    let state = State::new();
    state.set("input", "What is the Gemini Live API?")?;

    println!("{}", agent.run(&state).await?);
    Ok(())
}
