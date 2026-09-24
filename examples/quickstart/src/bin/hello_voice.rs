use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    Live::builder()
        .instruction("You are a concise concierge.")
        .greeting("Ask how you can help.")
        .connect_from_env()
        .await?
        .talk()
        .await?;

    Ok(())
}
