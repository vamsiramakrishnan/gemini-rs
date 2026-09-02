//! What a real application looks like: a phase machine, a governed flow, and
//! memory — composed, not bolted together.
//!
//! The point of this example is the *absence* of memory-specific plumbing. A
//! remembered fact fills a governed `State` slot, and from there every existing
//! mechanism reads it: `needs` stops the model re-asking, `requires` gates a
//! phase, `Flow` guards advance, `P::with_state` puts the value in the prompt.
//! The application declares what it needs and never has to ask whether the
//! answer arrived this minute or last month.
//!
//! ```text
//! cargo run -p gemini-memory-rs --example flow_memory_companion
//! ```
//!
//! This builds and inspects the session offline; it does not connect.

use std::sync::Arc;

use gemini_adk_fluent_rs::compose::T;
use gemini_adk_fluent_rs::live::Live;
use gemini_adk_rs::flow::{Enforcement, Flow, FlowMonitor, Guard};
use gemini_adk_rs::state::State;
use gemini_genai_rs::prelude::{ModelId, Voice};

use gemini_memory_rs::core::{SessionId, UserId};
use gemini_memory_rs::engine::MemoryEngine;
use gemini_memory_rs::runtime::{LiveMemoryExt, MemorySlot, MemoryTurnExtractor};

use gemini_adk_rs::live::extractor::TurnExtractor;
use gemini_adk_rs::live::transcript::TranscriptTurn;

/// The slots this application reasons about.
///
/// Left: what memory calls the fact. Right: the `State` key the phase machine
/// and the flow gate on. This mapping is the entire contract between the two.
fn slots() -> Vec<MemorySlot> {
    vec![
        MemorySlot::new("dietary_identity", "user:diet"),
        MemorySlot::new("preference", "user:venue"),
        MemorySlot::new("spouse", "user:partner"),
    ]
}

