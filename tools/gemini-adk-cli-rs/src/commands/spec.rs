//! `adk spec` — work with a session spec (`agent.json`): generate a project
//! around it, run its tests, call one of its tools, or run it live.
//!
//! A spec is the whole agent as data: model, instruction, conversation, tool
//! declarations and tests. These commands are how a spec authored in Flow
//! Studio becomes a project, and how a Python or Go project's tools are
//! exercised through the same bindings the runtime uses.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::spec::{
    ProjectLanguage, ProjectOptions, SdkSource, SessionSpec, SpecModality, SpecResources,
};
use serde_json::Value;

type CliResult = Result<(), Box<dyn std::error::Error>>;

fn load(path: &str) -> Result<SessionSpec, Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| format!("{path}: {e}"))?;
    Ok(SessionSpec::from_value(value)?)
}

/// `adk spec codegen <spec> --lang <rust|python|go> --out <dir>` — write a
/// project that runs the spec, with one typed stub per mock tool.
pub fn codegen(
    spec_path: &str,
    lang: &str,
    out: &str,
    sdk_path: Option<&str>,
    force: bool,
) -> CliResult {
    let spec = load(spec_path)?;
    let language: ProjectLanguage = lang.parse()?;
    let sdk = match sdk_path {
        Some(path) => SdkSource::Path(
            fs::canonicalize(path)
                .map_err(|e| format!("--sdk-path {path}: {e}"))?
                .to_string_lossy()
                .into_owned(),
        ),
        None => SdkSource::Registry,
    };
    let files = spec.to_project_with(language, &ProjectOptions { sdk });
    let root = Path::new(out);
    if !force {
        // Stubs are where implementations go: never overwrite them silently.
        let existing: Vec<String> = files
            .iter()
            .map(|f| root.join(&f.path))
            .filter(|p| p.exists())
            .map(|p| p.display().to_string())
            .collect();
        if !existing.is_empty() {
            return Err(format!(
                "would overwrite {}; pass --force to replace them",
                existing.join(", ")
            )
            .into());
        }
    }
    for file in &files {
        let path = root.join(&file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, &file.contents)?;
        println!("  wrote {}", path.display());
    }
    println!("\nNext: see {}", root.join("README.md").display());
    Ok(())
}

/// `adk spec test <spec>` — validate the spec and run its embedded tests and
/// conversation scenarios offline. Exits non-zero on any failure.
pub async fn test(spec_path: &str) -> CliResult {
    let spec = load(spec_path)?;
    let validation = spec.validate();
    for warning in &validation.warnings {
        println!("warning: {warning}");
    }
    if !validation.valid {
        for error in &validation.errors {
            eprintln!("error: {error}");
        }
        return Err(format!("{spec_path} is invalid").into());
    }
    let mut passed = 0;
    let mut failed = 0;
    for report in spec.run_tests() {
        if report.passed {
            passed += 1;
            println!("ok    test {}", report.name);
        } else {
            failed += 1;
            println!("FAIL  test {}", report.name);
            for step in &report.failures {
                for failure in &step.failures {
                    println!("        event {} ({}): {failure}", step.index, step.event);
                }
            }
        }
    }
    for report in spec.run_scenarios().await {
        if report.passed {
            passed += 1;
            println!("ok    scenario {}", report.name);
        } else {
            failed += 1;
            println!("FAIL  scenario {}", report.name);
            if let Some(error) = &report.error {
                println!("        {error}");
            }
        }
    }
    println!("\n{passed} passed, {failed} failed");
    if failed > 0 {
        return Err(format!("{failed} failed").into());
    }
    Ok(())
}

/// `adk spec call <spec> <tool> [args]` — call one tool through its binding
/// (in-process mock, HTTP, or MCP server), as a session would, and print the
/// result and the state it wrote.
pub async fn call(spec_path: &str, tool: &str, args: Option<&str>) -> CliResult {
    dotenvy::dotenv().ok();
    let spec = load(spec_path)?;
    if !spec.tools.iter().any(|t| t.name == tool) {
        let declared: Vec<&str> = spec.tools.iter().map(|t| t.name.as_str()).collect();
        return Err(format!("{spec_path} declares no tool '{tool}' (it has: {declared:?})").into());
    }
    let args: Value = match args {
        Some(raw) => serde_json::from_str(raw).map_err(|e| format!("arguments: {e}"))?,
        None => serde_json::json!({}),
    };
    let state = State::new();
    let result = spec
        .build_dispatcher(&state)
        .call_function(tool, args)
        .await
        .map_err(|e| format!("{tool}: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    let mut keys = state.keys();
    keys.sort();
    if !keys.is_empty() {
        println!("\nstate:");
        for key in keys {
            let value: Value = state.get(&key).unwrap_or(Value::Null);
            println!("  {key} = {value}");
        }
    }
    Ok(())
}

/// `adk spec run <spec>` — run the spec as a live session. Text specs get a
/// terminal REPL; audio specs use the microphone and speakers when `adk` is
/// built with the `voice` feature.
pub async fn run(spec_path: &str) -> CliResult {
    dotenvy::dotenv().ok();
    let spec = load(spec_path)?;
    if spec.memory.is_some() {
        return Err(
            "this spec uses `memory`, which needs a memory engine: generate a Rust project \
             (`adk spec codegen --lang rust`) and run that"
                .into(),
        );
    }
    let mut resources = SpecResources::default();
    if !spec.extract.is_empty() {
        resources.extraction_llm = Some(Arc::new(GeminiLlm::from_env()?));
    }
    let state = State::new();
    let live = spec.apply(Live::builder(), &state, &resources)?;
    match spec.modality {
        SpecModality::Text => {
            let session = live
                .on_text(|t| print!("{t}"))
                .on_turn_complete(|| async { println!() })
                .connect_from_env()
                .await?;
            println!("Connected. Type a line and press enter; Ctrl-D to quit.");
            let stdin = std::io::stdin();
            let mut line = String::new();
            while stdin.read_line(&mut line)? > 0 {
                session.send_text(line.trim()).await?;
                line.clear();
            }
            session.disconnect().await?;
        }
        SpecModality::Audio => run_audio(live).await?,
    }
    Ok(())
}

#[cfg(feature = "voice")]
async fn run_audio(live: Live) -> CliResult {
    let session = live.connect_from_env().await?;
    println!("Connected. Speak; Ctrl-C to quit.");
    session.talk().await?;
    Ok(())
}

#[cfg(not(feature = "voice"))]
#[allow(clippy::unused_async, reason = "same signature as the voice build")]
async fn run_audio(_live: Live) -> CliResult {
    Err("this is an audio spec: build adk with the `voice` feature \
         (cargo install gemini-adk-cli-rs --features voice), or run the generated Rust project"
        .into())
}

/// `adk spec schema` — the JSON Schema of a session spec, for editors,
/// validators and tools that draft specs.
pub fn schema() -> CliResult {
    println!(
        "{}",
        serde_json::to_string_pretty(&SessionSpec::json_schema())?
    );
    Ok(())
}
