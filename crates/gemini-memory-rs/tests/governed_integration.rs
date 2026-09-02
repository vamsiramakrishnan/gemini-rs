//! The claim under test: a slot filled from memory is read by the platform's
//! existing gates — nothing memory-specific required on the application side.
//!
//! Everywhere else that claim is asserted at one remove: a test writes a slot
//! and checks the slot. That proves the extractor writes a key; it does not
//! prove a `PhaseMachine` or a `Flow` ever reads it. Here a real machine and a
//! real monitor are driven, so a key convention that no gate can see fails
//! loudly instead of passing quietly.
//!
//! Deterministic — no API key, no network.

use std::sync::Arc;
use std::time::Instant;

use gemini_adk_rs::flow::{Enforcement, Flow, FlowMonitor, Guard};
use gemini_adk_rs::live::extractor::TurnExtractor;
use gemini_adk_rs::live::phase::{Phase, PhaseMachine, Transition};
use gemini_adk_rs::live::transcript::TranscriptTurn;
use gemini_adk_rs::state::State;
use gemini_memory_rs::core::{SessionId, UserId};
use gemini_memory_rs::engine::{MemoryEngine, MemorySession};
use gemini_memory_rs::runtime::{
    MANAGE_TOOL, MEMORY_TOOLS, MemorySlot, MemoryTurnExtractor, RECALL_TOOL,
};

/// The slot every case here gates on.
const DIET: &str = "user:diet";
const VENUE: &str = "user:venue";

fn engine() -> Arc<MemoryEngine> {
    Arc::new(MemoryEngine::in_memory(UserId::new("usr_governed")))
}

fn session(engine: &MemoryEngine) -> Arc<MemorySession> {
    Arc::new(engine.begin_session(SessionId::new("ses_1")))
}

fn slots() -> Vec<MemorySlot> {
    vec![
        MemorySlot::new("dietary_identity", DIET),
        MemorySlot::new("venue_preference", VENUE),
    ]
}

fn turn(number: u32, user: &str) -> TranscriptTurn {
    TranscriptTurn {
        turn_number: number,
        user: user.to_string(),
        model: String::new(),
        tool_calls: Vec::new(),
        timestamp: Instant::now(),
    }
}

// ─── the phase machine actually reads the slot ──────────────────────────────

/// `gather → suggest` where `suggest` hard-`requires` the slot. Before memory
/// fills it the transition is refused even though its guard fires; after, the
/// same evaluation admits it.
///
/// `evaluate` is the pure decision the control lane makes at every turn
/// boundary, so this is the gate itself and not a restatement of it.
#[tokio::test]
async fn a_requires_gate_opens_only_once_memory_fills_the_slot() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone()).slots(slots());

    let mut machine = PhaseMachine::new("gather");
    let mut gather = Phase::new("gather", "Find out what you still need.");
    gather.needs = vec![DIET.into()];
    // Guard fires unconditionally: the only thing standing between the two
    // phases is `suggest`'s requirement, which is what we want to observe.
    gather.transitions = vec![Transition {
        target: "suggest".into(),
        guard: Arc::new(|_: &State| true),
        description: None,
    }];
    let mut suggest = Phase::new("suggest", "Suggest somewhere for dinner.");
    suggest.requires = vec![DIET.into()];
    suggest.terminal = true;
    machine.add_phase(gather);
    machine.add_phase(suggest);

    let state = State::new();
    assert_eq!(
        machine.evaluate(&state).map(|(t, _)| t),
        None,
        "`suggest` requires {DIET}; with it unset the machine must stay in `gather`"
    );

    extractor
        .extract_with_state(&[turn(1, "I am pescatarian")], &state)
        .await
        .unwrap();

    assert_eq!(
        machine.evaluate(&state).map(|(t, _)| t),
        Some("suggest"),
        "a memory-filled slot must satisfy `requires` exactly as a user-filled one would; \
         state holds {:?}",
        state.get::<String>(DIET)
    );
}

/// `needs` is the softer half of the same contract: the runtime reports what is
/// still missing so the model knows what to ask for. A memory-filled slot must
/// drop off that list, or a returning user is asked twice.
#[tokio::test]
async fn a_memory_filled_slot_drops_off_the_still_needed_list() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone()).slots(slots());

    let mut machine = PhaseMachine::new("gather");
    let mut gather = Phase::new("gather", "Find out what you still need.");
    gather.needs = vec![DIET.into(), VENUE.into()];
    machine.add_phase(gather);

    let state = State::new();
    let before = machine.describe_navigation(&state);
    assert!(
        before.contains(DIET),
        "with nothing known, {DIET} must be reported as still needed:\n{before}"
    );

    extractor
        .extract_with_state(&[turn(1, "I am pescatarian")], &state)
        .await
        .unwrap();

    let after = machine.describe_navigation(&state);
    let still_needed = after
        .lines()
        .find(|l| l.starts_with("Still needed:"))
        .unwrap_or("");
    assert!(
        !still_needed.contains(DIET),
        "memory knows the diet, yet the model is still told to ask for it:\n{after}"
    );
}

