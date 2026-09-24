use gemini_adk_fluent_rs::prelude::*;
use std::io::Write;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("assistant")
        .instruction("Be brief.")
        .build(GeminiLlm::from_env()?)?;

    let mut chat = agent.chat();
    for message in ["Hi, I'm Ada.", "What's my name?"] {
        println!("> {message}");
        let mut reply = chat.send_stream(message);
        while let Some(event) = reply.next().await {
            if let RunEvent::TextDelta(text) = event? {
                print!("{text}");
                std::io::stdout().flush().ok();
            }
        }
        println!();
    }
    Ok(())
}
