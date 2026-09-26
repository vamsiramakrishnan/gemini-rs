//! `adk flow` — devtools for the conversation compiler: inspect, graph, simulate.
//!
//! Operates on a serializable [`ConversationSpec`] (JSON) — the same artifact the
//! `conversation-from-script` skill emits — so the authoring loop is: draft →
//! `inspect`/`graph` → `simulate`, all without a live API.

use std::fs;

use gemini_adk_fluent_rs::conversation::{CompiledConversation, Conversation, ConversationSpec};
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::simulation::Scenario;

/// Load and compile a conversation spec from a JSON file.
fn load(spec_path: &str) -> Result<CompiledConversation, Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(spec_path)?;
    let spec: ConversationSpec = serde_json::from_str(&raw)?;
    Conversation::from_spec_stubbing_resolvers(spec).map_err(|e| e.to_string().into())
}

/// A human-readable summary of a compiled conversation.
pub fn inspect_summary(convo: &CompiledConversation) -> String {
    let flow = convo.flow().flow();
    let mut out = String::new();
    out.push_str(&format!("conversation: {}\n", convo.spec().name));
    out.push_str(&format!("stages ({}):\n", flow.steps.len()));
    for s in &flow.steps {
        let kind = if s.terminal { "terminal" } else { "stage" };
        out.push_str(&format!("  - {} [{kind}]\n", s.id));
    }
    let tools = &convo.flow().tool_surface().tools;
    out.push_str(&format!(
        "tools ({}): {}\n",
        tools.len(),
        tools.iter().cloned().collect::<Vec<_>>().join(", ")
    ));
    if !convo.overlays().is_empty() {
        out.push_str(&format!("digressions ({}):\n", convo.overlays().len()));
        for ov in convo.overlays() {
            out.push_str(&format!("  - {} (resume: {:?})\n", ov.name, ov.resume));
        }
    }
    let redacted = convo.redacted_fields();
    if !redacted.is_empty() {
        out.push_str(&format!(
            "redacted: {}\n",
            redacted.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !convo.policies().is_empty() {
        out.push_str(&format!("policies ({}):\n", convo.policies().len()));
        for p in convo.policies() {
            out.push_str(&format!("  - {p:?}\n"));
        }
    }
    out
}

/// `adk flow inspect <spec.json>` — print a compiled-conversation summary.
pub fn inspect(spec_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let convo = load(spec_path)?;
    print!("{}", inspect_summary(&convo));
    Ok(())
}

/// `adk flow graph <spec.json>` — print the governed flow as a Mermaid diagram.
pub fn graph(spec_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let convo = load(spec_path)?;
    println!("{}", convo.to_mermaid());
    Ok(())
}

/// `adk flow schema` — print the JSON Schema for a `ConversationSpec`.
///
/// This is the machine-readable contract an authoring tool, form, or LLM
/// targets when drafting a spec.
pub fn schema() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "{}",
        gemini_adk_fluent_rs::conversation::conversation_spec_schema()
    );
    Ok(())
}

/// `adk flow validate <spec.json>` — compile the spec and report errors as JSON.
///
/// Prints `{"valid": true}` on success, or a structured diagnostic
/// (`{"valid": false, "error": { "kind": ..., "errors": [...] }}`) on failure,
/// and exits non-zero so it can gate CI. Errors are machine-readable so a web
/// UI can render them.
pub fn validate(spec_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(spec_path)?;
    let spec: ConversationSpec = serde_json::from_str(&raw)?;
    match Conversation::from_spec_stubbing_resolvers(spec) {
        Ok(_) => {
            println!("{}", serde_json::json!({ "valid": true }));
            Ok(())
        }
        Err(e) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "valid": false,
                    "error": e,
                }))?
            );
            Err("spec failed to compile".into())
        }
    }
}

/// `adk flow simulate <spec.json> <scenario.json>` — run a model-free scenario.
pub async fn simulate(
    spec_path: &str,
    scenario_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let convo = load(spec_path)?;
    let scenario: Scenario = serde_json::from_str(&fs::read_to_string(scenario_path)?)?;
    match scenario.run(&convo, Enforcement::Enforce).await {
        Ok(()) => {
            println!("PASS: scenario '{}'", scenario.name);
            Ok(())
        }
        Err(msg) => {
            eprintln!("FAIL: {msg}");
            Err(msg.into())
        }
    }
}