// ─── a governed flow actually reads the slot ────────────────────────────────

/// A `Flow` whose step completes on `Guard::captured([DIET])` and whose next
/// step gates a tool behind it. Before memory fills the slot the tool is denied;
/// after, it is admitted — through `FlowMonitor::admits_tool`, the same call the
/// control lane's tool gate makes.
#[tokio::test]
async fn a_flow_guard_admits_a_tool_only_once_memory_fills_the_slot() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone()).slots(slots());

    let flow = Flow::new()
        .step("know_diet")
        .done(Guard::captured([DIET]))
        .step("book")
        .after("know_diet")
        .allow(["book_table"])
        .done(Guard::called_ok("book_table"))
        .never("book_table")
        .until(Guard::is_set(DIET))
        .build()
        .expect("flow is structurally valid");

    let monitor = FlowMonitor::try_new(flow, Enforcement::Enforce).expect("flow compiles");

    let state = State::new();
    assert!(
        monitor.admits_tool("book_table", &state).is_err(),
        "booking must be blocked until the diet is known"
    );

    extractor
        .extract_with_state(&[turn(1, "I am pescatarian")], &state)
        .await
        .unwrap();

    assert!(
        monitor.eval(&Guard::captured([DIET]), &state),
        "`Guard::captured` must see the memory-filled slot; state holds {:?}",
        state.get::<String>(DIET)
    );
    assert_eq!(
        monitor.admits_tool("book_table", &state),
        Ok(()),
        "with the diet remembered, the flow must admit the booking tool"
    );
}

/// The completed step must show up in the monitor's own account of itself, so
/// `explain` tells an operator the truth about what memory unlocked.
#[tokio::test]
async fn the_flow_explains_what_memory_unlocked() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone()).slots(slots());

    let flow = Flow::new()
        .step("know_diet")
        .done(Guard::captured([DIET]))
        .step("book")
        .after("know_diet")
        .allow(["book_table"])
        .done(Guard::called_ok("book_table"))
        .require(["book"])
        .build()
        .expect("flow is structurally valid");

    let mut monitor = FlowMonitor::try_new(flow, Enforcement::Enforce).expect("flow compiles");

    let state = State::new();
    assert_eq!(
        monitor.explain(&state).active,
        ["know_diet"],
        "the flow starts waiting on the fact memory supplies"
    );

    extractor
        .extract_with_state(&[turn(1, "I am pescatarian")], &state)
        .await
        .unwrap();
    // `on_turn` is what the control lane calls at each turn boundary to latch
    // newly-satisfied `done` guards into the marking.
    monitor.on_turn(&state);

    assert_eq!(
        monitor.explain(&state).active,
        ["book"],
        "the remembered diet must complete `know_diet` and advance the flow"
    );
    assert!(
        !monitor.is_complete(),
        "`book` still requires its tool call"
    );
}

// ─── the durable half: next session, before a word is spoken ────────────────

/// The whole point of memory: on a later session the gates open on the first
/// turn, from durable storage, without the user restating anything.
#[tokio::test]
async fn a_returning_user_passes_the_gate_on_their_first_turn() {
    let engine = engine();

    let first = session(&engine);
    MemoryTurnExtractor::new(first.clone())
        .slots(slots())
        .extract_with_state(&[turn(1, "I am pescatarian")], &State::new())
        .await
        .unwrap();
    first.finish().await.unwrap();
    engine.compile_index().await.unwrap();

    let returning = Arc::new(engine.begin_session(SessionId::new("ses_next_week")));
    let state = State::new();
    MemoryTurnExtractor::new(returning)
        .slots(slots())
        .extract_with_state(&[turn(1, "hey, where should we eat tonight")], &state)
        .await
        .unwrap();

    let mut machine = PhaseMachine::new("gather");
    let mut gather = Phase::new("gather", "Find out what you still need.");
    gather.transitions = vec![Transition {
        target: "suggest".into(),
        guard: Arc::new(|_: &State| true),
        description: None,
    }];
    let mut suggest = Phase::new("suggest", "Suggest somewhere for dinner.");
    suggest.requires = vec![DIET.into()];
    suggest.terminal = true;
    machine.add_phase(gather);
    machine.add_phase(suggest);

    assert_eq!(
        machine.evaluate(&state).map(|(t, _)| t),
        Some("suggest"),
        "a fact from a previous session must open the gate on turn one; state holds {:?}",
        state.get::<String>(DIET)
    );
}

// ─── memory's own tools under a governing flow ──────────────────────────────

/// A step's `allow` list excludes by omission. Memory's tools are not part of
/// any step's repertoire, so without `ambient` a flow that whitelists its own
/// tools switches recall off for the duration — and silently, because the model
/// simply stops being told what it knows.
///
/// This drives the same `admits_tool` the control lane's tool gate calls
/// (`control_plane/tool_handler.rs`), so it is the gate itself, not a
/// restatement of it.
#[tokio::test]
async fn a_whitelisting_step_blocks_memory_tools_without_ambient() {
    let flow = Flow::new()
        .step("book")
        .allow(["book_table"])
        .done(Guard::called_ok("book_table"))
        .build()
        .expect("flow is structurally valid");
    let monitor = FlowMonitor::try_new(flow, Enforcement::Enforce).expect("flow compiles");

    for tool in MEMORY_TOOLS {
        assert!(
            monitor.admits_tool(tool, &State::new()).is_err(),
            "this is the friction `ambient` exists to remove: '{tool}' is denied \
             by a whitelist that was never about memory"
        );
    }
}

