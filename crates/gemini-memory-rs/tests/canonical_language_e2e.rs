//! The canonical form is English; the search terms are not.
//!
//! These two rules look like they are in tension and are in fact the same
//! rule seen from two sides. Reconciliation is lexical — fingerprints, subject
//! and predicate matching, refinement by term containment — so it only works
//! if the same fact spoken three ways lands on one predicate and one value. If
//! half the corpus said `vegetarian` and half said `shakahari`, three
//! restatements would reinforce nothing and produce three records.
//!
//! Retrieval is the other side. The query arrives in whatever language the
//! user is speaking *this* turn, so an index containing only English has
//! nothing for a Hindi question to match. Normalizing the search terms to
//! English too would be exactly the mistake the phrase tables were.
//!
//! So: canonicalize the fact, keep the index multilingual. Tested against the
//! real model, because the contract is enforced by a prompt.

#![cfg(feature = "gemini-llm")]

mod common;

use gemini_memory_rs::core::{CanonicalMemory, SessionId, TurnId};
use gemini_memory_rs::engine::MemorySession;

use common::{active, describe, diagnose, have_api_key, model_backed_engine, skip, ScratchDir};

async fn say(session: &MemorySession, turn: u64, utterance: &str) {
    let turn_id = TurnId(turn);
    session.begin_turn(turn_id);
    session
        .observe_final_transcript(turn_id, utterance)
        .await
        .expect("ingestion should not fail the turn");
    session.on_turn_complete(turn_id).await.unwrap();
}

/// Every character that is unambiguously not Latin script.
fn non_latin(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphabetic() && !c.is_ascii_alphabetic())
        .collect()
}

fn canonical_fields(m: &CanonicalMemory) -> String {
    format!(
        "{} {} {} {}",
        m.statement,
        m.predicate,
        m.value.display(),
        m.qualifier.clone().unwrap_or_default()
    )
}

#[tokio::test]
async fn the_same_fact_in_three_languages_becomes_one_record() {
    if !have_api_key() {
        return skip("the_same_fact_in_three_languages_becomes_one_record");
    }
    let scratch = ScratchDir::new("canon-merge");
    let engine = model_backed_engine("usr_canon", scratch.path());

    // English, Hinglish, Devanagari — one claim, three lexicons.
    for (i, (utterance, session_id)) in [
        ("I am vegetarian, I don't eat meat.", "ses_1"),
        ("Main vegetarian hoon, main meat nahi khata.", "ses_2"),
        ("मैं शाकाहारी हूँ, मैं मांस नहीं खाता।", "ses_3"),
    ]
    .iter()
    .enumerate()
    {
        let session = engine.begin_session(SessionId::new(*session_id));
        say(&session, i as u64 + 1, utterance).await;
        session.finish().await.unwrap();
        engine.compile_index().await.unwrap();
    }

    let records = engine.repository().all(engine.user()).await.unwrap();
    let live = active(&records);
    let dietary: Vec<_> = live
        .iter()
        .filter(|m| {
            let text = canonical_fields(m).to_lowercase();
            text.contains("vegetarian") || text.contains("meat") || text.contains("diet")
        })
        .collect();

    assert_eq!(
        dietary.len(),
        1,
        "one claim in three languages produced {} records instead of reinforcing one\n{}",
        dietary.len(),
        diagnose(&engine, "ses_3", &records).await
    );
    assert!(
        dietary[0].evidence.count >= 2,
        "the restatements did not reinforce: evidence count {}\n{}",
        dietary[0].evidence.count,
        describe(&records)
    );
}

#[tokio::test]
async fn the_canonical_fields_are_english_and_the_search_terms_are_not() {
    if !have_api_key() {
        return skip("the_canonical_fields_are_english_and_the_search_terms_are_not");
    }
    let scratch = ScratchDir::new("canon-split");
    let engine = model_backed_engine("usr_split", scratch.path());

    let session = engine.begin_session(SessionId::new("ses_1"));
    say(&session, 1, "मैं शाकाहारी हूँ, मैं मांस नहीं खाता।").await;
    say(&session, 2, "Enakku filter coffee romba pidikkum daily.").await;
    session.finish().await.unwrap();

    let records = engine.repository().all(engine.user()).await.unwrap();
    let live = active(&records);
    assert!(
        !live.is_empty(),
        "nothing was stored\n{}",
        diagnose(&engine, "ses_1", &records).await
    );

    for m in &live {
        // The canonical layer: reconciliation reads these, so they must be one
        // language. Devanagari here would mean a later English restatement
        // could not match it.
        let stray = non_latin(&canonical_fields(m));
        assert!(
            stray.is_empty(),
            "canonical fields carry non-Latin script {stray:?}, which reconciliation \
             cannot match against an English restatement: {}",
            canonical_fields(m)
        );
    }

    // The index layer: at least one record must carry vocabulary from the
    // language the user actually spoke, or a question in that language has
    // nothing to hit.
    let native_terms: Vec<&str> = live
        .iter()
        .flat_map(|m| m.retrieval.tags.iter().map(String::as_str))
        .filter(|t| {
            !non_latin(t).is_empty()
                || matches!(
                    *t,
                    "khaana" | "saapadu" | "pidikkum" | "enakku" | "romba" | "nahi" | "hoon"
                )
        })
        .collect();
    assert!(
        !native_terms.is_empty(),
        "every search term was normalized to English, so a question in the user's \
         own language can never match:\n{}",
        live.iter()
            .map(|m| format!("  {} -> {:?}", m.predicate, m.retrieval.tags))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
