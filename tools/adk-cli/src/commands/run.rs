use crate::manifest;
use rs_adk::{BaseLlm, GeminiLlm, GeminiLlmParams};
use adk_rs_fluent::prelude::*;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

pub async fn run(
    agent_dir: &str,
    save_session: Option<&str>,
    _session_id: Option<&str>,
    replay: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let dir = std::path::Path::new(agent_dir);
    let m = manifest::load_manifest(&dir.join("agent.toml"))?;

    println!("\n  Agent: {} — {}", m.name, m.description);
    println!("  Model: {}\n", m.model);

    // ── Create LLM (auto-detects auth from env) ──────────────────────
    let params = GeminiLlmParams {
        model: Some(m.model.clone()),
        ..Default::default()
    };
    let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(params));

    // ── Build agent from manifest ────────────────────────────────────
    let mut builder = AgentBuilder::new(&m.name).instruction(&m.instruction);

    for tool in &m.tools {
        builder = match tool.as_str() {
            "google_search" => builder.google_search(),
            "code_execution" => builder.code_execution(),
            other => {
                eprintln!("  Warning: unknown built-in tool '{}' — skipping", other);
                builder
            }
        };
    }

    let agent = builder.build(llm);
    let state = State::new();
    let mut transcript: Vec<serde_json::Value> = Vec::new();

    // ── Replay mode ──────────────────────────────────────────────────
    if let Some(replay_path) = replay {
        let data = std::fs::read_to_string(replay_path)?;
        let turns: Vec<serde_json::Value> = serde_json::from_str(&data)?;
        for turn in &turns {
            if let Some(user_input) = turn["user"].as_str() {
                println!("> {}", user_input);
                state.set("input", user_input);
                match agent.run(&state).await {
                    Ok(output) => println!("\n{}\n", output),
                    Err(e) => eprintln!("\nError: {}\n", e),
                }
            }
        }
        return Ok(());
    }

    // ── Interactive REPL ─────────────────────────────────────────────
    let stdin = io::stdin();
    println!("  Type /quit to exit.\n");

    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        stdin.lock().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        match input {
            "/quit" | "/exit" => break,
            _ => {}
        }

        state.set("input", input);
        match agent.run(&state).await {
            Ok(output) => {
                println!("\n{}\n", output);
                transcript.push(serde_json::json!({
                    "user": input,
                    "agent": output,
                }));
            }
            Err(e) => eprintln!("\nError: {}\n", e),
        }
    }

    // ── Save session ─────────────────────────────────────────────────
    if let Some(path) = save_session {
        let json = serde_json::to_string_pretty(&transcript)?;
        std::fs::write(path, json)?;
        println!("  Session saved to {}", path);
    }

    Ok(())
}
