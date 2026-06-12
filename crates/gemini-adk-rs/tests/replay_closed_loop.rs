//! Closed-loop record/replay integration test — the Milestone 7 keystone.
//!
//! 1. **Record**: a scripted session (handcrafted server frames driven through
//!    the gated `ReplayTransport` — a scripted mock, like `MockTransport` but
//!    race-free) runs through the REAL L1 three-lane processor with a
//!    `RecordingCodec` tap installed via `SessionConfig::record_wire`. The
//!    session covers: setup handshake, a text exchange, a tool call that the
//!    dispatcher executes (writing state and sending a tool response), and
//!    turn completes. A user text send is recorded as an outbound frame.
//! 2. **Replay**: the recorded wire log is fed back through
//!    `replay_session(..)` — the same real processor, a fresh `State`, the
//!    same tool implementations — and the resulting `LiveEvent`s, final
//!    state, mutation journal, and regenerated outbound frames are compared
//!    against the original run.
//!
//! ## Normalizations (each one documented honestly)
//!
//! - **Cross-lane event interleaving is not asserted.** The processor is a
//!   three-lane concurrent architecture (fast / control / telemetry); the
//!   relative ordering of, say, a fast-lane `TextDelta` and a control-lane
//!   `ToolExecution` is scheduler-dependent *in production too*. We assert
//!   the full ordered sequence **per lane** instead.
//! - **Periodic events (`Telemetry`, `TurnMetrics`) are excluded** — they are
//!   timer-driven, not wire-driven.
//! - **Wall-clock-derived state keys are excluded** from the final-state and
//!   journal diffs (`session:elapsed_ms`, `session:silence_ms`,
//!   `session:remaining_budget_ms`): their values are timestamps/durations of
//!   the run itself, not conversation state.
//! - **Mutation `sequence`/`timestamp` fields are not compared** — sequences
//!   depend on cross-lane write interleaving and timestamps on wall clock. We
//!   compare the per-key **final values** plus the presence/origin of the
//!   tool-driven write.
//! - **User-originated outbound frames are not regenerated.** Replay re-sends
//!   only processor-originated frames (setup, tool responses); the recorded
//!   user text frame exists in the log for audit but is asserted absent from
//!   the replayed outbound.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use gemini_adk_rs::live::replay::{collect_events_until_idle, replay_session};
use gemini_adk_rs::live::{LiveEvent, LiveSessionBuilder};
use gemini_adk_rs::state::{MemoryJournalSink, State, StateMutation, StateMutationOrigin};
use gemini_adk_rs::tool::{SimpleTool, ToolDispatcher};
use gemini_genai_rs::prelude::{
    GeminiModel, MemoryWireRecorder, SessionConfig, WireDirection, WireEntry,
};

/// Handcrafted "server" script: setup handshake, a greeting turn, a tool
/// call, and the post-tool answer turn.
fn server_script() -> Vec<WireEntry> {
    let frames: Vec<&[u8]> = vec![
        br#"{"setupComplete":{}}"#,
        br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hello! Ask me about the weather."}]},"turnComplete":true}}"#,
        br#"{"toolCall":{"functionCalls":[{"name":"get_weather","args":{"city":"London"},"id":"call-1"}]}}"#,
        br#"{"serverContent":{"modelTurn":{"parts":[{"text":"It is 22C in London."}]},"turnComplete":true}}"#,
    ];
    frames
        .into_iter()
        .enumerate()
        .map(|(i, payload)| WireEntry {
            seq: (i + 1) as u64,
            dir: WireDirection::Inbound,
            ts_ms: 1_718_000_000_000 + i as u64,
            payload: payload.to_vec(),
        })
        .collect()
}

/// The deterministic local tool used in both runs: returns a fixed result and
/// writes `app:last_city` into session state.
fn weather_dispatcher(state: State) -> ToolDispatcher {
    let mut dispatcher = ToolDispatcher::new();
    dispatcher.register(SimpleTool::new(
        "get_weather",
        "Get weather for a city",
        Some(json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        })),
        move |args| {
            let state = state.clone();
            async move {
                let city = args["city"].as_str().unwrap_or("Unknown").to_string();
                let _ = state.set("app:last_city", city.clone());
                Ok(json!({"temp": 22, "city": city}))
            }
        },
    ));
    dispatcher
}

