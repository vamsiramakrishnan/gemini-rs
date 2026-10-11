//! Skills on Gemini 3.8 Live, with the foreground skill's tools declared
//! through `contextUpdate` and with every skill's tools declared at connect.
//!
//! A task session on a model that accepts `contextUpdate` declares only
//! `task_control` at connect. Starting a task declares that skill's tools
//! (those its flow offers) before the `task_control` response, so the model
//! can call them in the same turn, and puts the skill's instruction in the
//! system instruction. The `unscoped` arm sets `runtime.steering` to
//! `hybrid`, which keeps the earlier behaviour: every skill's tools declared
//! from the start.
//!
//! Each run plays the same typed caller against the collections gallery spec
//! (payment history, then a payment-method question) and records, per turn,
//! the calls the model made, which of them named a tool of a skill that was
//! not in the foreground, what was declared, and the prompt tokens the
//! server reported. It reports; it asserts nothing about the model.
//!
//! ```text
//! GEMINI_API_KEY=… cargo test -p gemini-adk-fluent-rs --test skills_live -- --ignored --nocapture
//! ```
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `SKILLS_LIVE_MODEL` | `models/gemini-3.8-live` | Live model |
//! | `SKILLS_LIVE_RUNS` | `3` | Runs per arm |

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};

use gemini_adk_fluent_rs::live::Live;
use gemini_adk_fluent_rs::spec::{SessionSpec, SpecResources};
use gemini_adk_rs::State;
use gemini_adk_rs::llm::GeminiLlm;
use gemini_genai_rs::prelude::ModelId;
use gemini_genai_rs::session::SessionEvent;

const SPEC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/gemini-adk-web-rs/static/examples/flows/collections.json"
);

const LINES: &[&str] = &[
    "Hi, I'd like to check whether my recent payments went through. I'm Jordan Ellis, account ACME-4821, born March 14th, 1988.",
    "Great, thanks. One more thing: can I pay by bank transfer?",
    "Okay, that's everything. Thank you.",
];

#[derive(Default)]
struct Seen {
    /// Model calls, with the foreground skill when each arrived.
    calls: Vec<(String, Option<String>)>,
    /// `task_control` arguments, in order.
    control: Vec<Value>,
    /// Errors the session reported.
    errors: Vec<String>,
    /// Highest prompt token count reported since the last line.
    prompt_tokens: u32,
    turns: usize,
    last_event: Option<Instant>,
}

fn foreground_skill(handle: &gemini_adk_fluent_rs::live::LiveHandle) -> Option<String> {
    let snapshot = handle.task_snapshot()?;
    let id = snapshot.foreground.as_ref()?;
    snapshot
        .tasks
        .iter()
        .find(|t| &t.id == id)
        .map(|t| t.skill.name.clone())
}

/// Wait until a turn has completed since `turns` and nothing has arrived
/// for a moment: a call's own turn ends before the model reacts to its
/// result.
async fn settle(seen: &Mutex<Seen>, turns: usize) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let s = seen.lock();
        let quiet = s
            .last_event
            .is_some_and(|t| t.elapsed() > Duration::from_millis(2500));
        if (s.turns > turns && quiet) || Instant::now() > deadline {
            return;
        }
    }
}