/// Load a recorded session's mutation journal and the scenario it implies.
fn recorded(journal_path: &str) -> Result<Scenario, Box<dyn std::error::Error>> {
    let journal = gemini_adk_rs::state::read_journal(journal_path)?;
    let name = std::path::Path::new(journal_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recorded")
        .to_string();
    Ok(Scenario::from_journal(name, &journal))
}

/// `adk flow replay <spec> --journal <journal>` — re-run a recorded session's
/// decisions through a spec, turn by turn, and report where the spec now
/// decides differently from the session.
///
/// The session's slot writes and tool outcomes are replayed as recorded; at
/// each turn the spec's active steps, and each tool admission, are compared
/// with what the session did. Exits non-zero on divergence, so a spec change
/// can be checked against production recordings.
pub async fn replay(spec_path: &str, journal_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    use gemini_adk_fluent_rs::simulation::{Sim, SimStep};

    let convo = load(spec_path)?;
    let scenario = recorded(journal_path)?;
    let mut sim = Sim::new(&convo, Enforcement::Enforce);
    let mut turn = 0u32;
    let mut diverged = 0usize;
    println!("replaying {journal_path} through {spec_path}\n");
    for step in &scenario.steps {
        let outcome = sim.apply(step).await;
        match step {
            SimStep::Turn => {
                turn += 1;
                let overlay = sim
                    .active_overlay()
                    .map(|o| format!(" (in digression `{o}`)"))
                    .unwrap_or_default();
                println!("turn {turn}: active {:?}{overlay}", sim.active());
            }
            SimStep::Set { key, value } => println!("  {key} = {value}"),
            SimStep::ToolResult { tool, ok } => {
                println!("  tool {tool} {}", if *ok { "succeeded" } else { "failed" });
            }
            _ => {}
        }
        if let Err(msg) = outcome {
            diverged += 1;
            println!("  DIVERGED: the session and the spec disagree — {msg}");
        }
    }
    if sim.is_complete() {
        println!("\nconversation complete");
    }
    if diverged == 0 {
        println!("CLEAN: the spec reproduces every recorded decision ({turn} turns)");
        Ok(())
    } else {
        Err(format!("DIVERGED at {diverged} point(s) over {turn} turns").into())
    }
}

/// `adk flow why <spec> --journal <journal> --turn N [--tool T]` — explain
/// the flow's state at turn `N` of a recorded session: which steps were
/// active, what each was waiting for, and which tools were blocked and why.
/// With `--tool`, answer for that tool alone.
pub async fn why(
    spec_path: &str,
    journal_path: &str,
    at_turn: u32,
    tool: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use gemini_adk_fluent_rs::simulation::{Sim, SimStep};

    let convo = load(spec_path)?;
    let scenario = recorded(journal_path)?;
    let mut sim = Sim::new(&convo, Enforcement::Enforce);
    // Replay what happened up to the end of turn `at_turn`'s boundary.
    let mut turn = 0u32;
    for step in &scenario.steps {
        if turn == at_turn {
            break;
        }
        let expectation = matches!(
            step,
            SimStep::ExpectActive(_)
                | SimStep::ExpectAllowed(_)
                | SimStep::ExpectDenied(_)
                | SimStep::ExpectSlot { .. }
                | SimStep::ExpectComplete
        );
        if !expectation {
            let _ = sim.apply(step).await;
        }
        if matches!(step, SimStep::Turn) {
            turn += 1;
        }
    }
    if turn < at_turn {
        return Err(format!("the recording has only {turn} turns").into());
    }
    match tool {
        Some(tool) => {
            if sim.allowed(tool) {
                println!("turn {at_turn}: `{tool}` was admitted");
            } else {
                let why = sim.denied().get(tool).cloned().unwrap_or_default();
                println!("turn {at_turn}: `{tool}` was blocked: {why}");
            }
        }
        None => println!("{}", serde_json::to_string_pretty(&sim.explain())?),
    }
    Ok(())
}

/// `adk flow ci <dir>` — Conversation CI: compile every `*.spec.json` in `dir`
/// (recursively) and run each paired `*.scenario.json`, deterministically.
///
/// Pairing convention: `<name>.spec.json` is tested by `<name>.scenario.json`
/// and any `<name>.<label>.scenario.json` in the **same** directory. A spec
/// with no scenarios is still compile-checked.
///
/// Exits non-zero if any spec fails to compile or any scenario fails. With
/// `--json`, prints a machine-readable report instead of the human summary.
pub async fn ci(dir: &str, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut specs = Vec::new();
    collect_specs(std::path::Path::new(dir), &mut specs)?;
    specs.sort();
    if specs.is_empty() {
        return Err(format!("no *.spec.json files found under '{dir}'").into());
    }

    let mut spec_reports = Vec::new();
    let (mut specs_failed, mut scen_total, mut scen_passed) = (0usize, 0usize, 0usize);

    for spec_path in &specs {
        let raw = fs::read_to_string(spec_path)?;
        let spec: Result<ConversationSpec, _> = serde_json::from_str(&raw);
        let compiled = spec
            .map_err(|e| serde_json::Value::String(format!("invalid JSON: {e}")))
            .and_then(|s| {
                Conversation::from_spec_stubbing_resolvers(s)
                    .map_err(|e| serde_json::to_value(&e).unwrap_or_default())
            });
        let convo = match compiled {
            Ok(c) => c,
            Err(err) => {
                specs_failed += 1;
                spec_reports.push(serde_json::json!({
                    "spec": spec_path.display().to_string(),
                    "compiled": false,
                    "error": err,
                }));
                if !json {
                    println!("  {} — compile FAILED", spec_path.display());
                    println!("      {err}");
                }
                continue;
            }
        };

        if !json {
            println!("  {} — compiled ✓", spec_path.display());
        }
        let mut scen_reports = Vec::new();
        for scen_path in paired_scenarios(spec_path)? {
            scen_total += 1;
            // A malformed scenario is a failed scenario, not an abort: the
            // command's contract is compile-and-run-ALL with a full report.
            let scenario: Scenario = match fs::read_to_string(&scen_path)
                .map_err(|e| e.to_string())
                .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
            {
                Ok(s) => s,
                Err(msg) => {
                    let msg = format!("unreadable or invalid scenario JSON: {msg}");
                    scen_reports.push(serde_json::json!({
                        "scenario": scen_path.display().to_string(),
                        "ok": false, "error": msg,
                    }));
                    if !json {
                        println!("      {} ... FAIL", scen_path.display());
                        println!("          {msg}");
                    }
                    continue;
                }
            };
            let name = scenario.name.clone();
            match scenario.run(&convo, Enforcement::Enforce).await {
                Ok(()) => {
                    scen_passed += 1;
                    scen_reports.push(serde_json::json!({
                        "scenario": scen_path.display().to_string(), "name": name, "ok": true,
                    }));
                    if !json {
                        println!("      {name} ... PASS");
                    }
                }
                Err(msg) => {
                    scen_reports.push(serde_json::json!({
                        "scenario": scen_path.display().to_string(), "name": name,
                        "ok": false, "error": msg,
                    }));
                    if !json {
                        println!("      {name} ... FAIL");
                        println!("          {msg}");
                    }
                }
            }
        }
        spec_reports.push(serde_json::json!({
            "spec": spec_path.display().to_string(),
            "compiled": true,
            "scenarios": scen_reports,
        }));
    }

    let scen_failed = scen_total - scen_passed;
    let ok = specs_failed == 0 && scen_failed == 0;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": ok,
                "specs": spec_reports,
                "summary": {
                    "specs_total": specs.len(), "specs_failed": specs_failed,
                    "scenarios_total": scen_total, "scenarios_passed": scen_passed,
                    "scenarios_failed": scen_failed,
                },
            }))?
        );
    } else {
        println!(
            "\nspecs: {} ok / {} failed   scenarios: {} passed / {} failed",
            specs.len() - specs_failed,
            specs_failed,
            scen_passed,
            scen_failed
        );
    }

    if ok {
        Ok(())
    } else {
        Err("conversation CI failed".into())
    }
}

