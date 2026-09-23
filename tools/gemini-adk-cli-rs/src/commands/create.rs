use std::fs;
use std::path::Path;

pub fn run(
    name: &str,
    model: &str,
    api_key: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    create_in(Path::new("."), name, model, api_key)
}

/// Write the project `name` under `parent`.
fn create_in(
    parent: &Path,
    name: &str,
    model: &str,
    api_key: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !is_package_name(name) {
        return Err(format!(
            "'{name}' is not a valid package name: use letters, digits, '-' and '_', \
             starting with a letter"
        )
        .into());
    }
    let dir = parent.join(name);
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
    fs::write(dir.join("Cargo.toml"), cargo_toml(name))?;

    // ── src/main.rs — the template compiled in this repository ───────
    fs::write(dir.join("src/main.rs"), main_rs(name))?;

    // ── .env ─────────────────────────────────────────────────────────
    let key_line = api_key.unwrap_or("your-api-key-here");
    fs::write(dir.join(".env"), format!("GEMINI_API_KEY={key_line}\n"))?;

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
        println!("    echo 'GEMINI_API_KEY=...' > .env         # add your API key");
    }
    println!("    adk run .                              # interactive REPL");
    println!("    adk web .                              # full devtools UI");
    println!("    adk eval . tests.evalset.json          # run evaluations");
    println!("    cargo run                              # run your custom main.rs");
    println!();

    Ok(())
}

/// The source of the scaffolded `src/main.rs`, compiled in this repository as
/// the CLI crate's `scaffold-agent` example.
const MAIN_TEMPLATE: &str = include_str!("../../templates/agent/main.rs");

/// The line of the template that names the agent.
const NAME_LINE: &str = "const AGENT: &str = \"my-agent\";";

/// `src/main.rs` for an agent named `name`.
fn main_rs(name: &str) -> String {
    assert!(
        MAIN_TEMPLATE.contains(NAME_LINE),
        "the template names its agent"
    );
    MAIN_TEMPLATE.replace(NAME_LINE, &format!("const AGENT: &str = {name:?};"))
}

/// `Cargo.toml` depending on the release of the SDK this CLI was built from.
fn cargo_toml(name: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
gemini-adk-fluent-rs = "{version}"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
dotenvy = "0.15"
"#
    )
}

/// Whether `name` is usable as a Cargo package name and directory.
fn is_package_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_rs_is_the_compiled_template_with_the_name() {
        let main = main_rs("weather-bot");
        assert!(main.contains(r#"const AGENT: &str = "weather-bot";"#));
        assert_eq!(
            main.replace(r#""weather-bot""#, r#""my-agent""#),
            MAIN_TEMPLATE,
            "only the name changes"
        );
    }

    /// The scaffold used to pin 0.5 and depend on a `gemini-live` crate that
    /// does not exist.
    #[test]
    fn cargo_toml_depends_on_this_release() {
        let manifest = cargo_toml("weather-bot");
        assert!(manifest.contains(&format!(
            "gemini-adk-fluent-rs = \"{}\"",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(!manifest.contains("gemini-live"));
        // Everything the template uses is declared.
        assert!(MAIN_TEMPLATE.contains("#[tokio::main]") && manifest.contains("tokio"));
        assert!(MAIN_TEMPLATE.contains("dotenvy::") && manifest.contains("dotenvy"));
    }

    #[test]
    fn package_names_are_validated() {
        assert!(is_package_name("weather-bot") && is_package_name("bot_2"));
        assert!(!is_package_name("2bot") && !is_package_name("my bot"));
        assert!(!is_package_name("../escape") && !is_package_name(""));
    }

    #[test]
    fn create_writes_a_project() {
        let root = std::env::temp_dir().join(format!("adk-create-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        create_in(&root, "demo-agent", "gemini-flash-latest", None).unwrap();
        assert!(
            create_in(&root, "demo-agent", "gemini-flash-latest", None).is_err(),
            "an existing project is never overwritten"
        );

        let project = root.join("demo-agent");
        let main = std::fs::read_to_string(project.join("src/main.rs")).unwrap();
        assert_eq!(main, main_rs("demo-agent"));
        let toml = std::fs::read_to_string(project.join("agent.toml")).unwrap();
        assert!(toml.contains(r#"model = "gemini-flash-latest""#));
        assert!(
            std::fs::read_to_string(project.join(".env"))
                .unwrap()
                .starts_with("GEMINI_API_KEY=")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
