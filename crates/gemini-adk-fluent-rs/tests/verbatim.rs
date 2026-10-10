//! A verbatim stage completes only when the model's words match the text.

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use gemini_adk_rs::live::LiveEvent;

const TERMS: &str = "Calls may be recorded for quality and training purposes.";

fn disclosure() -> CompiledConversation {
    Conversation::new("disclosure")
        .stage("terms")
        .verbatim(TERMS)
        .stage("help")
        .after("terms")
        .terminal()
        .compile()
        .unwrap()
}

#[tokio::test]
async fn a_paraphrase_keeps_the_stage_open_and_the_exact_text_completes_it() {
    let run = ScriptedServer::new()
        .speaks("We might record this call.")
        .turn_complete()
        .speaks(TERMS)
        .turn_complete()
        .play(Live::builder().converse(&disclosure()))
        .await
        .unwrap();

    let checks: Vec<bool> = run
        .events()
        .iter()
        .filter_map(|e| match e {
            LiveEvent::VerbatimChecked { step, passed, .. } if step == "terms" => Some(*passed),
            _ => None,
        })
        .collect();
    assert_eq!(checks, [false, true]);

    let done: Vec<String> = run.state().get("flow:done").unwrap_or_default();
    assert!(done.contains(&"terms".to_string()), "{done:?}");
    run.disconnect().await;
}

#[tokio::test]
async fn a_paraphrase_alone_never_completes_the_stage() {
    let run = ScriptedServer::new()
        .speaks("We might record this call.")
        .turn_complete()
        .play(Live::builder().converse(&disclosure()))
        .await
        .unwrap();
    let done: Vec<String> = run.state().get("flow:done").unwrap_or_default();
    assert!(!done.contains(&"terms".to_string()), "{done:?}");
    let active: Vec<String> = run.state().get("flow:active").unwrap_or_default();
    assert_eq!(active, ["terms"]);
    run.disconnect().await;
}

#[test]
fn a_verbatim_stage_holds_the_floor_and_asks_for_the_exact_text() {
    let convo = disclosure();
    assert!(convo.timing_policies()["terms"].holds_floor());
    assert_eq!(convo.verbatim_policies()["terms"], TERMS);
    let spec = serde_json::to_value(convo.spec()).unwrap();
    assert_eq!(spec["stages"][0]["verbatim"], TERMS);
}

/// The first stage's posture, with its verbatim text, reaches the model before
/// the greeting. In live runs a greeting asked to "say the disclosure line" had
/// no line to say: the posture only arrived at the first turn boundary, and the
/// stage waited for words the model never had.
#[tokio::test]
async fn the_opening_stage_is_sent_before_the_greeting() {
    let run = ScriptedServer::new()
        .speaks(TERMS)
        .turn_complete()
        .play(
            Live::builder()
                .greeting("Say the disclosure line, then offer to help.")
                .converse(&disclosure()),
        )
        .await
        .unwrap();

    let sent = run.sent();
    let steers = |m: &serde_json::Value| {
        m["clientContent"]["turns"]
            .as_array()
            .is_some_and(|turns| turns.iter().any(|t| t.to_string().contains(TERMS)))
    };
    let opening = sent
        .iter()
        .position(steers)
        .expect("the opening steering carries the verbatim text");
    let greeting = sent
        .iter()
        .position(|m| m.to_string().contains("Say the disclosure line"))
        .expect("the greeting was sent");
    assert!(
        opening < greeting,
        "opening at {opening}, greeting at {greeting}"
    );

    // The first turn boundary does not send the same steering again.
    let repeats = sent.iter().filter(|m| steers(m)).count();
    assert_eq!(repeats, 1);
    run.disconnect().await;
}
