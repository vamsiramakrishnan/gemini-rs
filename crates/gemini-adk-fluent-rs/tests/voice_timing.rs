//! Per-stage voice timing, exercised offline against a scripted server.

use std::time::Duration;

use base64::Engine;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use gemini_adk_rs::flow::{Flow, Guard};
use gemini_adk_rs::live::LiveEvent;
use serde_json::json;

/// One open step, `ask`, that never completes on its own.
fn ask_flow() -> Flow {
    Flow::new()
        .step("ask")
        .allow(["lookup"])
        .done(Guard::is_true("never"))
        .build()
        .unwrap()
}

/// A session governed by [`ask_flow`], with its `lookup` tool registered.
fn governed() -> Live {
    Live::builder()
        .tools(T::simple("lookup", "Lookup", |_| async { Ok(json!({})) }))
        .govern(ask_flow())
}

#[tokio::test]
async fn a_slow_tool_cues_a_filler() {
    let run = ScriptedServer::new()
        .calls("lookup", json!({}))
        .says("Found it.")
        .play(
            Live::builder()
                // Slower than the settle window: the run must still wait
                // for the tool's answer.
                .tools(T::simple("lookup", "Slow lookup", |_| async {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    Ok(json!({ "found": true }))
                }))
                .govern(ask_flow())
                .stage_timing(
                    "ask",
                    VoiceTiming::new().filler_after(Duration::from_millis(50)),
                ),
        )
        .await
        .unwrap();

    let cue = run.events().iter().find_map(|e| match e {
        LiveEvent::FillerCue { tool, elapsed_ms } => Some((tool.clone(), *elapsed_ms)),
        _ => None,
    });
    let (tool, elapsed_ms) = cue.expect("a filler cue while the tool ran");
    assert_eq!(tool, "lookup");
    assert!(elapsed_ms >= 50, "{elapsed_ms}");
    assert_eq!(
        run.tool_responses()[0]["response"],
        json!({ "found": true }),
        "the cue does not disturb the tool"
    );
    run.disconnect().await;
}

#[tokio::test]
async fn a_fast_tool_cues_nothing() {
    let run = ScriptedServer::new()
        .calls("lookup", json!({}))
        .says("Found it.")
        .play(
            Live::builder()
                .tools(T::simple("lookup", "Fast lookup", |_| async {
                    Ok(json!({ "found": true }))
                }))
                .govern(ask_flow())
                .stage_timing(
                    "ask",
                    VoiceTiming::new().filler_after(Duration::from_secs(5)),
                ),
        )
        .await
        .unwrap();
    assert!(
        !run.events()
            .iter()
            .any(|e| matches!(e, LiveEvent::FillerCue { .. }))
    );
    run.disconnect().await;
}

#[tokio::test]
async fn silence_past_the_stage_threshold_reprompts_once() {
    let run = ScriptedServer::new()
        .says("What is your account number?")
        .settle_after(Duration::from_millis(1200))
        .play(governed().stage_timing(
            "ask",
            VoiceTiming::new().reprompt_with(
                Duration::from_millis(300),
                "Ask for the account number again.",
            ),
        ))
        .await
        .unwrap();

    let reprompts = run
        .events()
        .iter()
        .filter(|e| matches!(e, LiveEvent::Reprompted { .. }))
        .count();
    assert_eq!(reprompts, 1, "one reprompt per silence");
    let nudge = run
        .sent()
        .into_iter()
        .find(|m| m.to_string().contains("Ask for the account number again."))
        .expect("the reprompt reached the model");
    assert_eq!(nudge["clientContent"]["turnComplete"], true);
    run.disconnect().await;
}

#[tokio::test]
async fn a_stage_that_holds_the_floor_silences_the_mic_while_the_model_speaks() {
    // The model is mid-turn: audio has arrived, the turn is not complete.
    let pcm = base64::engine::general_purpose::STANDARD.encode([1u8; 64]);
    let run = ScriptedServer::new()
        .frame(json!({
            "serverContent": { "modelTurn": { "parts": [
                { "inlineData": { "mimeType": "audio/pcm;rate=24000", "data": pcm } }
            ] } }
        }))
        .play(governed().stage_timing("ask", VoiceTiming::new().uninterruptible()))
        .await
        .unwrap();
    assert_eq!(
        run.state().session().get::<bool>("is_model_speaking"),
        Some(true)
    );

    run.handle().send_audio(vec![0x55u8; 320]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let audio = run
        .sent()
        .into_iter()
        .rev()
        .find_map(|m| {
            m.pointer("/realtimeInput/audio/data")
                .and_then(|d| d.as_str().map(str::to_owned))
        })
        .expect("the audio chunk was still sent, keeping the stream's cadence");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(audio)
        .unwrap();
    assert_eq!(bytes.len(), 320);
    assert!(
        bytes.iter().all(|b| *b == 0),
        "the user's audio was replaced by silence"
    );
    run.disconnect().await;
}
