use std::fs;
use std::path::Path;

pub fn run(
    name: &str,
    model: &str,
    api_key: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(name);
    if dir.exists() {
        return Err(format!("Directory '{}' already exists", name).into());
    }

    fs::create_dir_all(dir.join("src"))?;

    // ── agent.toml ───────────────────────────────────────────────────
    fs::write(
        dir.join("agent.toml"),
        format!(
            r#"name = "{name}"
description = "A new ADK agent"
model = "{model}"
instruction = "You are a helpful assistant. Be concise and informative."
tools = ["google_search"]
sub_agents = []

# ── Optional settings ────────────────────────────────────────────
# temperature = 0.7          # Sampling temperature (0.0–2.0)
# thinking = 2048            # Enable extended thinking with token budget
# greeting = "Hello! How can I help you today?"  # Model speaks first (Live sessions)
# voice = "Kore"             # Voice for Live sessions: Kore, Puck, Charon, Fenrir, Aoede
# output_modality = "audio"  # Live output: "text", "audio", or "text_and_audio"
"#
        ),
    )?;

    // ── Cargo.toml ───────────────────────────────────────────────────
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
gemini-adk-fluent = {{ version = "0.5", features = ["gemini-llm"] }}
gemini-adk = {{ version = "0.5", features = ["gemini-llm"] }}
gemini-live = "0.5"
tokio = {{ version = "1", features = ["full"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
schemars = "0.8"
dotenvy = "0.15"
"#
        ),
    )?;

    // ── src/main.rs — working agent that compiles and runs ───────────
    fs::write(
        dir.join("src/main.rs"),
        format!(
            r#"//! {name} — built with the Gemini ADK for Rust.
//!
//! Run with: adk run .
//! Or directly: cargo run

use gemini_adk_fluent::prelude::*;
use std::io::{{self, Write}};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {{
    dotenvy::dotenv().ok();

    // Create LLM — auto-detects credentials from environment.
    // Set GOOGLE_GENAI_API_KEY, GEMINI_API_KEY, or GOOGLE_API_KEY.
    let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));

    // Build your agent with instruction and tools.
    let agent = AgentBuilder::new("{name}")
        .instruction("You are a helpful assistant. Be concise and informative.")
        .google_search()
        .temperature(0.7)
        .build(llm);

    // Interactive REPL.
    let state = State::new();
    println!("{name} ready. Type /quit to exit.\n");

    loop {{
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {{ continue; }}
        if input == "/quit" || input == "/exit" {{ break; }}

        state.set("input", input);
        match agent.run(&state).await {{
            Ok(output) => println!("\n{{output}}\n"),
            Err(e) => eprintln!("\nError: {{e}}\n"),
        }}
    }}

    Ok(())
}}
"#
        ),
    )?;

    // ── .env ─────────────────────────────────────────────────────────
    let key_line = api_key.unwrap_or("your-api-key-here");
    fs::write(
        dir.join(".env"),
        format!("GOOGLE_GENAI_API_KEY={key_line}\n"),
    )?;

    // ── .gitignore ───────────────────────────────────────────────────
    fs::write(dir.join(".gitignore"), "/target\n.env\n")?;

    // ── Sample evalset ───────────────────────────────────────────────
    fs::write(
        dir.join("tests.evalset.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "name": format!("{name} evaluation"),
            "cases": [
                {
                    "id": "greeting",
                    "inputs": ["Hello, who are you?"],
                    "expected": ["assistant", "helpful"],
                    "tags": ["basic"]
                },
                {
                    "id": "factual",
                    "inputs": ["What is the capital of France?"],
                    "expected": ["Paris"],
                    "tags": ["knowledge"]
                }
            ]
        }))?,
    )?;

    // ── Success message ──────────────────────────────────────────────
    println!("\n  Created agent project: {name}/\n");
    println!("  Next steps:\n");
    println!("    cd {name}");
    if api_key.is_none() {
        println!("    echo 'GOOGLE_GENAI_API_KEY=...' > .env   # add your API key");
    }
    println!("    adk run .                              # interactive REPL");
    println!("    adk web .                              # full devtools UI");
    println!("    adk eval . tests.evalset.json          # run evaluations");
    println!("    cargo run                              # run your custom main.rs");
    println!();

    Ok(())
}
