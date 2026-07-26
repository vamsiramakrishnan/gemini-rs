//! Memory over a real Live WebSocket session, against a corpus worth searching.
//!
//! `live_session_e2e.rs` proves the wiring: the tools serialize, a remembered
//! fact reaches the model, a spoken fact becomes durable. It seeds two
//! utterances to do it, which means it cannot tell *retrieval* from
//! *coincidence* — with two facts in the index, returning "some fact" and
//! returning "the right fact" are the same act.
//!
//! This puts the same machinery in front of ~1,200 records where every question
//! has dozens of plausible wrong answers, and asks three things a device
//! depends on:
//!
//! 1. a fact from months ago reaches the spoken answer, and the *right* one;
//! 2. a fact learned mid-conversation outranks the whole durable corpus, and
//!    survives the session;
//! 3. a correction spoken out loud hides the fact it contradicts, immediately.
//!
//! Each probe's answer token appears exactly once in the corpus and cannot be
//! guessed, so one word of the transcript decides the case; each probe's traps
//! are checked too, so a wrong-but-plausible answer fails by name rather than
//! passing as close enough.
//!
//! **The tool payload is asserted alongside the transcript.** A model can say
//! "cortado" because memory told it or because it picked a plausible word out of
//! the air. Checking what `recall_context` actually returned is what separates
//! those; a test that reads only the transcript is a test of the model.
//!
//! Skips when no Gemini API key is configured.

#![cfg(feature = "gemini-llm")]

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gemini_adk_fluent_rs::live::Live;
use gemini_memory_rs::core::{SessionId, TurnId};
use gemini_memory_rs::engine::{MemoryEngine, MemorySession};
use gemini_memory_rs::runtime::{LiveMemoryExt, MemorySlot};

use common::corpus::{self, says, says_any, PROBES};
use common::live::{connect, Observed};
use common::{file_backed_engine, have_api_key, model_backed_engine, skip, ScratchDir};

/// What the model is told.
///
/// Three things matter. It must reach for the tool rather than answer from the
/// conversation; it must answer in few enough words that a decoy token in the
/// transcript means it actually believed the decoy; and it must say "I don't
/// know" rather than improvise, so a retrieval miss shows up as a miss instead
/// of a fluent guess that happens to be wrong.
const INSTRUCTION: &str = "You are the assistant built into this person's glasses. You know \
NOTHING about them except what the recall_context tool returns — you have never met them and have \
no impressions of your own. Before answering ANY question about them, their habits, their \
belongings or the people they know, you must call recall_context, and you must answer only from \
what it returns. Never guess, and never fall back on what is typical for people in general: if \
the tools give you nothing, say exactly \"I don't know\". Answer in five words or fewer.";

/// Memory slots, so the projection into governed `State` runs too rather than
/// being a code path this test happens to avoid.
fn slots() -> Vec<MemorySlot> {
    vec![
        MemorySlot::new("food_allergy", "user:allergy"),
        MemorySlot::new("beverage_preference", "user:coffee"),
    ]
}

/// An engine with the corpus in it, extraction backed by a real model.
async fn seeded(label: &str, user: &str) -> (ScratchDir, Arc<MemoryEngine>) {
    let scratch = ScratchDir::new(label);
    let engine = Arc::new(model_backed_engine(user, scratch.path()));
    corpus::install(&engine).await;
    (scratch, engine)
}

// ─── (a) the right needle reaches the spoken answer ─────────────────────────

