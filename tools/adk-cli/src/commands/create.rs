use std::fs;
use std::path::Path;

/// Scaffold a new agent project directory.
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

    // agent.toml
    let agent_toml = format!(
        r#"name = "{name}"
description = "A new ADK agent"
model = "{model}"
instruction = "You are a helpful assistant."
tools = []
sub_agents = []
"#
    );
    fs::write(dir.join("agent.toml"), agent_toml)?;

    // Cargo.toml
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
adk-rs-fluent = {{ git = "https://github.com/vamsiramakrishnan/gemini-rs", features = ["gemini-llm"] }}
tokio = {{ version = "1", features = ["full"] }}
serde_json = "1"
dotenvy = "0.15"
"#
    );
    fs::write(dir.join("Cargo.toml"), cargo_toml)?;

    // src/main.rs
    let main_rs = r#"use adk_rs_fluent::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    // TODO: Load agent.toml and run the agent
    println!("Agent ready. Implement your logic here.");
    Ok(())
}
"#;
    fs::write(dir.join("src/main.rs"), main_rs)?;

    // .env
    let env_content = format!(
        "GOOGLE_API_KEY={}\n",
        api_key.unwrap_or("YOUR_API_KEY_HERE")
    );
    fs::write(dir.join(".env"), env_content)?;

    // .gitignore
    fs::write(dir.join(".gitignore"), "/target\n.env\n")?;

    println!("Created agent project '{name}' with model '{model}'");
    println!("  {}/agent.toml", name);
    println!("  {}/Cargo.toml", name);
    println!("  {}/src/main.rs", name);
    println!("  {}/.env", name);
    println!("\nNext steps:");
    println!("  1. Set your API key in {}/.env", name);
    println!("  2. Edit {}/agent.toml to configure your agent", name);
    println!("  3. Run: adk run {}", name);

    Ok(())
}
