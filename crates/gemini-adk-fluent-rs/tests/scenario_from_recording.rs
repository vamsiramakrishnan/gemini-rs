//! An incident becomes a regression test: record a governed session, extract
//! a scenario from its mutation journal, and run it against the spec.

use std::sync::Arc;

use gemini_adk_fluent_rs::compose::tools::ToolComposite;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::simulation::Scenario;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use gemini_adk_rs::state::MemoryJournalSink;
use serde_json::json;

fn booking(gate: Guard) -> CompiledConversation {
    Conversation::new("booking")
        .stage("collect")
        .collect(["party_size"])
        .stage("book")
        .after("collect")
        .allow(["confirm"])
        .commit("book_table", gate)
        .complete_when(Guard::called_ok("book_table"))
        .stage("end")
        .after("book")
        .terminal()
        .compile()
        .unwrap()
}

/// A tool that writes `key = value` into the session state.
fn setter(
    name: &'static str,
    key: &'static str,
    value: serde_json::Value,
    state: &State,
) -> ToolComposite {
    let state = state.clone();
    T::simple(name, name, move |_| {
        let state = state.clone();
        let value = value.clone();
        async move {
            let _ = state.set(key, value);
            Ok(json!({ "ok": true }))
        }
    })
}

#[tokio::test]
async fn a_recorded_session_becomes_a_scenario_that_catches_a_regression() {
    let journal = Arc::new(MemoryJournalSink::new());
    let state = State::new().with_journal_sink(journal.clone());
    let convo = booking(Guard::is_true("confirmed"));

    let run = ScriptedServer::new()
        .calls("set_party", json!({ "size": 4 }))
        .says("Four people. Shall I book?")
        .calls("confirm", json!({}))
        .says("Confirmed.")
        .calls("book_table", json!({}))
        .says("Booked.")
        .play(
            Live::builder()
                .state(state.clone())
                .tools(setter("set_party", "party_size", json!(4), &state))
                .tools(setter("confirm", "confirmed", json!(true), &state))
                .tools(setter("book_table", "booked", json!(true), &state))
                .converse(&convo),
        )
        .await
        .unwrap();
    run.disconnect().await;

    let scenario = Scenario::from_journal("booking-incident", &journal.entries());
    let json = serde_json::to_string_pretty(&scenario).unwrap();
    assert!(json.contains("\"tool_result\""), "{json}");
    assert!(
        json.contains("\"expect_allowed\": \"book_table\""),
        "{json}"
    );

    // The spec the session ran under reproduces it.
    scenario
        .run(&convo, Enforcement::Enforce)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{json}"));

    // A change that would have blocked the booking is caught.
    let stricter = booking(Guard::all(vec![
        Guard::is_true("confirmed"),
        Guard::is_true("manager_approved"),
    ]));
    let err = scenario
        .run(&stricter, Enforcement::Enforce)
        .await
        .expect_err("the stricter spec diverges from the recording");
    assert!(err.contains("book_table"), "{err}");
}