/// The flow the session is governed by.
///
/// Ordinary flow code that knows nothing about memory. `know_diet` completes on
/// a slot memory fills, and the booking tool is forbidden until both slots
/// exist — so the model cannot book a table before the dietary constraint is
/// established, whether that fact arrives this minute or came back from last
/// month.
fn flow_spec() -> Flow {
    Flow::new()
        .step("know_diet")
        .posture("Establish what they can eat before proposing anywhere.")
        .ground("Known diet: {user:diet}")
        .done(Guard::captured(["user:diet"]))
        .step("choose_venue")
        .after("know_diet")
        .done(Guard::captured(["user:venue"]))
        .step("book")
        .after("choose_venue")
        .allow(["book_table"])
        .done(Guard::called_ok("book_table"))
        .never("book_table")
        .until(Guard::captured(["user:diet", "user:venue"]))
        .build()
        .expect("the flow above is structurally valid")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = MemoryEngine::in_memory(UserId::new("usr_72ab"));
    engine.compile_index().await?;
    let session = Arc::new(engine.begin_session(SessionId::new("ses_dinner")));

    // ── The session an application actually writes ──────────────────────────
    //
    // `with_memory_slots` adds two tools and one turn extractor. Everything
    // below it is ordinary phase-machine and flow code that knows nothing about
    // memory.
    let _live = Live::builder()
        .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
        .voice(Voice::Kore)
        .instruction("You are B, a companion. Be brief and concrete.")
        .with_memory_slots(session.clone(), slots())
        .with_tools(T::google_search())
        .govern(flow_spec())
        // Gathering: only entered while something is still unknown. For a
        // returning user memory has already filled these, so the phase is
        // skipped entirely and they are never asked twice.
        .phase("gather")
        .instruction("Find out what you still need, one question at a time.")
        .needs(&["user:diet", "user:venue"])
        .transition("suggest", |s: &State| {
            s.contains("user:diet") && s.contains("user:venue")
        })
        .done()
        // Suggesting: a hard gate. The phase cannot be entered until the facts
        // exist, whether they came from this conversation or from memory.
        .phase("suggest")
        .requires(&["user:diet"])
        .dynamic_instruction(|s: &State| {
            let diet: String = s
                .get("user:diet")
                .unwrap_or_else(|| "no restrictions".into());
            let venue: String = s.get("user:venue").unwrap_or_else(|| "anywhere".into());
            format!("Suggest somewhere for dinner. Diet: {diet}. Venue preference: {venue}.")
        })
        .done()
        .initial_phase("gather")
        // Phase-wide prompt composition reads the same slots, so every phase
        // instruction carries what memory knows without repeating itself.
        .phase_defaults(|p| {
            p.with_state(&["user:diet", "user:partner"]).when(
                |s: &State| s.contains("user:diet"),
                "You already know their dietary preference. Do not ask again.",
            )
        });

    // ── What that buys, demonstrated offline ────────────────────────────────
    //
    // Drive one turn through the same extractor the Live pipeline drives, and
    // watch the slots fill. A second monitor over the same flow stands in for
    // the one the governed session owns, so the effect on the flow is visible
    // without connecting.
    let extractor = MemoryTurnExtractor::new(session.clone()).slots(slots());
    let mut monitor = FlowMonitor::try_new(flow_spec(), Enforcement::Enforce)
        .map_err(|e| format!("flow does not compile: {e:?}"))?;
    let state = State::new();

    println!("before: user:diet = {:?}", state.get::<String>("user:diet"));
    println!("        gather phase needs it, so B would ask.");
    println!(
        "        flow is waiting on {:?}; book_table is {}\n",
        monitor.explain(&state).active,
        match monitor.admits_tool("book_table", &state) {
            Ok(()) => "admitted".to_string(),
            Err(reason) => format!("blocked — {reason}"),
        }
    );

    extractor
        .extract_with_state(
            &[TranscriptTurn {
                turn_number: 1,
                user: "I am pescatarian and I prefer quiet places".into(),
                model: String::new(),
                tool_calls: Vec::new(),
                timestamp: std::time::Instant::now(),
            }],
            &state,
        )
        .await?;

    // The turn boundary is where the control lane advances the flow.
    monitor.on_turn(&state);

    println!(
        "after:  user:diet  = {:?}",
        state.get::<String>("user:diet")
    );
    println!(
        "        user:venue = {:?}",
        state.get::<String>("user:venue")
    );
    println!("        both `needs` satisfied → the machine advances to `suggest`.");
    println!(
        "        flow advanced to {:?}; book_table is {}\n",
        monitor.explain(&state).active,
        match monitor.admits_tool("book_table", &state) {
            Ok(()) => "admitted".to_string(),
            Err(reason) => format!("blocked — {reason}"),
        }
    );

    // Next session, the same slots fill from durable memory before a word is
    // spoken — which is the whole point.
    session.finish().await?;
    engine.compile_index().await?;

    let returning = Arc::new(engine.begin_session(SessionId::new("ses_next_week")));
    let returning_state = State::new();
    MemoryTurnExtractor::new(returning.clone())
        .slots(slots())
        .extract_with_state(
            &[TranscriptTurn {
                turn_number: 1,
                user: "hey, where should we eat tonight".into(),
                model: String::new(),
                tool_calls: Vec::new(),
                timestamp: std::time::Instant::now(),
            }],
            &returning_state,
        )
        .await?;

    println!("next week, first turn:");
    println!(
        "        user:diet  = {:?}",
        returning_state.get::<String>("user:diet")
    );
    println!(
        "        user:venue = {:?}",
        returning_state.get::<String>("user:venue")
    );
    println!("        `gather` is skipped; B goes straight to suggesting.");

    // The same flow, over a session that has said nothing yet: the gate that was
    // shut a moment ago is already open, because the facts came back with the
    // user.
    let mut returning_monitor = FlowMonitor::try_new(flow_spec(), Enforcement::Enforce)
        .map_err(|e| format!("flow does not compile: {e:?}"))?;
    returning_monitor.on_turn(&returning_state);
    println!(
        "        flow already at {:?}; book_table is {}",
        returning_monitor.explain(&returning_state).active,
        match returning_monitor.admits_tool("book_table", &returning_state) {
            Ok(()) => "admitted".to_string(),
            Err(reason) => format!("blocked — {reason}"),
        }
    );

    Ok(())
}
