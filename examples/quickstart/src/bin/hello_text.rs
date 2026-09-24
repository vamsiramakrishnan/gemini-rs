use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("assistant")
        .instruction("Answer in one sentence.")
        .build(GeminiLlm::from_env()?)?;

    println!("{}", agent.ask("What is the Gemini Live API?").await?);
    Ok(())
}