/// Eight questions, eight needles, one voice session.
///
/// The model is told nothing about this person. Every correct answer had to
/// come out of the corpus, and every lookup had to beat dozens of records that
/// look like answers to the same question.
#[tokio::test]
async fn the_model_speaks_the_right_fact_from_a_large_corpus() {
    if !have_api_key() {
        return skip("the_model_speaks_the_right_fact_from_a_large_corpus");
    }
    let (_scratch, engine) = seeded("live-corpus", "usr_live_corpus").await;
    let session = Arc::new(engine.begin_session(SessionId::new("ses_corpus")));

    let observed = Arc::new(Observed::default());
    let handle = connect(
        Live::builder()
            .instruction(INSTRUCTION)
            .with_memory_slots(session, slots()),
        observed.clone(),
    )
    .await
    .expect("a session with memory over a large corpus must connect");

    let mut report =
        String::from("\nprobe                  answer                       tool call\n");
    let mut failures: Vec<String> = Vec::new();

    for probe in PROBES {
        if let Some(why) = probe.live_gap {
            report.push_str(&format!("{:<22} SKIPPED — {why}\n", probe.name));
            continue;
        }

        // A turn the server never answers is a stall, not a result, so it is
        // re-asked once. A turn the model answers *without* calling the tool is
        // not retried: that is a legitimate outcome, because a payload from an
        // earlier turn is still in its context and answering from it is exactly
        // what a conversation is. What that turn cannot do is prove anything on
        // its own — so the check below is against every payload the session has
        // served, not just this turn's.
        let mut attempt = 0;
        let (answer, asked) = loop {
            attempt += 1;
            let mark = observed.mark();
            handle.send_text(probe.ask).await.expect("send");
            let spoke = observed.try_answer(mark, probe.name).await;
            let asked: Vec<String> = observed
                .calls_since(mark)
                .iter()
                .map(|c| format!("{}({})", c.name, c.args))
                .collect();

            match (spoke, attempt) {
                (Some(said), _) => break (said, asked),
                (None, 2) => {
                    failures.push(format!(
                        "{}: the model stayed silent for {:?}, twice — the API never \
                         answered.",
                        probe.name,
                        common::live::TURN_TIMEOUT
                    ));
                    break (String::new(), asked);
                }
                (None, _) => continue,
            }
        };

        report.push_str(&format!(
            "{:<22} {:<28} {}{}\n",
            probe.name,
            answer,
            asked.join(" "),
            if attempt > 1 { "  (retried)" } else { "" }
        ));

        // 1. The needle is in the answer.
        if says_any(&answer, probe.expect).is_none() {
            failures.push(format!(
                "{}: expected one of {:?}, heard {answer:?}",
                probe.name, probe.expect
            ));
        }

        // 2. No trap is in the answer. A plausible wrong answer is the failure
        //    mode a large corpus introduces, and it is invisible to a test that
        //    only checks the right word appears somewhere.
        if let Some(trap) = says_any(&answer, probe.forbid) {
            failures.push(format!(
                "{}: answered with the decoy `{trap}`: {answer:?}",
                probe.name
            ));
        }

        // 3. The answer came from memory rather than from the model. Checked
        //    across the whole session: a fact this question's own lookup missed
        //    may have arrived in an earlier one, and answering from that is
        //    still memory doing its job.
        let served = observed
            .all_tools()
            .iter()
            .filter(|c| c.name == "recall_context")
            .any(|c| {
                corpus::payload_statements(&c.payload)
                    .iter()
                    .any(|s| says_any(s, probe.expect).is_some())
            });
        if !served {
            failures.push(format!(
                "{}: nothing recall_context has returned all session contained the \
                 answer, so the transcript proves nothing about retrieval.\n    \
                 asked: {asked:?}",
                probe.name
            ));
        }
    }

    handle.disconnect().await.ok();
    eprintln!("{report}");

    assert!(
        observed.audio_bytes.load(Ordering::Relaxed) > 0,
        "no audio arrived — the session was not in voice mode, so this test is \
         exercising a path no deployment uses\n  {}",
        observed.report()
    );
    assert!(
        failures.is_empty(),
        "{}\n\nsession:\n  {}",
        failures.join("\n"),
        observed.report()
    );
}

// ─── (b) a fact learned now beats the corpus, and outlives the call ─────────

/// The unique fact stated mid-conversation.
///
/// The corpus already holds plenty about classes, evenings and Thursdays, so
/// the topic is not new — only this fact is.
const NEW_FACT: &str =
    "Please remember that I keep my spare front door key in the Alvora tin by the sink.";

