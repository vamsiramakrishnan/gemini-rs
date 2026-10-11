//! Code mode inside a skill, on Gemini 3.8 Live.
//!
//! The collections gallery spec's `payment_history` skill gets 36 months of
//! payments (an implementation that honours the requested period) and
//! `"code": {"only_in_code": ["lookup_payment_history"]}`, so
//! the history is reachable only through `payment_history__run_code` while
//! `verify_identity` stays a direct tool. A typed caller verifies and asks
//! for a 2025 total and how many payments were returned. Each run records the
//! model's calls, the task operations that succeeded (a program's calls are
//! operations of the skill's task), the prompt tokens and whether the answer
//! named the right total and count. It reports; it asserts nothing about the
//! model.
//!
//! ```text
//! GEMINI_API_KEY=… cargo test -p gemini-adk-fluent-rs --test skills_code_live -- --ignored --nocapture
//! ```

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};

use gemini_adk_fluent_rs::live::Live;
use gemini_adk_fluent_rs::spec::{SessionSpec, SpecResources};
use gemini_adk_fluent_rs::tools::ContextTool;
use gemini_adk_rs::State;
use gemini_adk_rs::llm::GeminiLlm;
use gemini_genai_rs::prelude::ModelId;
use gemini_genai_rs::session::SessionEvent;

const SPEC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/gemini-adk-web-rs/static/examples/flows/collections.json"
);

const LINES: &[&str] = &[
    "Hi, I'm Jordan Ellis, account ACME-4821, born March 14th, 1988. For calendar year 2025, what did I pay in total to the cent, and how many of my payments were returned?",
    "Thanks, that's all.",
];

/// 36 monthly payments, January 2024 to December 2026, with a returned
/// payment every fifth month.
fn payments() -> Vec<Value> {
    (0..36)
        .map(|i| {
            let (year, month) = (2024 + i / 12, i % 12 + 1);
            let amount = 80.0 + f64::from((i * 37) % 90) + f64::from(i % 4) * 0.25;
            let status = if i % 5 == 3 { "returned" } else { "posted" };
            json!({ "date": format!("{year}-{month:02}-07"), "amount_usd": amount, "status": status })
        })
        .collect()
}

/// The 2025 total of posted payments, and the 2025 returned count.
fn truth() -> (f64, usize) {
    let rows = payments();
    let in_2025 = rows
        .iter()
        .filter(|p| p["date"].as_str().unwrap().starts_with("2025"));
    let mut total = 0.0;
    let mut returned = 0;
    for p in in_2025 {
        if p["status"] == "returned" {
            returned += 1;
        } else {
            total += p["amount_usd"].as_f64().unwrap();
        }
    }
    (total, returned)
}

#[derive(Default)]
struct Seen {
    calls: Vec<String>,
    programs: Vec<String>,
    text: String,
    prompt_tokens: u32,
    turns: usize,
    last_event: Option<Instant>,
}

async fn settle(seen: &Mutex<Seen>, turns: usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let s = seen.lock();
        let quiet = s
            .last_event
            .is_some_and(|t| t.elapsed() > Duration::from_millis(3000));
        // Task tools answer later: wait until the model has spoken a total.
        let answered = s.text.contains("total") || s.text.contains("Total");
        if (s.turns > turns && quiet && answered) || Instant::now() > deadline {
            return;
        }
    }
}

