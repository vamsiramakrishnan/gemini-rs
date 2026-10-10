//! `adk spec` — work with a session spec (`agent.json`): generate a project
//! around it, run its tests, call one of its tools, or run it live.
//!
//! `catalog`, `check`, `plan`, `answer` and `patch` are the authoring
//! interface a coding harness drives: see `spec::authoring`.
//!
//! A spec is the whole agent as data: model, instruction, conversation, tool
//! declarations and tests. These commands are how a spec authored in Flow
//! Studio becomes a project, and how a Python or Go project's tools are
//! exercised through the same bindings the runtime uses.

use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::spec::authoring::{self, Answer, CheckReport, Decisions, PatchOp};
use gemini_adk_fluent_rs::spec::{
    ProjectLanguage, ProjectOptions, SdkSource, SessionSpec, SpecModality, SpecResources,
};
use serde_json::{Value, json};

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
    let validation = spec.validate_for_replay();
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
    for report in spec.run_task_scenarios().await {
        if report.passed {
            passed += 1;
            println!("ok    task scenario {}", report.name);
        } else {
            failed += 1;
            println!("FAIL  task scenario {}: {:?}", report.name, report.error);
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
    // `.env.local` first: dotenvy never overrides a variable already set.
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
    let spec = load(spec_path)?;
    if spec.requires_memory() {
        return Err(
            "this spec uses `memory`, which needs a memory engine: generate a Rust project \
             (`adk spec codegen --lang rust`) and run that"
                .into(),
        );
    }
    let mut resources = SpecResources::default();
    if spec.requires_extraction() {
        resources.extraction_llm = Some(Arc::new(GeminiLlm::from_env()?));
    }
    if !spec.decisions.is_empty() {
        resources.decision_model = Some(Arc::new(
            gemini_adk_rs::decision::GatewayDecisionModel::from_env()?,
        ));
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

/// `adk spec graph <spec>` — the spec's flow as a Mermaid diagram.
pub fn graph(spec_path: &str) -> CliResult {
    let spec = load(spec_path)?;
    let validation = spec.validate_for_replay();
    if !validation.valid {
        for error in &validation.errors {
            eprintln!("error: {error}");
        }
        return Err(format!("{spec_path} is invalid; run `adk spec check`").into());
    }
    println!("{}", validation.mermaid);
    Ok(())
}

/// `adk spec catalog` — the authoring vocabulary as JSON.
pub fn catalog() -> CliResult {
    println!("{}", serde_json::to_string_pretty(&authoring::catalog())?);
    Ok(())
}

/// `adk spec check <spec> [--json]` — diagnostics with pointers and fixes.
/// Exits non-zero when the spec has errors.
pub fn check(spec_path: &str, as_json: bool) -> CliResult {
    let raw = fs::read_to_string(spec_path).map_err(|e| format!("{spec_path}: {e}"))?;
    let report = authoring::check_str(&raw);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    if report.valid {
        Ok(())
    } else {
        Err(format!("{spec_path} has errors").into())
    }
}

fn print_report(report: &CheckReport) {
    for d in &report.diagnostics {
        let severity = serde_json::to_value(d.severity).unwrap_or_default();
        println!(
            "{:<8} {:<14} {}",
            severity.as_str().unwrap_or_default(),
            d.code,
            d.path.as_deref().unwrap_or("")
        );
        println!("         {}", d.message);
        if let Some(fix) = &d.fix {
            println!("         fix: {}", fix.description);
        }
    }
    let errors = report
        .diagnostics
        .iter()
        .filter(|d| d.severity == authoring::Severity::Error)
        .count();
    let warnings = report.diagnostics.len() - errors;
    if report.diagnostics.is_empty() {
        println!("ok");
    } else {
        println!("\n{errors} error(s), {warnings} warning(s)");
    }
}

/// `adk spec plan <spec> [--decisions f] [--json]` — the open questions.
pub fn plan(spec_path: &str, decisions_path: Option<&str>, as_json: bool) -> CliResult {
    let doc = read_doc(spec_path)?;
    let decisions = read_decisions(decisions_path)?;
    let plan = authoring::plan(&doc, &decisions);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }
    if plan.questions.is_empty() {
        println!("No open questions.");
    }
    for q in &plan.questions {
        let tag = if q.blocking { "blocking" } else { "optional" };
        println!("[{tag}] {}\n  {}\n  why: {}", q.id, q.ask, q.why);
        let options: Vec<String> = q
            .options
            .iter()
            .map(|o| {
                let mut v = o.value.clone();
                if q.default.as_deref() == Some(o.value.as_str()) {
                    v.push_str(" (default)");
                }
                if o.needs_value {
                    v.push_str(" <value>");
                }
                v
            })
            .collect();
        println!("  options: {}\n", options.join(", "));
    }
    if plan.ready {
        println!("Ready to generate.");
    } else {
        println!(
            "Not ready: {} blocking question(s){}.",
            plan.blocking,
            if authoring::check(&doc).valid {
                ""
            } else {
                "; `adk spec check` reports errors"
            }
        );
    }
    Ok(())
}

/// `adk spec answer <spec> <answers> [--decisions f] [--write]` — apply
/// answers. Prints the applied ids, decisions, check report and remaining
/// plan, plus the spec unless it was written back.
pub fn answer(
    spec_path: &str,
    answers: &str,
    decisions_path: Option<&str>,
    write: bool,
) -> CliResult {
    let doc = read_doc(spec_path)?;
    let decisions = read_decisions(decisions_path)?;
    let answers: Vec<Answer> = match json_arg(answers)? {
        Value::Array(items) => serde_json::from_value(Value::Array(items))?,
        one => vec![serde_json::from_value(one)?],
    };
    let answered = authoring::answer(&doc, &answers, &decisions).map_err(|e| e.to_string())?;
    let mut out = json!({
        "applied": answered.applied,
        "decisions": answered.decisions,
        "check": authoring::check(&answered.spec),
        "plan": authoring::plan(&answered.spec, &answered.decisions),
    });
    if write {
        write_json(spec_path, &answered.spec)?;
        if let Some(path) = decisions_path {
            write_json(path, &serde_json::to_value(&answered.decisions)?)?;
        }
    } else {
        out["spec"] = answered.spec;
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// `adk spec patch <spec> <ops> [--write]` — apply JSON-patch operations.
/// Prints the check report, plus the spec unless it was written back.
pub fn patch(spec_path: &str, ops: &str, write: bool) -> CliResult {
    let doc = read_doc(spec_path)?;
    let ops: Vec<PatchOp> = serde_json::from_value(json_arg(ops)?)?;
    let patched = authoring::apply_patch(&doc, &ops).map_err(|e| e.to_string())?;
    let mut out = json!({ "check": authoring::check(&patched) });
    if write {
        write_json(spec_path, &patched)?;
    } else {
        out["spec"] = patched;
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// The spec file as JSON, without deserializing it as a spec.
fn read_doc(path: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(serde_json::from_str(&raw).map_err(|e| format!("{path}: {e}"))?)
}

fn read_decisions(path: Option<&str>) -> Result<Decisions, Box<dyn std::error::Error>> {
    match path {
        Some(path) if Path::new(path).exists() => {
            let raw = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            Ok(serde_json::from_str(&raw).map_err(|e| format!("{path}: {e}"))?)
        }
        _ => Ok(Decisions::new()),
    }
}

/// A JSON argument: inline JSON, `-` for stdin, or a file path.
fn json_arg(arg: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let text = if arg == "-" {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        text
    } else if arg.trim_start().starts_with(['[', '{']) {
        arg.to_string()
    } else {
        fs::read_to_string(arg).map_err(|e| format!("{arg}: {e}"))?
    };
    Ok(serde_json::from_str(&text).map_err(|e| format!("{arg}: {e}"))?)
}

fn write_json(path: &str, value: &Value) -> CliResult {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    Ok(())
}
