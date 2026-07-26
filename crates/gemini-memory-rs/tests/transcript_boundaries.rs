//! Boundary and adversarial behaviour, driven through the real integration path.
//!
//! Everything here is deterministic — no API key, no network — and every case is
//! something a real voice session actually does: a recognizer rewriting an
//! utterance it already emitted, a turn arriving out of order, a user who talks
//! for a very long time, a conversation that never stops.
//!
//! Ingestion is driven through [`MemoryTurnExtractor`], because that is how the
//! Live runtime drives it; a test that bypassed it would be testing a path no
//! application uses.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gemini_adk_rs::live::extractor::TurnExtractor;
use gemini_adk_rs::live::transcript::TranscriptTurn;
use gemini_adk_rs::state::State;
use gemini_memory_rs::core::{MemoryRuntimeConfig, SessionId, TranscriptConfig, TurnId, UserId};
use gemini_memory_rs::engine::{MemoryEngine, MemorySession};
use gemini_memory_rs::runtime::{MemorySlot, MemoryTurnExtractor};
use gemini_memory_rs::transcript::{
    GenerationGuard, SpeculationDecision, SpeculationGate, TranscriptAccumulator,
    TranscriptHypothesis,
};

fn engine() -> Arc<MemoryEngine> {
    Arc::new(MemoryEngine::in_memory(UserId::new("usr_boundary")))
}

fn session(engine: &MemoryEngine) -> Arc<MemorySession> {
    Arc::new(engine.begin_session(SessionId::new("ses_1")))
}

fn turn(number: u32, user: &str) -> TranscriptTurn {
    TranscriptTurn {
        turn_number: number,
        user: user.to_string(),
        model: String::new(),
        tool_calls: Vec::new(),
        timestamp: Instant::now(),
    }
}

// ─── transcript accumulation ────────────────────────────────────────────────

#[test]
fn a_recognizer_that_rewrites_everything_leaves_no_stale_prefix() {
    let mut acc = TranscriptAccumulator::new(TurnId(1));
    acc.push_partial("I hate spicy food");
    acc.push_partial("I ate spicy food");
    let h = acc.finalize("I ate spicy food last night");

    assert_eq!(h.stable_prefix, "I ate spicy food last night");
    assert!(
        !h.text().contains("hate"),
        "a revised-away reading survived"
    );
}

#[test]
fn a_partial_that_shrinks_does_not_resurrect_dropped_words() {
    let mut acc = TranscriptAccumulator::new(TurnId(1));
    acc.push_partial("book a table for eight people");
    let h = acc.push_partial("book a table");
    assert!(h.text().starts_with("book a table"));
    assert!(!h.text().contains("eight people"));
}

#[test]
fn unicode_and_devanagari_survive_accumulation_intact() {
    let mut acc = TranscriptAccumulator::new(TurnId(1));
    acc.push_partial("मैं शाकाहारी हूँ");
    let h = acc.finalize("मैं शाकाहारी हूँ 🙂");
    assert_eq!(h.stable_prefix, "मैं शाकाहारी हूँ 🙂");
}

#[test]
fn a_very_long_utterance_accumulates_without_quadratic_blowup() {
    let mut acc = TranscriptAccumulator::new(TurnId(1));
    let mut words: Vec<String> = Vec::new();
    let started = Instant::now();
    for i in 0..1_000 {
        words.push(format!("word{i}"));
        acc.push_partial(&words.join(" "));
    }
    let h = acc.finalize(&words.join(" "));
    assert_eq!(h.stable_prefix.split_whitespace().count(), 1_000);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "accumulating a long utterance took {:?}",
        started.elapsed()
    );
}

#[test]
fn empty_and_whitespace_transcripts_are_harmless() {
    let mut acc = TranscriptAccumulator::new(TurnId(1));
    assert!(acc.push_partial("").text().is_empty());
    assert!(acc.push_partial("   ").text().is_empty());
    assert!(acc.finalize("").stable_prefix.is_empty());
}

// ─── the generation guard ───────────────────────────────────────────────────

#[test]
fn a_result_from_two_turns_ago_is_never_current() {
    let guard = GenerationGuard::new();
    let stale = guard.current();
    guard.advance();
    guard.advance();
    assert!(!guard.is_current(stale));
}

