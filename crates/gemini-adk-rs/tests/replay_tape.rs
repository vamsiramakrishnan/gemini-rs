//! A replay answers out-of-band model calls from a tape, never the model.
//!
//! The first run drives a scripted session through the real processor with an
//! LLM extractor whose model is wrapped in `TapedLlm::recording`. The second
//! run replays the same frames with `TapedLlm::replaying`: no model exists,
//! yet the extractor produces the same state.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use gemini_adk_rs::live::LiveSessionBuilder;
use gemini_adk_rs::live::extractor::LlmExtractor;
use gemini_adk_rs::live::replay::{collect_events_until_idle, replay_session};
use gemini_adk_rs::llm::{BaseLlm, LlmResponse, MockLlm};
use gemini_adk_rs::tape::{MemoryTape, TapedLlm};
use gemini_genai_rs::prelude::{ModelId, SessionConfig};
use gemini_genai_rs::transport::{WireDirection, WireEntry};

fn frames() -> Vec<WireEntry> {
    let frames: Vec<&[u8]> = vec![
        br#"{"setupComplete":{}}"#,
        br#"{"serverContent":{"inputTranscription":{"text":"I am really happy today"}}}"#,
        br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Glad to hear it!"}]},"turnComplete":true}}"#,
    ];
    frames
        .into_iter()
        .enumerate()
        .map(|(i, payload)| WireEntry {
            seq: (i + 1) as u64,
            dir: WireDirection::Inbound,
            ts_ms: 1_718_000_000_000 + 100 * i as u64,
            payload: payload.to_vec(),
        })
        .collect()
}

/// Run the scripted frames with an extractor over `llm`; return the mood it
/// extracted into state.
async fn run(llm: Arc<dyn BaseLlm>) -> Option<String> {
    let config = SessionConfig::new("offline").model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);
    let builder = LiveSessionBuilder::new(config.clone()).extractor(Arc::new(LlmExtractor::new(
        "mood",
        llm,
        "Extract the user's mood as {\"mood\": string}.",
        3,
    )));
    let replay = replay_session(config, builder, &frames()).await.unwrap();
    let mut events = replay.handle().events();
    replay.release();
    replay.drained().await;
    collect_events_until_idle(
        &mut events,
        Duration::from_millis(300),
        Duration::from_secs(5),
    )
    .await;
    let mood = replay.handle().state().get::<String>("mood");
    replay.disconnect().await.unwrap();
    mood
}

#[tokio::test]
async fn a_replay_answers_the_extractor_from_the_tape() {
    let tape = Arc::new(MemoryTape::new());
    let model = MockLlm::script([LlmResponse::from_text(
        json!({ "mood": "happy" }).to_string(),
    )]);
    let recording = TapedLlm::recording(model.clone(), tape.clone());

    assert_eq!(run(Arc::new(recording)).await.as_deref(), Some("happy"));
    assert_eq!(
        model.call_count(),
        1,
        "the recording run calls the model once"
    );
    assert_eq!(tape.entries().len(), 1, "and records that call");

    let offline = TapedLlm::replaying(
        "gemini-2.5-flash",
        Arc::new(MemoryTape::from_entries(tape.entries())),
    );
    assert_eq!(
        run(Arc::new(offline)).await.as_deref(),
        Some("happy"),
        "the replay extracts the same state with no model"
    );
    assert_eq!(model.call_count(), 1, "the replay never reached the model");
}