async fn run(model: &str) -> Value {
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(SPEC).unwrap()).unwrap();
    let skills = doc["skills"].as_array_mut().unwrap();
    let skill = skills
        .iter_mut()
        .find(|s| s["name"] == "payment_history")
        .unwrap();
    for tool in skill["tools"].as_array_mut().unwrap() {
        if tool["name"] == "lookup_payment_history" {
            tool["response"]["payments"] = Value::Array(payments());
            tool["description"] = json!(
                "Read posted and returned payments for this checked account and requested period. \
                 A returned payment did not go through."
            );
        }
    }
    skill["code"] = json!({ "only_in_code": ["lookup_payment_history"] });
    let spec = SessionSpec::from_value(doc).expect("spec");
    let state = State::new();
    // The history honours `period`, as the tool's description promises: a
    // year in it keeps that year's rows. The mock response stays in the spec
    // for the shape the program's docstring shows.
    let resources = SpecResources {
        extraction_llm: Some(Arc::new(GeminiLlm::from_env().expect("GEMINI_API_KEY"))),
        ..SpecResources::default()
    }
    .implement_skill(
        "payment_history",
        ContextTool::new(
            "lookup_payment_history",
            "Read payments for the requested period",
            None,
            |args, _ctx| async move {
                let period = args["period"].as_str().unwrap_or_default().to_string();
                let year: String = period
                    .chars()
                    .filter(char::is_ascii_digit)
                    .take(4)
                    .collect();
                let rows: Vec<Value> = payments()
                    .into_iter()
                    .filter(|p| year.len() < 4 || p["date"].as_str().unwrap().starts_with(&year))
                    .collect();
                Ok(json!({ "account_last4": "4821", "payments": rows, "period": period }))
            },
        ),
    );
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
        tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                let event = match events.recv().await {
                    Ok(e) => e,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                let mut s = seen.lock();
                s.last_event = Some(Instant::now());
                match event {
                    SessionEvent::ToolCall(calls) => {
                        for call in calls {
                            if let Some(code) = call.args.get("code").and_then(Value::as_str) {
                                s.programs.push(code.to_string());
                            }
                            s.calls.push(call.name);
                        }
                    }
                    SessionEvent::TextDelta(t) | SessionEvent::OutputTranscription(t) => {
                        s.text.push_str(&t);
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
    settle(&seen, 0).await;
    let mut answer = String::new();
    for (i, line) in LINES.iter().enumerate() {
        let from = {
            let mut s = seen.lock();
            s.text.clear();
            s.turns
        };
        if handle.send_text(*line).await.is_err() {
            break;
        }
        settle(&seen, from).await;
        if i == 0 {
            answer = seen.lock().text.clone();
        }
    }
    let declared = handle
        .state()
        .session()
        .get::<Vec<String>>("declared_tools")
        .unwrap_or_default();
    let snapshot = handle.task_snapshot();
    let _ = handle.disconnect().await;
    let succeeded: Vec<String> = snapshot
        .iter()
        .flat_map(|s| &s.operations)
        .filter(|op| op.status == gemini_adk_rs::tasks::OperationStatus::Succeeded)
        .map(|op| op.tool.clone())
        .collect();
    let (total, returned) = truth();
    let said = answer.replace(',', "").replace('$', "");
    let words: Vec<&str> = said
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .map(|w| w.trim_end_matches('.'))
        .collect();
    let s = seen.lock();
    json!({
        "calls": s.calls,
        "succeeded": succeeded,
        "declared_at_end": declared,
        "prompt_tokens": s.prompt_tokens,
        "answer": answer,
        "programs": s.programs,
        "right_total": words.contains(&format!("{total:.2}").as_str())
            || words.contains(&format!("{total:.0}").as_str())
            || said.to_lowercase().contains("one thousand one hundred eighty-five"),
        "right_returned": words.contains(&returned.to_string().as_str())
            || words.contains(&["zero", "one", "two", "three", "four", "five"][returned.min(5)]),
        "truth": { "total": format!("{total:.2}"), "returned": returned },
    })
}

#[tokio::test]
#[ignore = "talks to the Live API; run with --ignored"]
async fn a_skill_program_answers_over_its_history() {
    let model =
        std::env::var("SKILLS_LIVE_MODEL").unwrap_or_else(|_| "models/gemini-3.8-live".to_string());
    let runs: usize = std::env::var("SKILLS_LIVE_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut report = Vec::new();
    for i in 0..runs {
        let result = run(&model).await;
        println!("■ run {}: {}", i + 1, result);
        report.push(result);
    }
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/tmp/skills-code-live"
    );
    std::fs::create_dir_all(dir).unwrap();
    let path = format!("{dir}/report.json");
    std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!("report: {path}");
}
