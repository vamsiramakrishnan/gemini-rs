use crate::manifest;
use std::io::{self, BufRead, Write};
use std::path::Path;

/// Run an interactive terminal REPL for the agent.
pub async fn run(
    agent_dir: &str,
    save_session: Option<&str>,
    session_id: Option<&str>,
    replay: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let dir = Path::new(agent_dir);
    let manifest_path = dir.join("agent.toml");
    let agent = manifest::load_manifest(&manifest_path)?;

    println!("Agent: {} (model: {})", agent.name, agent.model);
    if !agent.instruction.is_empty() {
        println!("Instruction: {}", agent.instruction);
    }
    if let Some(sid) = session_id {
        println!("Session ID: {}", sid);
    }
    println!("---");

    // Transcript accumulator for --save_session
    let mut transcript: Vec<serde_json::Value> = Vec::new();

    if let Some(replay_path) = replay {
        // Replay mode: read lines from file instead of stdin
        let content = std::fs::read_to_string(replay_path)?;
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content)?;
        for msg in &messages {
            if let Some(user_text) = msg.get("user").and_then(|v| v.as_str()) {
                println!("> {}", user_text);
                let response = run_agent_turn(&agent, user_text).await;
                println!("{}", response);
                transcript.push(serde_json::json!({
                    "user": user_text,
                    "agent": response,
                }));
            }
        }
    } else {
        // Interactive REPL
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        loop {
            print!("> ");
            io::stdout().flush()?;
            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                // EOF
                break;
            }
            let input = line.trim();
            if input.is_empty() {
                continue;
            }
            if input == "/quit" || input == "/exit" {
                break;
            }

            let response = run_agent_turn(&agent, input).await;
            println!("{}", response);
            transcript.push(serde_json::json!({
                "user": input,
                "agent": response,
            }));
        }
    }

    // Save session if requested
    if let Some(path) = save_session {
        let json = serde_json::to_string_pretty(&transcript)?;
        std::fs::write(path, json)?;
        println!("\nSession saved to {}", path);
    }

    Ok(())
}

/// Execute a single conversational turn.
///
/// In a full implementation this would invoke the LLM via the agent runtime.
async fn run_agent_turn(agent: &manifest::AgentManifest, _input: &str) -> String {
    // TODO: Wire up actual LLM call through rs-adk / adk-rs-fluent.
    // For now, return a placeholder to keep the REPL functional.
    format!(
        "[{}] (placeholder response — LLM integration pending)",
        agent.name
    )
}
