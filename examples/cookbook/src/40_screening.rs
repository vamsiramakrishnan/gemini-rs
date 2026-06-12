//! # 40 — Call-screening capstone: Flow × Extract × Orchestration  (Tier: Run)
//!
//! What it teaches: on-device call screening — fuzzy/keyword recognizers
//! classify the caller, a branching `Flow` routes or blocks, and an orchestrated
//! sub-agent produces the screening card. No caller data leaves the process; no
//! credentials needed.
//!
//! Key concepts:
//! - `Recognizer::fuzzy` (roster) + `Recognizer::one_of` (spam signatures / category)
//! - `Flow` branching via `gate(..)` — route vs block
//! - `call_agent(..)` produces a screening summary in `State`
//!
//! Runs real logic: Yes — two scenarios (legit caller + spam) end to end.

use std::sync::Arc;

use gemini_adk_fluent_rs::agents::call_agent;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::tools::Recognizer;

fn screening_flow() -> Flow {
    Flow::new()
        .step("screen")
        .posture("Find out who is calling and why.")
        .done(Guard::captured(["category"]))
        .step("route")
        .after("screen")
        .gate(Guard::not(Guard::is_true("spam"))) // only when not spam
        .allow(["transfer"])
        .done(Guard::called_ok("transfer"))
        .step("block")
        .after("screen")
        .gate(Guard::is_true("spam")) // only when spam
        .terminal()
        // A spam caller is never transferred — a global gate, independent of
        // which step is active (terminal steps latch done immediately).
        .never("transfer")
        .until(Guard::not(Guard::is_true("spam")))
        .build()
        .expect("valid flow")
}

const SPAM: [&str; 3] = ["extended warranty", "free cruise", "claim your prize"];

/// Classify one caller utterance into State using deterministic recognizers.
fn classify(utterance: &str, state: &State) {
    if let Some((org, _)) =
        Recognizer::fuzzy(["Acme Corp", "Globex", "Initech"]).recognize(utterance)
    {
        let _ = state.set("caller_org", org);
    }
    if Recognizer::one_of(SPAM).recognize(utterance).is_some() {
        let _ = state.set("spam", true);
        let _ = state.set("category", "spam");
    } else if let Some((cat, _)) =
        Recognizer::one_of(["invoice", "billing", "support", "sales"]).recognize(utterance)
    {
        let _ = state.set("category", cat);
    }
}

#[tokio::main]
async fn main() {
    println!("=== 40: Call-screening capstone ===\n");
    println!("--- The flow ---\n{}", screening_flow().to_mermaid());

    // Scenario A — a legitimate caller is screened and routed.
    println!("--- Scenario A: legitimate caller ---");
    let mut mon = FlowMonitor::new(screening_flow(), FlowMode::Enforce);
    let state = State::new();
    classify("hi this is acme calling about an invoice", &state);
    mon.on_turn(&state);
    println!(
        "    caller_org={:?} category={:?} spam={:?}",
        state.get::<String>("caller_org"),
        state.get::<String>("category"),
        state.get::<bool>("spam")
    );
    println!(
        "    route: {:?}   block: {:?}",
        mon.verdict("route", &state),
        mon.verdict("block", &state)
    );

    // Orchestration — produce the screening card with a sub-agent (sync `call`).
    let summary = Arc::new(FnTextAgent::new("screen_card", |s: &State| {
        Ok(format!(
            "{} — {}",
            s.get::<String>("caller_org")
                .unwrap_or_else(|| "unknown".into()),
            s.get::<String>("category")
                .unwrap_or_else(|| "general".into()),
        ))
    }));
    let card = call_agent("screen_card", summary, &state).await.unwrap();
    println!("    screening card: {card:?}");
    println!(
        "    'transfer': {}",
        if mon.admits_tool("transfer", &state).is_ok() {
            "admitted"
        } else {
            "denied"
        }
    );

    // Scenario B — a spam caller is blocked deterministically.
    println!("\n--- Scenario B: spam caller ---");
    let mut mon = FlowMonitor::new(screening_flow(), FlowMode::Enforce);
    let state = State::new();
    classify("I'm calling about your car's extended warranty", &state);
    mon.on_turn(&state);
    println!(
        "    spam={:?} category={:?}",
        state.get::<bool>("spam"),
        state.get::<String>("category")
    );
    println!(
        "    route: {:?}   block: {:?}",
        mon.verdict("route", &state),
        mon.verdict("block", &state)
    );
    match mon.admits_tool("transfer", &state) {
        Ok(()) => println!("    'transfer': admitted"),
        Err(e) => println!("    'transfer': DENIED — {e}"),
    }

    println!("\ncall-screening capstone completed successfully!");
}
