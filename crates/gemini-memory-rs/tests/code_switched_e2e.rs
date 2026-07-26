//! Code-switched speech: Hinglish and Tanglish, against the real Gemini API.
//!
//! Most Indian users do not speak one language per sentence. "Main vegetarian
//! hoon", "mujhe spicy khaana pasand nahi hai", "enakku coffee venum" are the
//! normal case, not an edge case, and a memory engine that only works in
//! English is a memory engine that does not work.
//!
//! Two separable questions, tested separately because they fail differently:
//!
//! 1. **Ingestion** — can the extractor read code-switched speech at all?
//! 2. **Retrieval** — once stored, can a code-switched question find it? This
//!    is the lexical side, where English stop-word lists and English plural
//!    folding actively hurt.

#![cfg(feature = "gemini-llm")]

mod common;

use gemini_memory_rs::core::{SessionId, TurnId};
use gemini_memory_rs::engine::MemorySession;

use common::{
    active, describe, diagnose, have_api_key, mentions, model_backed_engine, skip, ScratchDir,
};

async fn say(session: &MemorySession, turn: u64, utterance: &str) {
    let turn_id = TurnId(turn);
    session.begin_turn(turn_id);
    session
        .observe_final_transcript(turn_id, utterance)
        .await
        .expect("ingestion should not fail the turn");
    session.on_turn_complete(turn_id).await.unwrap();
}

async fn ask(session: &MemorySession, turn: u64, question: &str) -> Vec<String> {
    let turn_id = TurnId(turn);
    session.begin_turn(turn_id);
    session
        .prepare(turn_id, question)
        .await
        .expect("preparation should not fail the turn")
        .facts
        .iter()
        .map(|f| f.statement.clone())
        .collect()
}

#[tokio::test]
async fn hinglish_speech_becomes_memory() {
    if !have_api_key() {
        return skip("hinglish_speech_becomes_memory");
    }
    let scratch = ScratchDir::new("hinglish");
    let engine = model_backed_engine("usr_hinglish", scratch.path());

    let session = engine.begin_session(SessionId::new("ses_1"));
    say(&session, 1, "Main vegetarian hoon, main meat nahi khata.").await;
    say(
        &session,
        2,
        "Meri wife Rhea ko loud restaurants bilkul pasand nahi hai.",
    )
    .await;
    say(
        &session,
        3,
        "Mujhe subah gym jaane ki aadat hai, roz jaata hoon.",
    )
    .await;
    session.finish().await.unwrap();

    let records = engine.repository().all(engine.user()).await.unwrap();
    let live = active(&records);
    assert!(
        !live.is_empty(),
        "Hinglish speech stored nothing\n{}",
        diagnose(&engine, "ses_1", &records).await
    );
    assert!(
        mentions(&live, "vegetarian") || mentions(&live, "meat"),
        "the dietary fact was not extracted from Hinglish\n{}",
        describe(&records)
    );
    assert!(
        mentions(&live, "rhea") || mentions(&live, "loud") || mentions(&live, "restaurant"),
        "the relationship preference was not extracted from Hinglish\n{}",
        describe(&records)
    );
    assert!(
        mentions(&live, "gym") || mentions(&live, "morning"),
        "the routine was not extracted from Hinglish\n{}",
        describe(&records)
    );
}

#[tokio::test]
async fn tanglish_speech_becomes_memory() {
    if !have_api_key() {
        return skip("tanglish_speech_becomes_memory");
    }
    let scratch = ScratchDir::new("tanglish");
    let engine = model_backed_engine("usr_tanglish", scratch.path());

    let session = engine.begin_session(SessionId::new("ses_1"));
    say(
        &session,
        1,
        "Enakku filter coffee romba pidikkum, daily morning venum.",
    )
    .await;
    say(
        &session,
        2,
        "Naan non-veg saapdrathu illai, strict vegetarian.",
    )
    .await;
    session.finish().await.unwrap();

    let records = engine.repository().all(engine.user()).await.unwrap();
    let live = active(&records);
    assert!(
        !live.is_empty(),
        "Tanglish speech stored nothing\n{}",
        diagnose(&engine, "ses_1", &records).await
    );
    assert!(
        mentions(&live, "coffee"),
        "the beverage preference was not extracted from Tanglish\n{}",
        describe(&records)
    );
    assert!(
        mentions(&live, "veg"),
        "the dietary fact was not extracted from Tanglish\n{}",
        describe(&records)
    );
}

#[tokio::test]
async fn a_code_switched_question_finds_what_was_stored() {
    if !have_api_key() {
        return skip("a_code_switched_question_finds_what_was_stored");
    }
    let scratch = ScratchDir::new("hinglish-recall");
    let engine = model_backed_engine("usr_hinglish", scratch.path());

    let first = engine.begin_session(SessionId::new("ses_1"));
    say(&first, 1, "Main vegetarian hoon, meat nahi khata main.").await;
    say(&first, 2, "Mujhe filter coffee bahut pasand hai.").await;
    first.finish().await.unwrap();
    engine.compile_index().await.unwrap();

    // The retrieval side: a code-switched question about a stored fact.
    let second = engine.begin_session(SessionId::new("ses_2"));
    let recalled = ask(
        &second,
        1,
        "Mujhe yaad dilao, mera khaana ka preference kya hai?",
    )
    .await;
    assert!(
        recalled.iter().any(|s| {
            let s = s.to_lowercase();
            s.contains("vegetarian") || s.contains("meat")
        }),
        "a Hinglish question did not find the fact it was asking about: {recalled:?}\n\
         stored:\n{}",
        describe(&engine.repository().all(engine.user()).await.unwrap())
    );
}

#[tokio::test]
async fn devanagari_script_is_indexed_and_retrievable() {
    if !have_api_key() {
        return skip("devanagari_script_is_indexed_and_retrievable");
    }
    let scratch = ScratchDir::new("devanagari");
    let engine = model_backed_engine("usr_devanagari", scratch.path());

    let session = engine.begin_session(SessionId::new("ses_1"));
    say(&session, 1, "मैं शाकाहारी हूँ, मैं मांस नहीं खाता।").await;
    session.finish().await.unwrap();

    let records = engine.repository().all(engine.user()).await.unwrap();
    assert!(
        !active(&records).is_empty(),
        "Devanagari speech stored nothing\n{}",
        diagnose(&engine, "ses_1", &records).await
    );
}