/// Recursively collect `*.spec.json` files under `dir`.
fn collect_specs(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_specs(&path, out)?;
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".spec.json"))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Scenarios paired with `spec_path` by the `<name>[.<label>].scenario.json`
/// naming convention, in the same directory.
fn paired_scenarios(
    spec_path: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let file = spec_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = file.strip_suffix(".spec.json").unwrap_or(file);
    let dir = spec_path.parent().unwrap_or(std::path::Path::new("."));
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.ends_with(".scenario.json") => n.to_string(),
            _ => continue,
        };
        let scen_stem = name.strip_suffix(".scenario.json").unwrap_or(&name);
        if scen_stem == stem || scen_stem.starts_with(&format!("{stem}.")) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
    {
      "name": "booking",
      "stages": [
        { "id": "collect", "collect": ["party_size"],
          "next": [{ "to": "confirm", "when": { "captured": ["party_size"] } }] },
        { "id": "confirm", "allow": ["book"],
          "commit": { "tool": "book", "when": { "is_true": "user_confirmed" } },
          "next": [{ "to": "done", "when": { "called_ok": "book" } }] },
        { "id": "done", "terminal": true }
      ],
      "require": ["done"],
      "policies": [{ "kind": "redact", "keys": ["card_number"] }]
    }
    "#;

    fn compiled() -> CompiledConversation {
        let spec: ConversationSpec = serde_json::from_str(SPEC).unwrap();
        Conversation::from_spec(spec).unwrap()
    }

    #[test]
    fn inspect_summary_lists_stages_tools_and_policies() {
        let summary = inspect_summary(&compiled());
        assert!(summary.contains("conversation: booking"));
        assert!(summary.contains("collect"));
        assert!(summary.contains("book"));
        assert!(summary.contains("redacted: card_number"));
    }

    #[test]
    fn graph_renders_mermaid() {
        let mermaid = compiled().to_mermaid();
        assert!(mermaid.contains("flowchart") || mermaid.contains("graph"));
    }

    /// A recorded booking: party size captured, confirmed, booked.
    fn journal(dir: &std::path::Path) -> String {
        let lines = [
            (1, "party_size", "4"),
            (2, "flow:active", r#"["confirm"]"#),
            (3, "user_confirmed", "true"),
            (4, "flow:tool_call", r#"{"tool":"book","id":"c1"}"#),
            (
                5,
                "flow:tool_result",
                r#"{"tool":"book","id":"c1","ok":true}"#,
            ),
            (6, "flow:active", "[]"),
        ];
        let body: String = lines
            .iter()
            .map(|(seq, key, new)| {
                format!(
                    "{{\"sequence\":{seq},\"key\":\"{key}\",\"old\":null,\"new\":{new},\"origin\":\"set\",\"timestamp_ms\":{},\"delta\":false}}\n",
                    1_718_000_000_000u64 + seq
                )
            })
            .collect();
        let path = dir.join("session.journal.jsonl");
        fs::write(&path, body).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn replay_is_clean_under_the_recorded_spec_and_diverges_under_a_stricter_one() {
        let dir = corpus_dir("replay");
        let journal = journal(&dir);
        let spec = dir.join("booking.spec.json");
        fs::write(&spec, SPEC).unwrap();
        replay(spec.to_str().unwrap(), &journal).await.unwrap();

        let stricter = SPEC.replace(
            r#"{ "is_true": "user_confirmed" }"#,
            r#"{ "all": [{ "is_true": "user_confirmed" }, { "is_true": "manager_ok" }] }"#,
        );
        let strict = dir.join("strict.spec.json");
        fs::write(&strict, stricter).unwrap();
        let err = replay(strict.to_str().unwrap(), &journal)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("DIVERGED"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn why_explains_a_turn_of_the_recording() {
        let dir = corpus_dir("why");
        let journal = journal(&dir);
        let spec = dir.join("booking.spec.json");
        fs::write(&spec, SPEC).unwrap();
        why(spec.to_str().unwrap(), &journal, 1, Some("book"))
            .await
            .unwrap();
        why(spec.to_str().unwrap(), &journal, 1, None)
            .await
            .unwrap();
        assert!(
            why(spec.to_str().unwrap(), &journal, 9, None)
                .await
                .is_err()
        );
        fs::remove_dir_all(&dir).ok();
    }

    fn corpus_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "adk_flow_ci_{label}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const PASS_SCENARIO: &str = r#"{ "name": "happy", "steps": [
        { "expect_active": ["collect"] },
        { "expect_denied": "book" },
        { "set": { "key": "party_size", "value": 2 } },
        "turn",
        { "expect_active": ["confirm"] }
    ] }"#;

    #[tokio::test]
    async fn ci_passes_on_a_valid_corpus() {
        let dir = corpus_dir("pass");
        fs::write(dir.join("booking.spec.json"), SPEC).unwrap();
        fs::write(dir.join("booking.happy.scenario.json"), PASS_SCENARIO).unwrap();
        let res = ci(dir.to_str().unwrap(), true).await;
        assert!(res.is_ok(), "valid corpus should pass: {res:?}");
        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn ci_fails_on_a_bad_assertion() {
        let dir = corpus_dir("fail");
        fs::write(dir.join("booking.spec.json"), SPEC).unwrap();
        // Asserts a stage active that isn't — must fail and exit non-zero.
        fs::write(
            dir.join("booking.wrong.scenario.json"),
            r#"{ "name": "wrong", "steps": [ { "expect_active": ["done"] } ] }"#,
        )
        .unwrap();
        let res = ci(dir.to_str().unwrap(), true).await;
        assert!(res.is_err(), "a failing scenario must fail CI");
        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn ci_fails_on_an_uncompilable_spec() {
        let dir = corpus_dir("badspec");
        // An always-true commit guard is an unguarded commit — rejected at compile.
        fs::write(
            dir.join("loose.spec.json"),
            r#"{ "name": "loose", "stages": [
                { "id": "a", "commit": { "tool": "pay", "when": { "always": null } },
                  "next": [{ "to": "b", "when": { "called_ok": "pay" } }] },
                { "id": "b", "terminal": true } ], "require": ["b"] }"#,
        )
        .unwrap();
        let res = ci(dir.to_str().unwrap(), true).await;
        assert!(res.is_err(), "an uncompilable spec must fail CI");
        fs::remove_dir_all(&dir).ok();
    }
}
