//! End-to-end memory lifecycle against the real Gemini API, text-only.
//!
//! A scripted user talks across several sessions and the engine is driven
//! exactly as a live conversation would drive it: real model extraction, real
//! OKF files on disk, real reconciliation. The assertions are about *behaviour*
//! — a fact reached the corpus once, a correction retired the old record, an
//! event expires — not about the model's exact wording, which will vary.
//!
//! Skips when no Gemini API key is configured.

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use gemini_memory_rs::core::{MemoryStatus, SessionId, TurnId};
use gemini_memory_rs::engine::MemorySession;
use gemini_memory_rs::runtime::MemoryTurnExtractor;

use common::{
    active, corpus_text, describe, diagnose, have_api_key, mentions, model_backed_engine, skip,
    ScratchDir,
};

/// Drive a session the way the runtime does: begin turn, ingest, complete.
async fn say(session: &MemorySession, turn: u64, utterance: &str) {
    let turn_id = TurnId(turn);
    session.begin_turn(turn_id);
    session
        .observe_final_transcript(turn_id, utterance)
        .await
        .expect("ingestion should not fail the turn");
    session
        .on_turn_complete(turn_id)
        .await
        .expect("turn completion should not fail");
}

/// Ask, and return the statements the model would have been handed.
async fn ask(session: &MemorySession, turn: u64, question: &str) -> Vec<String> {
    let turn_id = TurnId(turn);
    session.begin_turn(turn_id);
    let snapshot = session
        .prepare(turn_id, question)
        .await
        .expect("preparation should not fail the turn");
    snapshot
        .facts
        .iter()
        .map(|f| f.presented_statement())
        .collect()
}

#[tokio::test]
async fn a_conversation_becomes_durable_human_readable_memory() {
    if !have_api_key() {
        return skip("a_conversation_becomes_durable_human_readable_memory");
    }
    let scratch = ScratchDir::new("lifecycle");
    let engine = model_backed_engine("usr_e2e", scratch.path());

    let session = engine.begin_session(SessionId::new("ses_monday"));
    say(&session, 1, "I'm vegetarian — I don't eat meat at all.").await;
    say(
        &session,
        2,
        "My wife Rhea really can't stand noisy restaurants.",
    )
    .await;
    say(&session, 3, "Anyway, what's the weather going to be like?").await;
    let report = session.finish().await.expect("reconciliation");

    let records = engine.repository().all(engine.user()).await.unwrap();
    let live = active(&records);
    assert!(
        !live.is_empty(),
        "a conversation with two clear facts stored nothing\n{}",
        diagnose(&engine, "ses_monday", &records).await
    );
    assert!(
        mentions(&live, "vegetarian") || mentions(&live, "meat"),
        "the dietary fact was not stored\n{}",
        describe(&records)
    );
    assert!(
        mentions(&live, "rhea") || mentions(&live, "noisy") || mentions(&live, "quiet"),
        "the relationship preference was not stored\n{}",
        describe(&records)
    );
    assert!(
        !mentions(&live, "weather"),
        "small talk became a memory\n{}",
        describe(&records)
    );
    assert!(report.creates > 0, "nothing was created: {report:?}");

    // The corpus is Markdown a human can read, not an opaque index.
    let text = corpus_text(scratch.path());
    assert!(text.contains("okf: memory/v1"), "corpus:\n{text}");
    assert!(text.contains("# Fact"), "corpus:\n{text}");
}

