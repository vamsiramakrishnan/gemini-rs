//! `adk session replay` — offline replay of a recorded wire log through the
//! real L1 processor.
//!
//! Honest scope: replay re-processes the **recorded frames** only. The model
//! is never re-executed (its outputs are in the recorded inbound frames), and
//! since the CLI has no access to the application's tool implementations,
//! recorded tool calls surface as events but produce no new responses. State
//! keys originally written by tools will therefore drift unless they were
//! also recorded in a journal — which is exactly what `--journal` diffs.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use gemini_adk_rs::live::LiveSessionBuilder;
use gemini_adk_rs::live::replay::{collect_events_until_idle, replay_session};
use gemini_adk_rs::state::{State, StateMutation};
use gemini_genai_rs::prelude::SessionConfig;
use gemini_genai_rs::transport::{WireDirection, read_wire_log};

pub async fn replay(
    wire_log_path: &str,
    journal_path: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let entries = read_wire_log(wire_log_path)?;
    if entries.is_empty() {
        return Err(format!("wire log `{wire_log_path}` contains no entries").into());
    }

    let inbound = entries
        .iter()
        .filter(|e| e.dir == WireDirection::Inbound)
        .count();
    let outbound = entries.len() - inbound;
    let span_ms = entries
        .last()
        .map(|e| e.ts_ms.saturating_sub(entries[0].ts_ms))
        .unwrap_or(0);

    println!("\n  ADK Session Replay — {}\n", wire_log_path);
    println!(
        "  Wire log:  {} entries ({} inbound, {} outbound), {:.1}s recorded",
        entries.len(),
        inbound,
        outbound,
        span_ms as f64 / 1000.0
    );
    println!("  Mode:      offline — recorded frames only, no LLM re-execution,");
    println!("             no tool re-execution (the CLI has no tool implementations)\n");

    // Offline replay: the config only shapes the re-encoded setup frame that
    // goes to the replay transport. No network, no credentials.
    let state = State::new();
    let config = SessionConfig::new("offline-replay");
    let builder = LiveSessionBuilder::new(config.clone()).state(state.clone());

    let session = replay_session(config, builder, &entries).await?;
    let mut live_events = session.handle().events();
    // The raw wire stream is single-producer and strictly wire-ordered —
    // use it for the turn-by-turn summary (the processed LiveEvent stream
    // interleaves the concurrent fast/control lanes).
    let mut wire_events = session.handle().session().subscribe();

    session.release();
    session.drained().await;

    // Settle: wait until the processor has stopped emitting effects.
    let _ = collect_events_until_idle(
        &mut live_events,
        Duration::from_millis(300),
        Duration::from_secs(30),
    )
    .await;

    let mut events: Vec<gemini_genai_rs::prelude::SessionEvent> = Vec::new();
    while let Ok(event) = wire_events.try_recv() {
        events.push(event);
    }

    print_turns(&events);
    print_final_state(&state);

    // Disconnect before diffing so the recorded journal's lifecycle tail
    // (session:disconnected, session:phase = "Disconnected") has its replayed
    // counterpart and doesn't show up as false drift.
    session.disconnect().await.ok();
    for _ in 0..20 {
        if state.get::<bool>("session:disconnected").unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    match journal_path {
        Some(path) => diff_journal(path, &state),
        None => Ok(()),
    }
}

/// Group wire events into turns (split on `TurnComplete`) and print a summary.
fn print_turns(events: &[gemini_genai_rs::prelude::SessionEvent]) {
    use gemini_genai_rs::prelude::SessionEvent;

    println!("  Turn-by-turn:");
    let mut turn = 1usize;
    let mut text = String::new();
    let mut tools: Vec<String> = Vec::new();
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut any = false;
    let mut all_tools: Vec<String> = Vec::new();

    let flush = |turn: usize,
                 text: &mut String,
                 tools: &mut Vec<String>,
                 counts: &mut BTreeMap<&'static str, usize>| {
        let summary = counts
            .iter()
            .map(|(k, v)| format!("{k}×{v}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("    turn {turn:>2}  [{summary}]");
        let preview: String = text.chars().take(72).collect();
        let ellipsis = if text.chars().count() > 72 { "…" } else { "" };
        if !preview.is_empty() {
            println!("             “{preview}{ellipsis}”");
        }
        for tool in tools.iter() {
            println!("             tool: {tool}");
        }
        text.clear();
        tools.clear();
        counts.clear();
    };

    for event in events {
        any = true;
        let label: &'static str = match event {
            SessionEvent::AudioData(_) => "audio",
            SessionEvent::TextDelta(t) => {
                text.push_str(t);
                "text"
            }
            SessionEvent::TextComplete(_) => "text_complete",
            SessionEvent::InputTranscription(_) => "input_transcript",
            SessionEvent::OutputTranscription(_) => "output_transcript",
            SessionEvent::Thought(_) => "thought",
            SessionEvent::ToolCall(calls) => {
                for call in calls {
                    let rendered = format!("{}({})", call.name, call.args);
                    tools.push(rendered.clone());
                    all_tools.push(rendered);
                }
                "tool_call"
            }
            SessionEvent::ToolCallCancelled(_) => "tool_cancelled",
            SessionEvent::Interrupted => "interrupted",
            SessionEvent::GenerationComplete => "generation_complete",
            SessionEvent::TurnComplete => {
                *counts.entry("turn_complete").or_insert(0) += 1;
                flush(turn, &mut text, &mut tools, &mut counts);
                turn += 1;
                continue;
            }
            SessionEvent::Connected
            | SessionEvent::PhaseChanged(_)
            | SessionEvent::Disconnected(_) => continue,
            _ => "other",
        };
        *counts.entry(label).or_insert(0) += 1;
    }
    if !counts.is_empty() || !text.is_empty() || !tools.is_empty() {
        flush(turn, &mut text, &mut tools, &mut counts);
    }
    if !any {
        println!("    (no events)");
    }

    if !all_tools.is_empty() {
        println!("\n  Tool calls (recorded; not re-executed):");
        for call in all_tools {
            println!("    {call}");
        }
    }
}

fn print_final_state(state: &State) {
    let map: BTreeMap<String, Value> = state.to_hashmap().into_iter().collect();
    println!("\n  Final state ({} keys):", map.len());
    if map.is_empty() {
        println!("    (empty)");
    }
    for (key, value) in &map {
        let rendered = value.to_string();
        let preview: String = rendered.chars().take(60).collect();
        let ellipsis = if rendered.chars().count() > 60 {
            "…"
        } else {
            ""
        };
        println!("    {key} = {preview}{ellipsis}");
    }
}

/// Keys derived from the wall clock of the run itself — excluded from drift.
fn is_volatile_key(key: &str) -> bool {
    matches!(
        key,
        "session:elapsed_ms" | "session:silence_ms" | "session:remaining_budget_ms"
    )
}

/// Diff the recorded journal's per-key final values against the replayed state.
fn diff_journal(path: &str, state: &State) -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read_to_string(path)?;
    let mut journal: Vec<StateMutation> = Vec::new();
    for (idx, line) in data.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let m: StateMutation = serde_json::from_str(line)
            .map_err(|e| format!("invalid journal entry on line {}: {e}", idx + 1))?;
        journal.push(m);
    }

    // Per-key final value from the journal (None = removed).
    let mut recorded: BTreeMap<String, Option<Value>> = BTreeMap::new();
    for m in &journal {
        if !is_volatile_key(&m.key) {
            recorded.insert(m.key.clone(), m.new.clone());
        }
    }
    let replayed: BTreeMap<String, Value> = state
        .to_hashmap()
        .into_iter()
        .filter(|(k, _)| !is_volatile_key(k))
        .collect();

    let mut drift: Vec<String> = Vec::new();
    for (key, recorded_value) in &recorded {
        match (recorded_value, replayed.get(key)) {
            (Some(r), Some(p)) if r == p => {}
            (None, None) => {}
            (Some(r), Some(p)) => drift.push(format!("{key}: recorded {r} ≠ replayed {p}")),
            (Some(r), None) => drift.push(format!("{key}: recorded {r}, missing on replay")),
            (None, Some(p)) => drift.push(format!("{key}: removed in recording, replayed {p}")),
        }
    }
    for key in replayed.keys() {
        if !recorded.contains_key(key) {
            drift.push(format!(
                "{key}: written on replay, absent from recorded journal"
            ));
        }
    }

    println!(
        "\n  Journal diff — {} ({} mutations, {} keys):",
        path,
        journal.len(),
        recorded.len()
    );
    if drift.is_empty() {
        println!("    CLEAN — replayed final state matches the recorded journal");
        Ok(())
    } else {
        println!("    DRIFT — {} key(s) diverged:", drift.len());
        for line in &drift {
            println!("      - {line}");
        }
        println!("    note: keys written by tools/LLM-driven extractors are expected to");
        println!("    drift in CLI replay, which re-executes neither.");
        Err(format!("journal drift on {} key(s)", drift.len()).into())
    }
}
