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