/// The same flow, with memory's tools declared ambient — which is what
/// `Live::with_memory` registers on the application's behalf.
#[tokio::test]
async fn ambient_memory_tools_survive_a_whitelisting_step() {
    let flow = Flow::new()
        .ambient(MEMORY_TOOLS)
        .step("book")
        .allow(["book_table"])
        .done(Guard::called_ok("book_table"))
        .build()
        .expect("flow is structurally valid");
    let monitor = FlowMonitor::try_new(flow, Enforcement::Enforce).expect("flow compiles");

    let state = State::new();
    for tool in MEMORY_TOOLS {
        assert_eq!(
            monitor.admits_tool(tool, &state),
            Ok(()),
            "'{tool}' must stay available while a step whitelists its own tools"
        );
    }
    assert!(
        monitor.admits_tool("some_other_tool", &state).is_err(),
        "ambient must not widen the whitelist for anything else"
    );
}

/// Ambient is an exemption from exclusion-by-omission, not a licence. A flow
/// that deliberately forbids memory writes until identity is verified must
/// still be obeyed — otherwise `ambient` would be a hole in the governance
/// model rather than a correction to it.
#[tokio::test]
async fn a_flow_can_still_gate_memory_writes_deliberately() {
    let flow = Flow::new()
        .ambient(MEMORY_TOOLS)
        .step("verify")
        .done(Guard::is_true("identity_verified"))
        .never(MANAGE_TOOL)
        .until(Guard::is_true("identity_verified"))
        .build()
        .expect("flow is structurally valid");
    let monitor = FlowMonitor::try_new(flow, Enforcement::Enforce).expect("flow compiles");

    let state = State::new();
    assert!(
        monitor.admits_tool(MANAGE_TOOL, &state).is_err(),
        "an unverified caller must not be able to write to memory"
    );
    assert_eq!(
        monitor.admits_tool(RECALL_TOOL, &state),
        Ok(()),
        "reading is not what was forbidden"
    );

    let _ = state.set("identity_verified", true);
    assert_eq!(
        monitor.admits_tool(MANAGE_TOOL, &state),
        Ok(()),
        "once verified, the constraint stops binding"
    );
}

/// `with_memory` must register the tools itself. Asserted through the builder
/// rather than by rebuilding the flow by hand, so deleting that line fails here
/// instead of passing quietly.
#[cfg(feature = "fluent")]
#[test]
fn with_memory_registers_its_tools_as_ambient() {
    use gemini_adk_fluent_rs::live::Live;
    use gemini_memory_rs::runtime::LiveMemoryExt;

    let engine = engine();
    let builder = Live::builder().with_memory(session(&engine));
    for tool in MEMORY_TOOLS {
        assert!(
            builder.ambient_tool_names().iter().any(|t| t == tool),
            "`with_memory` must register '{tool}' so the application does not \
             have to widen every step of its flow"
        );
    }
}

// ─── the key convention itself ──────────────────────────────────────────────

/// A slot key must satisfy all three surfaces at once, or a developer following
/// the platform's documented prefix conventions gets nothing back — silently.
///
/// The gates themselves are convention-agnostic — they treat a slot key as an
/// opaque string through `contains`, so a dotted `user.diet` satisfies `needs`,
/// `requires` and `Guard::is_set` just as well. What a dotted key does *not* do
/// is compose with `state.user()`, so a developer following the platform's
/// documented prefix scopes reads `None` and is given no hint why. That is the
/// whole reason these slots carry the colon.
///
/// `derived:` would be the wrong home despite being semantically apt: its
/// fallback lives only in `get`/`with`, and `contains` — which backs `needs`,
/// `requires` and `Guard::is_set` — has none. A `derived:` slot would be
/// invisible to exactly the gates memory exists to satisfy.
#[tokio::test]
async fn a_slot_key_is_visible_to_every_surface_that_reads_state() {
    let engine = engine();
    let session = session(&engine);

    let state = State::new();
    MemoryTurnExtractor::new(session)
        .slots(slots())
        .extract_with_state(&[turn(1, "I am pescatarian")], &state)
        .await
        .unwrap();

    assert!(
        state.contains(DIET),
        "`contains` backs needs / requires / Guard::is_set — a key it cannot see is invisible to every gate"
    );
    assert_eq!(
        state.get::<String>(DIET).as_deref(),
        Some("pescatarian"),
        "direct read"
    );
    assert_eq!(
        state.user().get::<String>("diet").as_deref(),
        Some("pescatarian"),
        "the slot must compose with the platform's `user:` prefix scope"
    );
}
