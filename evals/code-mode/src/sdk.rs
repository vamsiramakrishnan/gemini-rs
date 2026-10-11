//! The same tasks through the SDK: a `Live` session with the five tools
//! registered, the booking gated by a governed flow, and code mode from
//! `gemini_adk_fluent_rs::code_mode`.
//!
//! - `sdk-tools`: function calls only.
//! - `sdk-code`: the four reads only through `run_code`; `book_table` stays
//!   a direct call, as the SDK recommends for commits.
//! - `sdk-hybrid`: `run_code` beside all five tools (the SDK default).
//!
//! The caller's yes is set as state from the line the task confirms on, so
//! the flow gate (`never(book_table).until(confirmed)`) decides bookings
//! and the comparison is not about extraction.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use gemini_adk_fluent_rs::code_mode::CodeMode;
use gemini_adk_fluent_rs::live::Live;
use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::flow::{Flow, Guard};
use gemini_adk_rs::live::LiveEvent;
use gemini_adk_rs::tool::SimpleTool;
use gemini_genai_rs::prelude::ModelId;
use gemini_genai_rs::session::SessionEvent;

use super::{FUNCTIONS, Task, World, instruction, run_tool, tool_declarations, Arm};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SdkArm {
    Tools,
    Code,
    Hybrid,
}

impl SdkArm {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tools => "sdk-tools",
            Self::Code => "sdk-code",
            Self::Hybrid => "sdk-hybrid",
        }
    }
}

#[derive(Default)]
struct Seen {
    text: String,
    turns: usize,
    last_event: Option<Instant>,
    first_output: Option<Instant>,
    round_trips: usize,
    model_calls: Vec<String>,
    programs: Vec<String>,
    program_errors: usize,
    executions: Vec<(String, Value)>,
    refusals: usize,
    prompt_tokens: u32,
    errors: Vec<String>,
    /// Whether the model has said anything since its last call.
    spoke_since_calls: bool,
    /// When the model's first audio of the exchange arrived.
    first_audio: Option<Instant>,
    /// What the server heard the caller say.
    heard: String,
    /// Whether a turn has completed since the model last spoke or called:
    /// the server reports a turn's usage with its `turnComplete`.
    completed_since_output: bool,
}

/// Wait until a turn has completed since `turns`, the model has spoken
/// since its last call (a call's own turn ends before it reacts to the
/// result), and nothing has arrived for 2.5 s.
async fn settle(seen: &Mutex<Seen>, turns: usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let s = seen.lock().unwrap();
        let quiet = s
            .last_event
            .is_some_and(|t| t.elapsed() > Duration::from_millis(2500));
        let reacted = s.round_trips == 0 || s.spoke_since_calls;
        if (s.turns > turns && quiet && reacted && s.completed_since_output)
            || Instant::now() > deadline
        {
            return;
        }
    }
}