#[tokio::test]
async fn a_correction_supersedes_rather_than_duplicating() {
    if !have_api_key() {
        return skip("a_correction_supersedes_rather_than_duplicating");
    }
    let scratch = ScratchDir::new("correction");
    let engine = model_backed_engine("usr_e2e", scratch.path());

    let monday = engine.begin_session(SessionId::new("ses_monday"));
    say(&monday, 1, "Just so you know, I'm vegetarian.").await;
    monday.finish().await.unwrap();
    engine.compile_index().await.unwrap();

    let thursday = engine.begin_session(SessionId::new("ses_thursday"));
    say(
        &thursday,
        1,
        "Actually I've started eating fish again, so I'm pescatarian now.",
    )
    .await;

    // The correction has to take effect inside the conversation, before any
    // reconciliation has run.
    let recalled = ask(&thursday, 2, "remind me what my dietary preferences are").await;
    let joined = recalled.join(" | ").to_lowercase();
    assert!(
        !joined.contains("vegetarian") || joined.contains("pescatarian"),
        "the corrected-away fact was recalled unqualified: {recalled:?}"
    );

    thursday.finish().await.unwrap();

    let records = engine.repository().all(engine.user()).await.unwrap();
    let live = active(&records);
    let dietary: Vec<_> = live
        .iter()
        .filter(|m| {
            let s = m.statement.to_lowercase();
            s.contains("pescatarian") || s.contains("vegetarian") || s.contains("fish")
        })
        .collect();
    assert_eq!(
        dietary.len(),
        1,
        "a correction should leave exactly one active dietary fact\n{}",
        describe(&records)
    );
    assert!(
        records.iter().any(|m| m.status == MemoryStatus::Superseded),
        "nothing was superseded — the correction created a duplicate instead\n{}",
        describe(&records)
    );
    let retired = records
        .iter()
        .find(|m| m.status == MemoryStatus::Superseded)
        .unwrap();
    assert!(
        retired.superseded_by.is_some() && retired.temporal.valid_to.is_some(),
        "a superseded record must record what replaced it and when"
    );
}

#[tokio::test]
async fn repetition_across_sessions_reinforces_one_record() {
    if !have_api_key() {
        return skip("repetition_across_sessions_reinforces_one_record");
    }
    let scratch = ScratchDir::new("reinforce");
    let engine = model_backed_engine("usr_e2e", scratch.path());

    for session_name in ["ses_1", "ses_2", "ses_3"] {
        let session = engine.begin_session(SessionId::new(session_name));
        say(&session, 1, "I always go to the gym before work.").await;
        session.finish().await.unwrap();
        engine.compile_index().await.unwrap();
    }

    let records = engine.repository().all(engine.user()).await.unwrap();
    let routines: Vec<_> = active(&records)
        .into_iter()
        .filter(|m| {
            let s = m.statement.to_lowercase();
            s.contains("gym") || s.contains("work out") || s.contains("exercise")
        })
        .collect();

    assert_eq!(
        routines.len(),
        1,
        "the same routine stated three times should be one record\n{}",
        describe(&records)
    );
    assert!(
        routines[0].evidence.distinct_sessions >= 2,
        "evidence did not accumulate across sessions: {:?}",
        routines[0].evidence
    );
}

#[tokio::test]
async fn a_time_bounded_plan_expires_but_a_preference_does_not() {
    if !have_api_key() {
        return skip("a_time_bounded_plan_expires_but_a_preference_does_not");
    }
    let scratch = ScratchDir::new("temporality");
    let engine = model_backed_engine("usr_e2e", scratch.path());

    let session = engine.begin_session(SessionId::new("ses_1"));
    say(
        &session,
        1,
        "I'm flying to Delhi tomorrow for a couple of days.",
    )
    .await;
    say(&session, 2, "I drink flat whites, always have.").await;
    session.finish().await.unwrap();

    let records = engine.repository().all(engine.user()).await.unwrap();
    let live = active(&records);

    let bounded: Vec<_> = live
        .iter()
        .filter(|m| m.temporal.expires_at.is_some())
        .collect();
    let unbounded: Vec<_> = live
        .iter()
        .filter(|m| m.temporal.expires_at.is_none())
        .collect();

    assert!(
        !bounded.is_empty(),
        "the trip should have been stored with an expiry\n{}",
        describe(&records)
    );
    assert!(
        !unbounded.is_empty(),
        "the standing preference should have no expiry\n{}",
        diagnose(&engine, "ses_1", &records).await
    );

    // The bounded record stops being retrievable once its window passes; the
    // preference does not.
    let far_future = Utc::now() + Duration::days(90);
    assert!(
        bounded.iter().all(|m| !m.is_retrievable(far_future)),
        "a time-bounded record was still retrievable three months later\n{}",
        describe(&records)
    );
    assert!(
        unbounded.iter().any(|m| m.is_retrievable(far_future)),
        "a standing preference expired\n{}",
        describe(&records)
    );
}

