//! Memory over a real Live WebSocket session.
//!
//! Everything else in this crate exercises the text path: the extractor is
//! driven directly, or the engine is. That leaves the part an application
//! actually runs — a connected session, tools declared in the setup message,
//! the control lane driving the extractor at each turn boundary — asserted
//! nowhere.
//!
//! **Text in, audio out.** Only the input is text; the model answers in voice as
//! it does in production, and the assertions read the *output transcription*.
//! Reading a text-modality response would exercise a path no deployment uses.
//!
//! Skips when no Gemini API key is configured.
//!
//! The corpus here is deliberately tiny — this file is about the wiring. For
//! retrieval against a corpus large enough to be wrong about, see
//! `haystack_live_e2e.rs`.

// Needs `gemini-llm` for model-backed extraction and `fluent` for the L2
// `Live` builder these drive; `gemini-llm` alone does not compile.
#![cfg(all(feature = "gemini-llm", feature = "fluent"))]

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gemini_adk_fluent_rs::live::Live;
use gemini_memory_rs::core::{SessionId, TurnId};
use gemini_memory_rs::engine::{MemoryEngine, MemorySession};
use gemini_memory_rs::runtime::{LiveMemoryExt, MemorySlot};

use common::live::{connect, Observed};
use common::{have_api_key, model_backed_engine, skip, ScratchDir};

fn slots() -> Vec<MemorySlot> {
    vec![
        MemorySlot::new("dietary_identity", "user:diet"),
        MemorySlot::new("preference", "user:venue"),
    ]
}

/// Connect a voice session with memory installed, wired to an [`Observed`].
async fn connect_with_memory(
    session: Arc<MemorySession>,
    instruction: &str,
    observed: Arc<Observed>,
) -> Result<gemini_adk_rs::live::LiveHandle, gemini_adk_rs::error::AgentError> {
    connect(
        Live::builder()
            .instruction(instruction)
            .with_memory_slots(session, slots()),
        observed,
    )
    .await
}

/// Seed durable memory the way a previous conversation would have, then make it
/// retrievable.
async fn seed(engine: &MemoryEngine, utterances: &[&str]) {
    let past = engine.begin_session(SessionId::new("ses_last_week"));
    for (i, utterance) in utterances.iter().enumerate() {
        let turn = TurnId(i as u64 + 1);
        past.begin_turn(turn);
        past.observe_final_transcript(turn, utterance)
            .await
            .expect("ingestion");
        past.on_turn_complete(turn).await.expect("turn completion");
    }
    past.finish().await.expect("reconciliation");
    engine.compile_index().await.expect("index compile");
}

// ─── (a) the session accepts the memory tools ───────────────────────────────

/// A malformed tool declaration is rejected at setup, so a session that connects
/// and completes a turn is proof both memory tools serialize into a shape the
/// API accepts. Cheapest test here and the one that catches schema drift.
#[tokio::test]
async fn a_session_connects_with_the_memory_tools_declared() {
    if !have_api_key() {
        return skip("a_session_connects_with_the_memory_tools_declared");
    }
    let scratch = ScratchDir::new("live-connect");
    let engine = Arc::new(model_backed_engine("usr_live_connect", scratch.path()));
    let session = Arc::new(engine.begin_session(SessionId::new("ses_live")));

    let observed = Arc::new(Observed::default());
    let handle = connect_with_memory(
        session,
        "You are a brief dinner companion. Answer in one short sentence.",
        observed.clone(),
    )
    .await
    .expect("a session with the memory tools declared must connect");

    let mark = observed.mark();
    handle.send_text("Hello there.").await.expect("send");
    observed.await_answer(mark, "greeting the model").await;

    handle.disconnect().await.ok();

    assert!(
        observed.errors.lock().is_empty(),
        "the server reported errors on a session with memory tools declared\n  {}",
        observed.report()
    );
    assert!(
        observed.audio_bytes.load(Ordering::Relaxed) > 0,
        "no audio arrived — the session was not in voice mode, so this test is \
         exercising a path no deployment uses\n  {}",
        observed.report()
    );
}

// ─── (b) a remembered fact reaches the spoken answer ────────────────────────

/// The model is told nothing about the user's diet. If it answers correctly, it
/// went and got the fact — which is the entire proposition.
///
/// Asserted on the *output transcript*, not `on_text`: the answer is audio, and
/// the transcript is how a deployment reads it.
#[tokio::test]
async fn a_remembered_fact_reaches_the_models_spoken_answer() {
    if !have_api_key() {
        return skip("a_remembered_fact_reaches_the_models_spoken_answer");
    }
    let scratch = ScratchDir::new("live-recall");
    let engine = Arc::new(model_backed_engine("usr_live_recall", scratch.path()));
    seed(
        &engine,
        &[
            "I'm vegetarian — I don't eat meat at all.",
            "I really can't stand noisy restaurants.",
        ],
    )
    .await;

    let session = Arc::new(engine.begin_session(SessionId::new("ses_today")));
    let observed = Arc::new(Observed::default());
    let handle = connect_with_memory(
        session,
        "You are a dinner companion. You have tools that recall what you know \
         about this person — use them before answering questions about their \
         preferences. Answer in one short sentence.",
        observed.clone(),
    )
    .await
    .expect("connect");

    let mark = observed.mark();
    handle
        .send_text("Remind me — what do I eat? Say it in a few words.")
        .await
        .expect("send");
    observed
        .await_answer(mark, "asking about the remembered diet")
        .await;

    handle.disconnect().await.ok();

    let spoken = observed.transcript();
    assert!(
        spoken.contains("vegetarian") || spoken.contains("no meat") || spoken.contains("meat"),
        "the model answered without the fact memory holds\n  {}",
        observed.report()
    );
}

// ─── (c) a fact stated over the wire becomes durable memory ─────────────────

/// The end-to-end proof that `LiveHandle::send_text` reaches the transcript.
///
/// Before that fix the transcript's user side was written only by ASR of audio,
/// so this session would have ingested nothing at all and the corpus would come
/// back empty — which is exactly what this asserts against.
#[tokio::test]
async fn a_fact_stated_over_the_wire_becomes_durable_memory() {
    if !have_api_key() {
        return skip("a_fact_stated_over_the_wire_becomes_durable_memory");
    }
    let scratch = ScratchDir::new("live-ingest");
    let engine = Arc::new(model_backed_engine("usr_live_ingest", scratch.path()));
    let session = Arc::new(engine.begin_session(SessionId::new("ses_stating")));

    let observed = Arc::new(Observed::default());
    let handle = connect_with_memory(
        session.clone(),
        "You are a brief dinner companion. Acknowledge what you're told in one \
         short sentence.",
        observed.clone(),
    )
    .await
    .expect("connect");

    let mark = observed.mark();
    handle
        .send_text("Something you should know: I'm allergic to shellfish.")
        .await
        .expect("send");
    observed
        .await_answer(mark, "stating a fact over the wire")
        .await;

    handle.disconnect().await.ok();

    let candidates: Vec<String> = session
        .ledger()
        .usable_candidates()
        .iter()
        .map(|c| c.canonical_statement.to_lowercase())
        .collect();

    assert!(
        !candidates.is_empty(),
        "a fact stated over the wire was never ingested — the typed turn did not \
         reach the transcript the extractor reads\n  {}",
        observed.report()
    );
    assert!(
        candidates.iter().any(|c| c.contains("shellfish")),
        "ingestion ran but the stated fact is not among the candidates: \
         {candidates:?}\n  {}",
        observed.report()
    );
}