pub async fn run_task_sdk(model: &str, arm: SdkArm, task: &Task, spoken: bool) -> Value {
    // The world's own gate stays open: the flow decides bookings.
    let world = Arc::new(Mutex::new(World {
        confirmed_from: Some(0),
        ..World::default()
    }));
    let mut live = Live::builder()
        .model(ModelId::new(model.to_string()))
        .instruction(instruction(Arm::Tools));
    live = if spoken { live.transcription() } else { live.text_only() };
    if model.contains("extended") {
        live = live.thinking_level(gemini_genai_rs::protocol::ThinkingLevel::Low);
    }
    for decl in tool_declarations() {
        let world = world.clone();
        let name = decl.name.clone();
        live = live.tool(SimpleTool::new(
            decl.name.clone(),
            decl.description.clone(),
            decl.parameters.clone(),
            move |args| {
                let world = world.clone();
                let name = name.clone();
                async move {
                    let mut w = world.lock().unwrap();
                    run_tool(&name, &args, &mut w).map_err(ToolError::ExecutionFailed)
                }
            },
        ));
    }
    let flow = Flow::new()
        .step("serve")
        .allow(FUNCTIONS)
        .terminal()
        .never("book_table")
        .until(Guard::is_true("confirmed"))
        .build()
        .expect("valid flow");
    live = live.govern(flow);
    let reads = FUNCTIONS.iter().filter(|f| **f != "book_table").copied();
    live = match arm {
        SdkArm::Tools => live,
        SdkArm::Code => live.code_mode(CodeMode::new().functions(reads).direct(false)),
        SdkArm::Hybrid => live.code_mode(CodeMode::new().functions(reads)),
    };
    let handle = match tokio::time::timeout(Duration::from_secs(60), live.connect_from_env()).await
    {
        Ok(Ok(handle)) => Arc::new(handle),
        Ok(Err(e)) => return json!({"task": task.id, "arm": arm.name(), "error": format!("connect: {e}")}),
        Err(_) => return json!({"task": task.id, "arm": arm.name(), "error": "connect timed out"}),
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
                let mut s = seen.lock().unwrap();
                let now = Instant::now();
                s.last_event = Some(now);
                match event {
                    SessionEvent::OutputTranscription(t) => {
                        s.first_output.get_or_insert(now);
                        s.text.push_str(&t);
                        s.spoke_since_calls = true;
                        s.completed_since_output = false;
                    }
                    SessionEvent::AudioData(_) => {
                        s.first_output.get_or_insert(now);
                        s.first_audio.get_or_insert(now);
                        s.spoke_since_calls = true;
                        s.completed_since_output = false;
                    }
                    SessionEvent::InputTranscription(t) => s.heard.push_str(&t),
                    SessionEvent::TextDelta(t) => {
                        s.first_output.get_or_insert(now);
                        s.text.push_str(&t);
                        s.spoke_since_calls = true;
                        s.completed_since_output = false;
                    }
                    SessionEvent::ToolCall(calls) => {
                        s.first_output.get_or_insert(now);
                        s.round_trips += 1;
                        s.spoke_since_calls = false;
                        s.completed_since_output = false;
                        for call in calls {
                            if call.name == "run_code" {
                                s.programs.push(
                                    call.args["code"].as_str().unwrap_or_default().to_string(),
                                );
                            }
                            s.model_calls.push(call.name);
                        }
                    }
                    SessionEvent::Usage(usage) => {
                        let prompt = usage.prompt_token_count.unwrap_or(0);
                        s.prompt_tokens = s.prompt_tokens.max(prompt);
                    }
                    SessionEvent::TurnComplete => {
                        s.turns += 1;
                        s.completed_since_output = true;
                    }
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
                    Ok(LiveEvent::ToolExecution { name, result, .. }) => {
                        let mut s = seen.lock().unwrap();
                        let failed = result.get("error").is_some();
                        if name == "run_code" && failed {
                            s.program_errors += 1;
                        }
                        if name == "book_table" && failed {
                            let reason = result["error"].as_str().unwrap_or_default();
                            if reason.contains("not permitted") || reason.contains("first,") {
                                s.refusals += 1;
                            }
                        }
                        s.executions.push((name, result));
                    }
                    Ok(LiveEvent::Error(e)) => seen.lock().unwrap().errors.push(e),
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    let mic = spoken.then(|| crate::voice::Mic::open(&handle));
    let started = Instant::now();
    let mut turns = Vec::new();
    let mut max_prompt = 0u32;
    let mut round_trips = 0;
    let mut unbacked_claims = 0;
    let mut last = String::new();
    for (i, line) in task.lines.iter().enumerate() {
        if task.confirms_from.is_some_and(|from| i >= from) {
            let _ = handle.state().set("confirmed", true);
        }
        world.lock().unwrap().line = i;
        let (from_turns, from_calls, from_programs, from_errors, from_executions) = {
            let mut s = seen.lock().unwrap();
            s.text.clear();
            s.heard.clear();
            s.first_output = None;
            s.first_audio = None;
            s.prompt_tokens = 0;
            s.round_trips = 0;
            s.spoke_since_calls = false;
            (s.turns, s.model_calls.len(), s.programs.len(), s.program_errors, s.executions.len())
        };
        let sent = match &mic {
            None => {
                let sent = Instant::now();
                if let Err(e) = handle.send_text(*line).await {
                    turns.push(json!({"caller": line, "error": e.to_string()}));
                    break;
                }
                sent
            }
            Some(mic) => {
                let Some(pcm) = crate::voice::speak(line, "Puck").await else {
                    turns.push(json!({"caller": line, "error": "speech synthesis failed"}));
                    break;
                };
                if let Err(e) = mic.say(&pcm).await {
                    turns.push(json!({"caller": line, "error": e.to_string()}));
                    break;
                }
                // The caller stopped speaking before the trailing silence.
                Instant::now()
                    - Duration::from_millis(crate::voice::TRAILING_SILENCE_MS as u64)
            }
        };
        settle(&seen, from_turns).await;
        let s = seen.lock().unwrap();
        let reply_ms = s.last_event.map(|t| t.saturating_duration_since(sent).as_millis());
        let said = s.text.trim().to_string();
        let lower = said.to_lowercase();
        let claims = ["is booked", "have booked", "i've booked", "booked your", "booked a table", "reservation is confirmed", "booking is confirmed", "reference"]
            .iter()
            .any(|w| lower.contains(w));
        if claims && world.lock().unwrap().bookings.is_empty() {
            unbacked_claims += 1;
        }
        max_prompt = max_prompt.max(s.prompt_tokens);
        round_trips += s.round_trips;
        last = said.clone();
        turns.push(json!({
            "caller": line,
            "said": said,
            "ms": reply_ms,
            "first_output_ms": s.first_output.map(|t| t.saturating_duration_since(sent).as_millis()),
            "first_audio_ms": s.first_audio.map(|t| t.saturating_duration_since(sent).as_millis()),
            "heard": s.heard.trim(),
            "round_trips": s.round_trips,
            "calls": s.model_calls[from_calls..].to_vec(),
            "programs": s.programs[from_programs..].to_vec(),
            "code_errors": s.program_errors - from_errors,
            "program_results": s.executions[from_executions..]
                .iter()
                .filter(|(n, _)| n == "run_code")
                .map(|(_, r)| {
                    let text = r.to_string();
                    if text.len() > 600 { format!("{}…", &text[..text.floor_char_boundary(600)]) } else { text }
                })
                .collect::<Vec<_>>(),
            "prompt_tokens": s.prompt_tokens,
        }));
    }
    if let Some(mic) = &mic {
        mic.close();
    }
    let _ = handle.disconnect().await;
    let s = seen.lock().unwrap();
    let w = world.lock().unwrap();
    let all_said: String = turns
        .iter()
        .filter_map(|t| t["said"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let (ok, why) = (task.grade)(&w, if task.lines.len() > 1 { &last } else { &all_said });
    let ms: u128 = turns.iter().filter_map(|t| t["ms"].as_u64()).map(u128::from).sum();
    json!({
        "task": task.id,
        "kind": task.kind,
        "arm": if spoken { format!("{}-voice", arm.name()) } else { arm.name().to_string() },
        "ok": ok,
        "why": why,
        "ms": ms,
        "wall_ms": started.elapsed().as_millis(),
        "round_trips": round_trips,
        "code_blocks": s.programs.len(),
        "code_errors": s.program_errors,
        "max_prompt_tokens": max_prompt,
        "executions": w.executions.len(),
        "refusals": s.refusals,
        "unbacked_claims": unbacked_claims,
        "bookings": w.bookings,
        "errors": s.errors,
        "turns": turns,
    })
}
