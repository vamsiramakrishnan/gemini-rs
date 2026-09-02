//! # 37 — Governed Flow (conversation/tool DAG)  (Tier: Run)
//!
//! What it teaches: describe a workflow as one `Flow` DAG spanning conversation
//! stages *and* tool-call milestones, then enforce it during the session.
//!
//! Key concepts:
//! - `Flow::new()` / `Guard` — the cemented builder verbs (`step`, `after`,
//!   `done`, `posture`, `allow`, `once`, `never…until`, `require`)
//! - `FlowMonitor` — marking, `admits_tool`, `active_postures`, `verdict`
//! - `Live::builder().govern(flow)` — enforce it in a live session
//!
//! Runs real logic: Yes — drives a `FlowMonitor` through a simulated debt-
//! collection conversation (no credentials needed).

use gemini_adk_fluent_rs::prelude::*;

fn debt_collection_flow() -> Flow {
    Flow::new()
        .step("verify")
        .posture("Verify the caller's identity before anything else.")
        .allow(["lookup_account"])
        .done(Guard::is_true("identity_verified"))
        .step("disclose")
        .after("verify")
        .posture("Give the required mini-Miranda disclosure.")
        .done(Guard::is_true("disclosure_given"))
        .step("negotiate")
        .after("disclose")
        .posture("Negotiate an affordable payment.")
        .allow(["lookup_balance", "payment_plans"])
        .done(Guard::captured(["ptp_amount", "ptp_date"]))
        .step("take_payment")
        .after("negotiate")
        .allow(["charge_card"])
        .done(Guard::called_ok("charge_card"))
        .step("close")
        .after("negotiate")
        .terminal()
        // commit-tool safety: charge_card at most once, blocked until confirmed
        .never("charge_card")
        .until(Guard::is_true("ptp_confirmed"))
        .once("charge_card")
        .require(["close"])
        .build()
        .expect("valid flow")
}

fn main() {
    println!("=== 37: Governed Flow ===\n");

    let flow = debt_collection_flow();

    // The spec *is* the diagram.
    println!(
        "--- The flow as a Mermaid diagram ---\n{}",
        flow.to_mermaid()
    );

    // In a live session you'd simply: `Live::builder().govern(flow).connect_from_env()`
    println!("--- In a live session ---");
    println!(
        "    Live::builder()\n        .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)\n        .tools(dispatcher)\n        .govern(flow)            // enforce the DAG\n        .connect_from_env().await?;\n"
    );

    // Drive a monitor through a simulated conversation.
    let mut mon = FlowMonitor::new(flow, FlowMode::Enforce);
    let state = State::new();

    println!("--- Turn 0: only `verify` is active ---");
    report(&mon, &state);
    show_gate(&mon, &state, "lookup_account");
    show_gate(&mon, &state, "charge_card"); // not available yet

    println!("\n--- Caller verifies identity ---");
    let _ = state.set("identity_verified", true);
    mon.on_turn(&state);
    report(&mon, &state);

    println!("\n--- Disclosure given, then a promise-to-pay is captured ---");
    let _ = state.set("disclosure_given", true);
    let _ = state.set("ptp_amount", 200);
    let _ = state.set("ptp_date", "2026-06-12");
    mon.on_turn(&state);
    report(&mon, &state);

    println!("\n--- Attempt to charge before confirmation (blocked) ---");
    show_gate(&mon, &state, "charge_card");
    println!("    Caller confirms the plan…");
    let _ = state.set("ptp_confirmed", true);
    show_gate(&mon, &state, "charge_card"); // now admitted
    mon.observe_tool("charge_card", true, &state);
    mon.on_turn(&state);
    println!("    charge_card succeeded.");
    show_gate(&mon, &state, "charge_card"); // once → blocked second time
    report(&mon, &state);
    println!("    flow complete: {}", mon.is_complete());

    // Observe mode: nothing blocked, deviations recorded for audit.
    println!("\n--- Observe mode (audit, no blocking) ---");
    let mut audit = FlowMonitor::new(debt_collection_flow(), FlowMode::Observe);
    let s2 = State::new();
    audit.observe_tool("charge_card", true, &s2); // out of order on turn 0
    println!("    recorded {} violation(s):", audit.violations().len());
    for v in audit.violations() {
        println!("      - {} → {}", v.subject, v.reason);
    }

    println!("\ngoverned flow example completed successfully!");
}

fn report(mon: &FlowMonitor, state: &State) {
    let active: Vec<String> = mon
        .active_steps(state)
        .iter()
        .map(|s| s.id.clone())
        .collect();
    println!("    active: {active:?}");
    for id in ["verify", "disclose", "negotiate", "take_payment", "close"] {
        println!("      {id:14} {:?}", mon.verdict(id, state));
    }
    for posture in mon.active_postures(state) {
        println!("    posture → {posture}");
    }
}

fn show_gate(mon: &FlowMonitor, state: &State, tool: &str) {
    match mon.admits_tool(tool, state) {
        Ok(()) => println!("    tool '{tool}': admitted"),
        Err(reason) => println!("    tool '{tool}': DENIED — {reason}"),
    }
}
