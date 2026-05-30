//! # 39 — Booking capstone: Flow × Extract × Orchestration  (Tier: Run)
//!
//! What it teaches: combine all three higher-order lenses in one appointment
//! flow — deterministic extraction fills slots, an orchestrated sub-agent checks
//! availability (sync `call`), and a `Flow` DAG gates the booking commit.
//!
//! Key concepts:
//! - `Extract` recognizers fill `State` (slot, party size)
//! - `call_agent(..)` runs a sub-agent synchronously; its result lands in
//!   `availability:result`
//! - `Flow` `done(captured([..]))` + `done(resolved("availability"))` +
//!   `commit("book", ..)` gate the irreversible booking
//!
//! Runs real logic: Yes — drives the whole loop with no credentials.

use std::sync::Arc;

use gemini_adk_fluent_rs::prelude::*;
use serde_json::json;

fn booking_flow() -> Flow {
    Flow::new()
        .step("collect")
        .posture("Ask for the party size and a preferred time.")
        .done(Guard::captured(["party_size", "slot"]))
        .step("check")
        .after("collect")
        .posture("Check availability for the requested time.")
        .done(Guard::resolved("availability")) // ← an orchestrated sub-agent's result
        .step("book")
        .after("check")
        .allow(["book"])
        .done(Guard::called_ok("book"))
        .step("close")
        .after("book")
        .terminal()
        // The booking commit is once-only and gated until availability is known.
        .never("book")
        .until(Guard::resolved("availability"))
        .once("book")
        .require(["close"])
        .build()
        .expect("valid flow")
}

#[tokio::main]
async fn main() {
    println!("=== 39: Booking capstone (Flow × Extract × Orchestration) ===\n");

    let flow = booking_flow();
    println!("--- The flow ---\n{}", flow.to_mermaid());

    let mut mon = FlowMonitor::new(flow, FlowMode::Enforce);
    let state = State::new();

    // 1. EXTRACT — deterministic recognizers fill the slots from what was said.
    let utterance = "I'd like a table for 4 in the afternoon please";
    let party = Recognizer::integer_near(["table", "for", "party", "people"]);
    let slot = Recognizer::one_of(["morning", "afternoon", "evening"]);
    if let Some((v, _)) = party.recognize(utterance) {
        state.set("party_size", v);
    }
    if let Some((v, _)) = slot.recognize(utterance) {
        state.set("slot", v);
    }
    mon.on_turn(&state);
    println!("--- After extraction ---");
    println!(
        "    party_size = {:?}, slot = {:?}",
        state.get::<u32>("party_size"),
        state.get::<String>("slot")
    );
    println!(
        "    collect: {:?}   check: {:?}",
        mon.verdict("collect", &state),
        mon.verdict("check", &state)
    );

    // 2. ORCHESTRATION — call an availability sub-agent synchronously (`call`).
    //    A real one would hit a calendar; here it reads the recognized slot.
    let availability = Arc::new(FnTextAgent::new("availability", |s: &State| {
        let slot = s.get::<String>("slot").unwrap_or_default();
        Ok(if slot == "afternoon" {
            "open".into()
        } else {
            "full".into()
        })
    }));
    let verdict = call_agent("availability", availability, &state)
        .await
        .unwrap();
    println!("\n--- After orchestrated availability check ---");
    println!("    availability:result = {verdict:?}");
    mon.on_turn(&state);
    println!("    check: {:?}", mon.verdict("check", &state));

    // 3. FLOW — the commit is now admitted (availability resolved); book once.
    println!("\n--- Booking commit ---");
    match mon.admits_tool("book", &state) {
        Ok(()) => println!("    'book': admitted"),
        Err(e) => println!("    'book': DENIED — {e}"),
    }
    mon.observe_tool("book", true, &state);
    state.set("booking", json!({ "slot": "afternoon", "party_size": 4 }));
    mon.on_turn(&state);
    println!(
        "    'book' again: {:?}",
        mon.admits_tool("book", &state).err()
    );
    println!(
        "    book: {:?}   close: {:?}",
        mon.verdict("book", &state),
        mon.verdict("close", &state)
    );
    println!("    flow complete: {}", mon.is_complete());

    println!("\nbooking capstone completed successfully!");
}