struct RunOutcome {
    events: Vec<LiveEvent>,
    final_state: BTreeMap<String, Value>,
    journal: Vec<StateMutation>,
    outbound: Vec<Vec<u8>>,
}

/// Run one session over the given wire entries through the real processor.
/// `recorder`: tap the wire (record run). `send_user_text`: drive a user text
/// exchange after the scripted frames have drained (record run only).
async fn run_session(
    entries: &[WireEntry],
    recorder: Option<Arc<MemoryWireRecorder>>,
    send_user_text: bool,
) -> RunOutcome {
    let state = State::new();
    let sink = Arc::new(MemoryJournalSink::new());
    state.set_journal_sink(sink.clone());

    let mut config = SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
    if let Some(rec) = recorder {
        config = config.record_wire(rec);
    }

    let builder = LiveSessionBuilder::new(config.clone())
        .dispatcher(weather_dispatcher(state.clone()))
        .with_state(state.clone());

    let replay = replay_session(config, builder, entries)
        .await
        .expect("session should connect over the replay transport");

    let mut rx = replay.handle().events();
    replay.release();
    replay.drained().await;

    let mut events =
        collect_events_until_idle(&mut rx, Duration::from_millis(300), Duration::from_secs(10))
            .await;

    if send_user_text {
        // A recorded user-originated frame. Sent after the scripted inbound
        // frames so the log tail is deterministic.
        replay
            .handle()
            .send_text("What's the weather in London?")
            .await
            .expect("send_text over replay transport");
        events.extend(
            collect_events_until_idle(&mut rx, Duration::from_millis(200), Duration::from_secs(5))
                .await,
        );
    }

    let outcome = RunOutcome {
        events,
        final_state: state.to_hashmap().into_iter().collect(),
        journal: sink.entries(),
        outbound: replay.outbound_frames(),
    };
    replay.disconnect().await.expect("disconnect");
    outcome
}

/// State keys whose values derive from the wall clock of the run itself.
fn is_volatile_key(key: &str) -> bool {
    matches!(
        key,
        "session:elapsed_ms" | "session:silence_ms" | "session:remaining_budget_ms"
    )
}

/// Render an event as a stable comparison string, or `None` to exclude it.
fn event_repr(event: &LiveEvent) -> Option<String> {
    match event {
        LiveEvent::Telemetry(_) | LiveEvent::TurnMetrics { .. } => None, // timer-driven
        other => Some(format!("{other:?}")),
    }
}

/// Split events into the fast-lane and control-lane ordered subsequences.
fn split_lanes(events: &[LiveEvent]) -> (Vec<String>, Vec<String>) {
    let mut fast = Vec::new();
    let mut control = Vec::new();
    for event in events {
        let Some(repr) = event_repr(event) else {
            continue;
        };
        match event {
            LiveEvent::Audio(_)
            | LiveEvent::TextDelta(_)
            | LiveEvent::TextComplete(_)
            | LiveEvent::InputTranscript { .. }
            | LiveEvent::OutputTranscript { .. }
            | LiveEvent::Thought(_)
            | LiveEvent::VadStart
            | LiveEvent::VadEnd => fast.push(repr),
            _ => control.push(repr),
        }
    }
    (fast, control)
}

/// Per-key final value from a journal (None = removed), excluding volatile keys.
fn journal_final_values(journal: &[StateMutation]) -> BTreeMap<String, Option<Value>> {
    let mut map = BTreeMap::new();
    for m in journal {
        if !is_volatile_key(&m.key) {
            map.insert(m.key.clone(), m.new.clone());
        }
    }
    map
}