#[test]
fn the_guard_is_monotonic_under_concurrent_advances() {
    let guard = Arc::new(GenerationGuard::new());
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let guard = guard.clone();
            std::thread::spawn(move || {
                for _ in 0..1_000 {
                    guard.advance();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(guard.current(), 8_000);
}

// ─── the speculation gate ───────────────────────────────────────────────────

#[test]
fn a_burst_of_revisions_produces_at_most_one_speculation_per_window() {
    let mut gate = SpeculationGate::new(&TranscriptConfig::default());
    let t0 = Instant::now();

    let mut fired = 0;
    for i in 0..200u64 {
        let hypothesis = TranscriptHypothesis {
            stable_prefix: (0..=i)
                .map(|n| format!("w{n}"))
                .collect::<Vec<_>>()
                .join(" "),
            ..Default::default()
        };
        if gate.consider(&hypothesis, false, t0 + Duration::from_millis(i))
            == SpeculationDecision::Fire
        {
            fired += 1;
        }
    }
    assert!(
        fired <= 2,
        "{fired} speculations fired inside a 200ms burst; the debounce is not holding"
    );
}

// ─── ingestion through the runtime's own pipeline ───────────────────────────

#[tokio::test]
async fn a_turn_the_pipeline_skips_costs_nothing() {
    let engine = engine();
    let extractor = MemoryTurnExtractor::new(session(&engine));
    for filler in ["ok", "mm", "haan", "sari"] {
        assert!(
            !extractor.should_extract(&[turn(1, filler)]),
            "`{filler}` should not be worth an extraction"
        );
    }
}

#[tokio::test]
async fn turns_arriving_out_of_order_are_each_attributed_correctly() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone());

    // The pipeline hands windows in whatever order transcription finalized.
    extractor
        .extract(&[turn(4, "I am allergic to nuts")])
        .await
        .unwrap();
    extractor
        .extract(&[turn(2, "I am pescatarian")])
        .await
        .unwrap();

    let candidates = session.ledger().usable_candidates();
    assert_eq!(candidates.len(), 2);
    let turns: Vec<u64> = candidates.iter().map(|c| c.last_seen_turn.0).collect();
    assert!(turns.contains(&4) && turns.contains(&2));
}

#[tokio::test]
async fn a_redelivered_turn_does_not_double_count_evidence() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone());

    extractor
        .extract(&[turn(1, "I am pescatarian")])
        .await
        .unwrap();
    // The same turn number and the same words: a redelivery, not a restatement.
    extractor
        .extract(&[turn(1, "I am pescatarian")])
        .await
        .unwrap();

    let candidates = session.ledger().usable_candidates();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].distinct_turns, 1,
        "a redelivered turn was counted as independent evidence"
    );
}

#[tokio::test]
async fn an_empty_or_whitespace_turn_produces_nothing() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone());
    extractor.extract(&[turn(1, "     ")]).await.unwrap();
    assert!(session.ledger().is_empty());
}

// ─── budgets and scale ──────────────────────────────────────────────────────

#[tokio::test]
async fn an_adversarially_verbose_session_still_respects_the_token_cap() {
    let engine = engine();
    let session = engine.begin_session(SessionId::new("ses_1"));

    for i in 1..=50u64 {
        session.begin_turn(TurnId(i));
        session
            .observe_final_transcript(
                TurnId(i),
                &format!(
                    "I always prefer the extremely specific arrangement number {i} \
                     with all of its many elaborate and long-winded qualifications"
                ),
            )
            .await
            .unwrap();
        session.on_turn_complete(TurnId(i)).await.unwrap();
    }

    let snapshot = session
        .prepare(TurnId(51), "what do you remember about my preferences")
        .await
        .unwrap();
    let cap = MemoryRuntimeConfig::default().retrieval;
    assert!(
        usize::from(snapshot.token_count) <= cap.max_tokens,
        "context was {} tokens, cap is {}",
        snapshot.token_count,
        cap.max_tokens
    );
    assert!(snapshot.facts.len() <= cap.max_memories);
}

