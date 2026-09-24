//! A Gemini agent, scaffolded by `adk create`.
//!
//! `cargo run` starts a conversation in the terminal. The API key comes from
//! `GEMINI_API_KEY`, in the environment or in `.env`.
//!
//! This file is also compiled in the gemini-rs repository as an example of
//! the CLI crate, so every project `adk create` writes is known to build.

use std::io::{self, Write};

use gemini_adk_fluent_rs::prelude::*;

/// The agent's name, as given to `adk create`.
const AGENT: &str = "my-agent";

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    dotenvy::dotenv().ok();

    let agent = AgentBuilder::new(AGENT)
        .instruction("You are a helpful assistant. Be concise and informative.")
        .google_search()
        .build(GeminiLlm::from_env()?)?;

    let mut chat = agent.chat();
    println!("{AGENT} is ready. Type /quit to exit.\n");

    loop {
        print!("> ");
        io::stdout().flush().ok();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "/quit" | "/exit") {
            break;
        }

        let mut reply = chat.send_stream(line);
        while let Some(event) = reply.next().await {
            match event {
                Ok(RunEvent::TextDelta(text)) => {
                    print!("{text}");
                    io::stdout().flush().ok();
                }
                Ok(_) => {}
                Err(e) => eprintln!("\nerror: {e}"),
            }
        }
        println!("\n");
    }

    Ok(())
}