async fn run(scoped: bool, model: &str) -> Value {
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(SPEC).unwrap()).unwrap();
    if !scoped {
        doc["runtime"]["steering"] = json!("hybrid");
    }
    let spec = SessionSpec::from_value(doc).expect("spec");
    let state = State::new();
    let resources = SpecResources {
        extraction_llm: Some(Arc::new(GeminiLlm::from_env().expect("GEMINI_API_KEY"))),
        ..SpecResources::default()
    };
    let live = spec
        .apply(Live::builder(), &state, &resources)
        .expect("apply")
        .model(ModelId::new(model.to_string()))
        .transcription();
    let handle = match tokio::time::timeout(Duration::from_secs(60), live.connect_from_env()).await
    {
        Ok(Ok(handle)) => Arc::new(handle),
        Ok(Err(e)) => return json!({ "error": format!("connect: {e}") }),
        Err(_) => return json!({ "error": "connect timed out" }),
    };
    let seen = Arc::new(Mutex::new(Seen::default()));
    {
        let mut events = handle.subscribe();
        let seen = seen.clone();
        let handle = handle.clone();
        tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                let event = match events.recv().await {
                    Ok(e) => e,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                let foreground = matches!(event, SessionEvent::ToolCall(_))
                    .then(|| foreground_skill(&handle))
                    .flatten();
                let mut s = seen.lock();
                s.last_event = Some(Instant::now());
                match event {
                    SessionEvent::ToolCall(calls) => {
                        for call in calls {
                            if call.name == "task_control" {
                                s.control.push(call.args.clone());
                            }
                            s.calls.push((call.name, foreground.clone()));
                        }
                    }
                    SessionEvent::Usage(usage) => {
                        let prompt = usage.prompt_token_count.unwrap_or(0);
                        s.prompt_tokens = s.prompt_tokens.max(prompt);
                    }
                    SessionEvent::TurnComplete => s.turns += 1,
                    _ => {}
                }
            }
        });
    }
    {
        let mut events = handle.events();
        let seen = seen.clone();
        tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match events.recv().await {
                    Ok(gemini_adk_rs::live::LiveEvent::Error(e)) => seen.lock().errors.push(e),
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }
    let declared = || {
        handle
            .state()
            .session()
            .get::<Vec<String>>("declared_tools")
            .unwrap_or_default()
    };
    // The greeting.
    settle(&seen, 0).await;
    let mut turns = Vec::new();
    for line in LINES {
        let (from_calls, from_turns, from_control, from_errors) = {
            let mut s = seen.lock();
            s.prompt_tokens = 0;
            (s.calls.len(), s.turns, s.control.len(), s.errors.len())
        };
        if let Err(e) = handle.send_text(*line).await {
            turns.push(json!({ "caller": line, "error": e.to_string() }));
            break;
        }
        settle(&seen, from_turns).await;
        let s = seen.lock();
        let calls: Vec<_> = s.calls[from_calls..].to_vec();
        // A call naming a skill tool outside the foreground skill is refused.
        let wrong_skill = calls
            .iter()
            .filter(|(name, foreground)| {
                name.split_once("__")
                    .is_some_and(|(skill, _)| foreground.as_deref() != Some(skill))
            })
            .count();
        turns.push(json!({
            "caller": line,
            "calls": calls.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "wrong_skill_calls": wrong_skill,
            "declared_after": declared(),
            "prompt_tokens": s.prompt_tokens,
            "task_control": s.control[from_control..].to_vec(),
            "errors": s.errors[from_errors..].to_vec(),
        }));
    }
    let snapshot = handle.task_snapshot();
    let _ = handle.disconnect().await;
    let succeeded: Vec<_> = snapshot
        .iter()
        .flat_map(|s| &s.operations)
        .filter(|op| op.status == gemini_adk_rs::tasks::OperationStatus::Succeeded)
        .map(|op| op.tool.clone())
        .collect();
    let tasks: Vec<_> = snapshot
        .iter()
        .flat_map(|s| &s.tasks)
        .map(|t| format!("{}:{:?}", t.skill.name, t.status))
        .collect();
    json!({ "turns": turns, "succeeded": succeeded, "tasks": tasks })
}

#[tokio::test]
#[ignore = "talks to the Live API; run with --ignored"]
async fn skills_with_and_without_context_update() {
    let model =
        std::env::var("SKILLS_LIVE_MODEL").unwrap_or_else(|_| "models/gemini-3.8-live".to_string());
    let runs: usize = std::env::var("SKILLS_LIVE_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut report = Vec::new();
    for scoped in [true, false] {
        let arm = if scoped { "scoped" } else { "unscoped" };
        for i in 0..runs {
            let result = run(scoped, &model).await;
            println!("■ {arm} run {}: {}", i + 1, result);
            report.push(json!({ "arm": arm, "result": result }));
        }
    }
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/tmp/skills-live");
    std::fs::create_dir_all(dir).unwrap();
    let path = format!("{dir}/report.json");
    std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!("report: {path}");
}