fn normalized_state(state: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    state
        .iter()
        .filter(|(k, _)| !is_volatile_key(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[tokio::test]
async fn closed_loop_record_then_replay_through_real_processor() {
    // ── Record ──────────────────────────────────────────────────────────
    let recorder = Arc::new(MemoryWireRecorder::new());
    let original = run_session(&server_script(), Some(recorder.clone()), true).await;

    let wire_log = recorder.entries();
    // The log captured both directions: outbound setup + user text + tool
    // response, inbound setupComplete + 3 server frames.
    let outbound_count = wire_log
        .iter()
        .filter(|e| e.dir == WireDirection::Outbound)
        .count();
    let inbound_count = wire_log
        .iter()
        .filter(|e| e.dir == WireDirection::Inbound)
        .count();
    assert_eq!(inbound_count, 4, "all scripted server frames recorded");
    assert_eq!(
        outbound_count,
        3,
        "setup + tool response + user text recorded, got: {:?}",
        wire_log
            .iter()
            .filter(|e| e.dir == WireDirection::Outbound)
            .map(|e| String::from_utf8_lossy(&e.payload).into_owned())
            .collect::<Vec<_>>()
    );

    // ── Replay: same log, same tools, fresh state, real processor ───────
    let replayed = run_session(&wire_log, None, false).await;

    // ── 1. LiveEvents match per lane (see module docs for why per-lane) ──
    let (orig_fast, orig_ctrl) = split_lanes(&original.events);
    let (replay_fast, replay_ctrl) = split_lanes(&replayed.events);
    assert_eq!(orig_fast, replay_fast, "fast-lane event sequence differs");
    assert_eq!(
        orig_ctrl, replay_ctrl,
        "control-lane event sequence differs"
    );

    // Sanity: the sequences actually exercised text, tool, and turn events.
    assert!(orig_fast.iter().any(|e| e.contains("It is 22C in London")));
    assert!(orig_ctrl.iter().any(|e| e.contains("ToolExecution")));
    assert_eq!(
        orig_ctrl.iter().filter(|e| *e == "TurnComplete").count(),
        2,
        "two scripted turn completes"
    );

    // ── 2. Final state matches (minus wall-clock keys) ───────────────────
    assert_eq!(
        normalized_state(&original.final_state),
        normalized_state(&replayed.final_state),
        "final state diverged"
    );
    assert_eq!(
        original.final_state.get("app:last_city"),
        Some(&json!("London")),
        "tool wrote state in the original run"
    );
    assert_eq!(
        replayed.final_state.get("app:last_city"),
        Some(&json!("London")),
        "tool re-executed and wrote state on replay"
    );

    // ── 3. Mutation journals agree on per-key final values ───────────────
    assert_eq!(
        journal_final_values(&original.journal),
        journal_final_values(&replayed.journal),
        "mutation journals diverged"
    );
    for journal in [&original.journal, &replayed.journal] {
        assert!(
            journal
                .iter()
                .any(|m| m.key == "app:last_city" && m.origin == StateMutationOrigin::Set),
            "journal records the tool-driven write"
        );
    }

    // ── 4. Outbound frames ────────────────────────────────────────────────
    // Setup re-encodes byte-identically from the same config.
    let recorded_outbound: Vec<&WireEntry> = wire_log
        .iter()
        .filter(|e| e.dir == WireDirection::Outbound)
        .collect();
    assert_eq!(
        recorded_outbound[0].payload, replayed.outbound[0],
        "setup frame differs between record and replay"
    );
    // The dispatcher regenerated a byte-identical tool response.
    let find_tool_response = |frames: &[Vec<u8>]| -> Option<Vec<u8>> {
        frames
            .iter()
            .find(|f| String::from_utf8_lossy(f).contains("toolResponse"))
            .cloned()
    };
    let recorded_tool_response = find_tool_response(
        &recorded_outbound
            .iter()
            .map(|e| e.payload.clone())
            .collect::<Vec<_>>(),
    )
    .expect("record run sent a tool response");
    let replayed_tool_response =
        find_tool_response(&replayed.outbound).expect("replay sent a tool response");
    assert_eq!(
        recorded_tool_response, replayed_tool_response,
        "tool response differs between record and replay"
    );
    // User-originated text is recorded but, by design, not regenerated.
    assert!(recorded_outbound
        .iter()
        .any(|e| String::from_utf8_lossy(&e.payload).contains("What's the weather")));
    assert!(!replayed
        .outbound
        .iter()
        .any(|f| String::from_utf8_lossy(f).contains("What's the weather")));
}
