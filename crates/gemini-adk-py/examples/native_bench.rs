//! The native-Rust side of `bench/python/live_api.py`.
//!
//! It runs the same SDK calls the Python binding makes (`AgentBuilder` +
//! `GeminiLlm::from_env`, `TextAgent::ask`, a JSON tool, `Live::builder()` +
//! `connect_from_env`) and times each call inside Rust, on a Tokio runtime
//! configured like the binding's. The Python driver sends one JSON command
//! per stdin line and reads one JSON result per stdout line, so process
//! start-up and the pipe are never inside a measurement.
//!
//! Commands:
//! - `{"op":"init","model":..,"temperature":..,"tool_schema":{..}}`
//! - `{"op":"ask","prompt":..}` → `{"ms":..,"chars":..}`
//! - `{"op":"tool","prompt":..}` → `{"ms":..,"tool_calls":..,"chars":..}`
//! - `{"op":"live","instruction":..,"text":..}` →
//!   `{"connect_ms":..,"first_audio_ms":..,"turn_ms":..,"audio_bytes":..,"chunks":..}`
//! - `{"op":"mock","n":..}` → `{"us":[..]}` (MockLlm echo, no network)
//!
//! Needs `GEMINI_API_KEY` for every op but `mock`. Build with
//! `cargo build --release --example native_bench` in `crates/gemini-adk-py`.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gemini_adk_fluent_rs::live::LiveEvent;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_rs::llm::MockLlm;
use gemini_adk_rs::text::TextAgent;
use gemini_adk_rs::tool::SimpleTool;
use serde_json::{json, Value};

struct Agents {
    plain: Arc<dyn TextAgent>,
    tooled: Arc<dyn TextAgent>,
    tool_calls: Arc<AtomicUsize>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn build(model: &str, temperature: f32, schema: Value) -> Result<Agents, String> {
    let llm = || GeminiLlm::from_env().map_err(|e| e.to_string());
    let plain = AgentBuilder::new("bench")
        .instruction("You are a concise assistant.")
        .model(ModelId::new(model))
        .temperature(temperature)
        .build(llm()?)
        .map_err(|e| e.to_string())?;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    // The same function the Python tool runs: a fixed reading for the city.
    let weather = SimpleTool::new(
        "get_weather",
        "Current weather for a city.",
        Some(schema),
        move |args: Value| {
            counter.fetch_add(1, Ordering::Relaxed);
            async move {
                let city = args["city"].as_str().unwrap_or("").to_string();
                Ok(json!({"city": city, "temp_c": 21, "conditions": "sunny"}))
            }
        },
    );
    let tooled = AgentBuilder::new("bench_tools")
        .instruction("You are a concise assistant. Use tools when they help.")
        .model(ModelId::new(model))
        .temperature(temperature)
        .tool(weather)
        .build(llm()?)
        .map_err(|e| e.to_string())?;
    Ok(Agents {
        plain,
        tooled,
        tool_calls: calls,
    })
}

async fn live(instruction: &str, text: &str) -> Result<Value, String> {
    let started = Instant::now();
    let handle = Live::builder()
        .instruction(instruction)
        .connect_from_env()
        .await
        .map_err(|e| e.to_string())?;
    let connect_ms = ms(started.elapsed());
    let mut events = handle.events();
    let sent = Instant::now();
    handle.send_text(text).await.map_err(|e| e.to_string())?;
    let (mut first_audio_ms, mut bytes, mut chunks) = (None, 0usize, 0usize);
    let deadline = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(deadline);
    let outcome = loop {
        tokio::select! {
            () = &mut deadline => break Err("no turn_complete within 60 s".to_string()),
            event = events.recv() => match event {
                Ok(LiveEvent::Audio(data)) => {
                    first_audio_ms.get_or_insert_with(|| ms(sent.elapsed()));
                    bytes += data.len();
                    chunks += 1;
                }
                Ok(LiveEvent::TurnComplete) => break Ok(ms(sent.elapsed())),
                Ok(LiveEvent::Disconnected { reason }) => {
                    break Err(format!("disconnected: {reason:?}"))
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => break Err(e.to_string()),
            }
        }
    };
    let _ = handle.disconnect().await;
    let turn_ms = outcome?;
    Ok(json!({
        "connect_ms": connect_ms,
        "first_audio_ms": first_audio_ms,
        "turn_ms": turn_ms,
        "audio_bytes": bytes,
        "chunks": chunks,
    }))
}

async fn mock(n: usize) -> Result<Value, String> {
    let agent = AgentBuilder::new("mock")
        .instruction("You are a concise assistant.")
        .build(MockLlm::echo())
        .map_err(|e| e.to_string())?;
    let mut us = Vec::with_capacity(n);
    for i in 0..n {
        let t = Instant::now();
        agent
            .ask(format!("ping {i}"))
            .await
            .map_err(|e| e.to_string())?;
        us.push(t.elapsed().as_secs_f64() * 1e6);
    }
    Ok(json!({ "us": us }))
}

async fn handle(command: Value, agents: &mut Option<Agents>) -> Result<Value, String> {
    let op = command["op"].as_str().unwrap_or("");
    let need = |agents: &Option<Agents>| -> Result<(), String> {
        agents.as_ref().map(|_| ()).ok_or_else(|| "send init first".to_string())
    };
    match op {
        "init" => {
            let model = command["model"].as_str().ok_or("init needs model")?;
            let temperature = command["temperature"].as_f64().unwrap_or(0.0) as f32;
            *agents = Some(build(model, temperature, command["tool_schema"].clone())?);
            Ok(json!({ "ok": true }))
        }
        "ask" => {
            need(agents)?;
            let a = agents.as_ref().ok_or("init")?;
            let prompt = command["prompt"].as_str().unwrap_or("").to_string();
            let t = Instant::now();
            let text = a.plain.ask(prompt).await.map_err(|e| e.to_string())?;
            Ok(json!({ "ms": ms(t.elapsed()), "chars": text.chars().count() }))
        }
        "tool" => {
            need(agents)?;
            let a = agents.as_ref().ok_or("init")?;
            let prompt = command["prompt"].as_str().unwrap_or("").to_string();
            let before = a.tool_calls.load(Ordering::Relaxed);
            let t = Instant::now();
            let text = a.tooled.ask(prompt).await.map_err(|e| e.to_string())?;
            let elapsed = ms(t.elapsed());
            Ok(json!({
                "ms": elapsed,
                "tool_calls": a.tool_calls.load(Ordering::Relaxed) - before,
                "chars": text.chars().count(),
            }))
        }
        "live" => {
            live(
                command["instruction"].as_str().unwrap_or(""),
                command["text"].as_str().unwrap_or(""),
            )
            .await
        }
        "mock" => mock(command["n"].as_u64().unwrap_or(1000) as usize).await,
        other => Err(format!("unknown op {other:?}")),
    }
}

fn main() {
    // Configured like the binding's runtime (`src/runtime.rs`).
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("runtime: {e}");
            std::process::exit(1);
        }
    };
    let mut agents = None;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(command) => runtime
                .block_on(handle(command, &mut agents))
                .unwrap_or_else(|e| json!({ "error": e })),
            Err(e) => json!({ "error": format!("bad command: {e}") }),
        };
        if writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).is_err() {
            break;
        }
    }
}