#[tokio::test]
async fn only_the_users_own_speech_becomes_memory() {
    if !have_api_key() {
        return skip("only_the_users_own_speech_becomes_memory");
    }
    let scratch = ScratchDir::new("attribution");
    let engine = model_backed_engine("usr_e2e", scratch.path());
    let session = engine.begin_session(SessionId::new("ses_1"));

    // Something a person nearby said, attributed to a bystander. The extractor
    // must not spend a request on it, let alone store it.
    let bystander = gemini_memory_rs::llm::observation_context(
        "I'm allergic to peanuts and I hate flying",
        SessionId::new("ses_1"),
        TurnId(1),
        Utc::now(),
        gemini_memory_rs::core::SpeakerAttribution::Bystander,
    );
    let extractor = gemini_memory_rs::llm::GeminiObservationExtractor::from_env();
    let observations = {
        use gemini_memory_rs::ingestion::MemoryObservationExtractor;
        extractor.extract(bystander).await.unwrap()
    };
    assert!(
        observations.is_empty(),
        "bystander speech produced {observations:?}"
    );

    say(&session, 1, "I'm allergic to peanuts.").await;
    session.finish().await.unwrap();

    let records = engine.repository().all(engine.user()).await.unwrap();
    let live = active(&records);
    assert!(
        mentions(&live, "peanut") || mentions(&live, "allerg"),
        "the user's own statement was not stored\n{}",
        diagnose(&engine, "ses_1", &records).await
    );
    assert!(
        !mentions(&live, "flying"),
        "bystander content reached the corpus\n{}",
        describe(&records)
    );
}

#[tokio::test]
async fn the_turn_extractor_drives_ingestion_from_the_runtime_pipeline() {
    if !have_api_key() {
        return skip("the_turn_extractor_drives_ingestion_from_the_runtime_pipeline");
    }
    use gemini_adk_rs::live::extractor::TurnExtractor;
    use gemini_adk_rs::live::transcript::TranscriptTurn;

    let scratch = ScratchDir::new("turn-extractor");
    let engine = model_backed_engine("usr_e2e", scratch.path());
    let session = Arc::new(engine.begin_session(SessionId::new("ses_1")));
    let extractor = MemoryTurnExtractor::new(session.clone());

    let turn = TranscriptTurn {
        turn_number: 1,
        user: "I'm allergic to shellfish, it's quite serious.".into(),
        model: "Understood, I'll remember that.".into(),
        tool_calls: Vec::new(),
        timestamp: std::time::Instant::now(),
    };
    assert!(extractor.should_extract(std::slice::from_ref(&turn)));
    let summary = extractor
        .extract(std::slice::from_ref(&turn))
        .await
        .unwrap();

    // One utterance may legitimately carry more than one fact, so this asserts
    // that ingestion happened — not how the model chose to split it.
    assert!(
        summary["created"].as_u64().unwrap_or(0) >= 1,
        "the turn produced no candidate: {summary}"
    );
    // The model's own turn is in the window and must not become a memory.
    let statements: Vec<String> = session
        .ledger()
        .usable_candidates()
        .iter()
        .map(|c| c.canonical_statement.to_lowercase())
        .collect();
    assert!(
        statements.iter().all(|s| !s.contains("i'll remember")),
        "the assistant's turn became a memory: {statements:?}"
    );
}
