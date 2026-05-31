//! # 39 — Booking capstone: Flow × Extract × Orchestration  (Tier: Run)
//!
//! What it teaches: combine all three higher-order lenses in one appointment
//! flow — deterministic extraction fills slots, the `check` step's `on_enter`
//! action orchestrates an availability sub-agent automatically, and a `Flow`
//! DAG gates the booking commit.
//!
//! Key concepts:
//! - `Recognizer` (incl. `datetime`) fills `State` (party size, slot, time)
//! - `Flow` `on_enter("check", run(agent, mode))` — the step drives
//!   orchestration in-session; the result lands in `check:result`
//! - `Flow` `done(captured([..]))` + `done(resolved("check"))` +
//!   `commit("book", ..)` gate the irreversible booking
//!
//! Runs real logic: Yes — drives the whole loop with no credentials.

use std::sync::Arc;

use gemini_adk_fluent_rs::prelude::*;
use serde_json::{json, Value};

fn booking_flow() -> Flow {
    Flow::new()
        .step("collect")
        .posture("Ask for the party size and a preferred time.")
        .done(Guard::captured(["party_size", "slot"]))
        .step("check")
        .after("collect")
        .posture("Check availability for the requested time.")
        .done(Guard::resolved("check")) // ← completes on its own on_enter result
        .step("book")
        .after("check")
        // Ground the model on the known facts so it restates rather than invents.
        .ground("Party of {party_size} at {slot}; availability: {check:result}.")
        .allow(["book"])
        .done(Guard::called_ok("book"))
        .step("close")
        .after("book")
        .terminal()
        // The booking commit is once-only and gated until availability is known.
        .never("book")
        .until(Guard::resolved("check"))
        .once("book")
        .require(["close"])
        .build()
        .expect("valid flow")
}

/// The availability sub-agent the `check` step runs on entry. A real one would
/// hit a calendar; here it reads the recognized slot from `State`.
fn availability_agent() -> Arc<dyn TextAgent> {
    Arc::new(FnTextAgent::new("availability", |s: &State| {
        let slot = s.get::<String>("slot").unwrap_or_default();
        Ok(if slot == "afternoon" { "open" } else { "full" }.to_string())
    }))
}

#[tokio::main]
async fn main() {
    println!("=== 39: Booking capstone (Flow × Extract × Orchestration) ===\n");

    let flow = booking_flow();
    println!("--- The flow ---\n{}", flow.to_mermaid());

    // The `check` step orchestrates the availability agent the moment it
    // activates — no manual call, just `on_enter`. In a Live session this is
    // `Live::builder().govern(flow).on_enter("check", agent, AgentMode::Call)`.
    let mut mon = FlowMonitor::new(flow, FlowMode::Enforce)
        .on_enter("check", run_on_enter(availability_agent(), AgentMode::Call));
    let state = State::new();

    // 1. EXTRACT — deterministic recognizers fill the slots from what was said.
    let utterance = "I'd like a table for 4 tomorrow afternoon at 3pm please";
    let party = Recognizer::integer_near(["table", "for", "party", "people"]);
    let slot = Recognizer::one_of(["morning", "afternoon", "evening"]);
    if let Some((v, _)) = party.recognize(utterance) {
        state.set("party_size", v);
    }
    if let Some((v, _)) = slot.recognize(utterance) {
        state.set("slot", v);
    }
    // The `datetime` recognizer normalizes the clock/calendar phrase on-device.
    if let Some((when, _)) = Recognizer::datetime().recognize(utterance) {
        state.set("when", when);
    }
    mon.on_turn(&state);
    println!("--- After extraction ---");
    println!(
        "    party_size = {:?}, slot = {:?}, when = {:?}",
        state.get::<u32>("party_size"),
        state.get::<String>("slot"),
        state.get::<Value>("when"),
    );
    println!(
        "    collect: {:?}   check: {:?}",
        mon.verdict("collect", &state),
        mon.verdict("check", &state)
    );

    // 2. ORCHESTRATION — `check` is now active, so its `on_enter` action runs
    //    the availability agent at the turn boundary (exactly what the Live
    //    control plane does for you). The result lands in `check:result`.
    mon.fire_enter_actions(&state).await;
    println!("\n--- After the check step's on_enter orchestration ---");
    println!(
        "    check:result = {:?}",
        state.get::<String>("check:result")
    );
    mon.on_turn(&state);
    println!("    check: {:?}", mon.verdict("check", &state));
    println!(
        "    provenance(check:result) = {:?}",
        provenance(&state, "check:result")
    );

    // 3. FLOW — the commit is now admitted (availability resolved); book once.
    //    The `book` step grounds the model on the known facts (anti-hallucination).
    println!("\n--- Booking commit ---");
    for line in mon.active_grounds(&state) {
        println!("    [ground] {line}");
    }
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