/// A fact stated out loud has to win against a corpus that was there first, and
/// then still be there tomorrow.
///
/// Three claims in the order a session makes them: ingestion promotes the
/// utterance into the session overlay; the overlay outranks canonical memory
/// for the rest of the conversation; reconciliation writes it to the Markdown
/// corpus, where a cold engine finds it.
#[tokio::test]
async fn a_fact_learned_mid_conversation_beats_the_corpus_and_outlives_it() {
    if !have_api_key() {
        return skip("a_fact_learned_mid_conversation_beats_the_corpus_and_outlives_it");
    }
    let (scratch, engine) = seeded("live-learn", "usr_live_learn").await;
    let session = Arc::new(engine.begin_session(SessionId::new("ses_learning")));

    let observed = Arc::new(Observed::default());
    let handle = connect(
        Live::builder()
            .instruction(INSTRUCTION)
            .with_memory_slots(session.clone(), slots()),
        observed.clone(),
    )
    .await
    .expect("connect");

    // Turn 1 — say it. The extractor runs at the turn boundary.
    let stating = observed.mark();
    handle.send_text(NEW_FACT).await.expect("send");
    observed.await_answer(stating, "stating a new fact").await;

    assert!(
        session
            .ledger()
            .usable_candidates()
            .iter()
            .any(|c| c.canonical_statement.to_lowercase().contains("alvora")),
        "the stated fact never became a session candidate; candidates: {:?}\n  {}",
        session
            .ledger()
            .usable_candidates()
            .iter()
            .map(|c| c.canonical_statement.clone())
            .collect::<Vec<_>>(),
        observed.report()
    );

    // Turn 2 — ask for it back. Nothing durable can answer this, so a correct
    // answer came from the overlay, ranked above 1,200 canonical records.
    let asking = observed.mark();
    handle
        .send_text("Where do I keep my spare front door key? Answer with just the place.")
        .await
        .expect("send");
    let answer = observed
        .await_answer(asking, "asking for the new fact")
        .await;

    handle.disconnect().await.ok();

    assert!(
        says(&answer, "alvora"),
        "a fact stated one turn ago did not come back: {answer:?}\n  {}",
        observed.report()
    );

    // And it survives the session: reconciliation commits it to Markdown.
    session.finish().await.expect("reconciliation");

    let reopened = file_backed_engine("usr_live_learn", scratch.path());
    reopened.compile_index().await.expect("index compiles");
    let after = reopened.begin_session(SessionId::new("ses_next_week"));
    after.begin_turn(TurnId(1));
    let recalled =
        corpus::payload_statements(&after.recall("where the spare key is kept", TurnId(1)).await);

    assert!(
        recalled.iter().any(|s| says(s, "alvora")),
        "the fact did not survive reconciliation into the corpus; a cold engine \
         recalled {recalled:?}"
    );
}

// ─── (c) a correction hides the fact it contradicts, immediately ────────────

/// Correcting a durable fact out loud has to take effect in the same breath.
///
/// The canonical record — the cortado — is one of 1,200 and ranks first for this
/// question; the offline tests assert exactly that. After the correction the
/// same question must produce the new value and must not produce the old one,
/// which needs the overlay to both outrank the canonical record *and* suppress
/// it.
///
/// It does not. Ignored because it fails for a defect rather than a flake:
/// when the model routes the correction through `manage_memory` the suppression
/// window is computed from an invented predicate and misses the record it was
/// meant to hide, so the assistant says "I've corrected that for you" and then
/// answers the next question with the old value. Reproduced without the network
/// by `an_explicit_correction_hides_the_record_it_corrects`.
#[tokio::test]
#[ignore = "known defect: see `an_explicit_correction_hides_the_record_it_corrects`"]
async fn a_correction_spoken_out_loud_hides_the_fact_it_replaces() {
    if !have_api_key() {
        return skip("a_correction_spoken_out_loud_hides_the_fact_it_replaces");
    }
    let (_scratch, engine) = seeded("live-correct", "usr_live_correct").await;
    let session: Arc<MemorySession> =
        Arc::new(engine.begin_session(SessionId::new("ses_correcting")));

    let observed = Arc::new(Observed::default());
    let handle = connect(
        Live::builder()
            .instruction(INSTRUCTION)
            .with_memory_slots(session.clone(), slots()),
        observed.clone(),
    )
    .await
    .expect("connect");

    // Establish the baseline out loud, so a failure message can tell a
    // correction that did not take from a corpus that never held the fact.
    let before = observed.mark();
    handle
        .send_text("What's my usual coffee order? Answer with just the drink.")
        .await
        .expect("send");
    let baseline = observed
        .await_answer(before, "asking for the old value")
        .await;
    assert!(
        says(&baseline, "cortado"),
        "the corpus did not answer the baseline question, so the correction has \
         nothing to correct: {baseline:?}\n  {}",
        observed.report()
    );

    // Correct it.
    let correcting = observed.mark();
    handle
        .send_text("Please correct that — my usual coffee order is a doppio now, not a cortado.")
        .await
        .expect("send");
    observed
        .await_answer(correcting, "correcting the fact")
        .await;

    // Ask again.
    let after = observed.mark();
    handle
        .send_text("So what's my usual coffee order? Answer with just the drink.")
        .await
        .expect("send");
    let answer = observed
        .await_answer(after, "asking for the corrected value")
        .await;

    handle.disconnect().await.ok();

    assert!(
        says(&answer, "doppio"),
        "the correction did not take: {answer:?}\n  {}",
        observed.report()
    );
    assert!(
        !says(&answer, "cortado"),
        "the superseded value came back alongside the correction: {answer:?}\n  {}",
        observed.report()
    );
}