#[tokio::test]
async fn a_long_session_stays_responsive_and_does_not_accumulate_duplicates() {
    let engine = engine();
    let session = engine.begin_session(SessionId::new("ses_long"));

    let started = Instant::now();
    for i in 1..=500u64 {
        session.begin_turn(TurnId(i));
        session
            .observe_final_transcript(TurnId(i), "I like the colour blue")
            .await
            .unwrap();
        session.on_turn_complete(TurnId(i)).await.unwrap();
    }
    let ingest = started.elapsed();

    let query_started = Instant::now();
    let snapshot = session
        .prepare(TurnId(501), "do you remember what colour I like")
        .await
        .unwrap();
    let query = query_started.elapsed();

    assert!(!snapshot.is_empty());
    assert!(
        query < Duration::from_millis(200),
        "retrieval took {query:?} after 500 turns"
    );
    assert!(ingest < Duration::from_secs(60), "ingest took {ingest:?}");
    assert_eq!(
        session.ledger().usable_candidates().len(),
        1,
        "the same fact restated 500 times should be one candidate"
    );
}

// ─── slot projection, the application-facing contract ───────────────────────

#[tokio::test]
async fn a_slot_filled_from_memory_is_indistinguishable_from_one_the_user_just_filled() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone())
        .slots([MemorySlot::new("dietary_identity", "user.diet")]);
    let state = State::new();

    extractor
        .extract_with_state(&[turn(1, "I am pescatarian")], &state)
        .await
        .unwrap();

    // This is exactly what `phase.needs(&["user.diet"])` and a `Flow` guard
    // read, which is what stops a returning user being asked twice.
    assert_eq!(
        state.get::<String>("user.diet").as_deref(),
        Some("pescatarian")
    );
}

#[tokio::test]
async fn slot_projection_survives_a_correction_within_the_session() {
    let engine = engine();
    let session = session(&engine);
    let extractor = MemoryTurnExtractor::new(session.clone())
        .slots([MemorySlot::new("dietary_identity", "user.diet")]);

    extractor
        .extract_with_state(&[turn(1, "I am vegetarian")], &State::new())
        .await
        .unwrap();
    extractor
        .extract_with_state(&[turn(2, "actually I am pescatarian")], &State::new())
        .await
        .unwrap();

    // A fresh state, as a later phase evaluation would see.
    let later = State::new();
    extractor
        .extract_with_state(&[turn(3, "so what should we cook")], &later)
        .await
        .unwrap();
    assert_eq!(
        later.get::<String>("user.diet").as_deref(),
        Some("pescatarian"),
        "the slot still held the corrected-away value"
    );
}

// ─── isolation ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn two_sessions_for_one_user_do_not_leak_into_each_other() {
    let engine = engine();
    let first = Arc::new(engine.begin_session(SessionId::new("ses_a")));
    let second = Arc::new(engine.begin_session(SessionId::new("ses_b")));

    first
        .observe_final_transcript(TurnId(1), "I am pescatarian")
        .await
        .unwrap();
    second
        .observe_final_transcript(TurnId(1), "I am allergic to nuts")
        .await
        .unwrap();

    let statements: Vec<String> = first
        .ledger()
        .usable_candidates()
        .iter()
        .map(|c| c.canonical_statement.clone())
        .collect();
    assert!(
        statements.iter().all(|s| !s.contains("nuts")),
        "a concurrent session's evidence leaked: {statements:?}"
    );
}

#[tokio::test]
async fn a_deletion_targets_only_whole_word_matches() {
    // Deletion is irreversible, so this is the invariant that matters most.
    let engine = engine();
    let session = engine.begin_session(SessionId::new("ses_1"));
    session
        .observe_final_transcript(TurnId(1), "I like art galleries")
        .await
        .unwrap();
    session
        .observe_final_transcript(TurnId(2), "I always leave things in my cart")
        .await
        .unwrap();
    session.finish().await.unwrap();

    let before = engine.repository().all(engine.user()).await.unwrap().len();

    let forgetting = engine.begin_session(SessionId::new("ses_2"));
    forgetting
        .observe_final_transcript(TurnId(1), "forget about art")
        .await
        .unwrap();
    forgetting.finish().await.unwrap();

    let remaining = engine.repository().all(engine.user()).await.unwrap();
    assert!(
        remaining.iter().any(|m| m.statement.contains("cart")),
        "forgetting `art` deleted an unrelated memory about a cart: \
         {before} records before, {} after",
        remaining.len()
    );
}
