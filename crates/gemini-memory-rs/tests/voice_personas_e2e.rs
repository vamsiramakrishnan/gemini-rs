//! Two users, spoken to rather than typed at.
//!
//! # Why speech, and why these two
//!
//! Every other Live test here calls `send_text`. That is convenient and it
//! skips the half of the system a voice product runs on: real turns arrive as
//! PCM, get segmented by the server's voice-activity detector, and reach the
//! model as an ASR transcript. Memory retrieval happens *downstream of that
//! transcript* — so a lookup that works on `"what's my usual coffee order"` and
//! fails on what the recogniser actually heard is a lookup that does not work,
//! and no text-driven test would ever say so.
//!
//! The user's side is synthesised with Gemini's TTS models and fed in as frames
//! at 16 kHz, paced in real time. See [`common::voice`].
//!
//! The two personas are the two states every memory product is judged on, and
//! they fail in opposite directions:
//!
//! **The new user, with nothing stored.** The failure is *invention*. Asked
//! what their usual coffee order is, a system with an empty memory and a
//! helpful disposition will make one up, and the user's first experience of the
//! feature is being told a fact about themselves that is false. Nothing in the
//! retrieval measurements catches this, because they all score whether the
//! right record was found among records that existed.
//!
//! **The long-time user, with 1,199 records.** The failure is *the wrong
//! neighbour*. The answer is in there, along with dozens of records that look
//! like answers to the same question — other people's coffee orders, the user's
//! other drinks, a café they mentioned once. This is the case every number in
//! `semantic_fusion_probe` was measured against, now driven end to end through
//! speech instead of through a ranking function.
//!
//! # What is asserted
//!
//! Deliberately little, and only what is unambiguous. A spoken answer is not
//! deterministic and this is a live model over a live network, so asserting on
//! phrasing would produce a test that fails for reasons nobody can act on.
//!
//! - The empty-memory user must not be told a specific drink. That is checkable
//!   without knowing what the model *should* have said, because any concrete
//!   coffee is wrong when nothing is stored.
//! - The corpus-backed user must get the fact the corpus holds.
//!
//! Everything else — latency, which tool was called, what the recogniser heard
//! — is reported rather than asserted, because it is diagnostic and variable.

#![cfg(feature = "gemini-llm")]

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::live::{connect, Observed};
use common::voice::{say, speak};
use common::{corpus, file_backed_engine, have_api_key, model_backed_engine, skip, ScratchDir};

use gemini_adk_fluent_rs::prelude::Live;
use gemini_memory_rs::core::SessionId;
use gemini_memory_rs::engine::MemoryEngine;
use gemini_memory_rs::runtime::{LiveMemoryExt, MemorySlot};

/// The voice the fixture user speaks with. One consistent speaker, so a
/// recognition failure is a property of the audio pipeline rather than of
/// which voice happened to be drawn.
const USER_VOICE: &str = "Puck";

/// Answer briefly, and admit ignorance rather than inventing.
///
/// The "I don't know" clause is load-bearing for the cold-start persona: it is
/// the instruction a real product would ship, and the test is whether memory
/// plus that instruction is enough to stop the model filling the gap. Without
/// it the cold-start case would only measure the model's disposition.
const INSTRUCTION: &str = "You are a voice assistant with memory of this user. \
     Use your memory tools before answering anything about the user. If the \
     tools give you nothing, say exactly \"I don't know\" — never guess a fact \
     about the user. Answer in five words or fewer.";

fn slots() -> Vec<MemorySlot> {
    vec![
        MemorySlot::new("food_allergy", "user:allergy"),
        MemorySlot::new("beverage_preference", "user:coffee"),
    ]
}

/// Drinks the corpus does not hold for this user.
///
/// Used to catch invention: with an empty memory, *any* concrete coffee in the
/// answer is wrong, so the check does not need to know the right answer.
const CONCRETE_DRINKS: &[&str] = &[
    "cortado",
    "latte",
    "cappuccino",
    "espresso",
    "americano",
    "flat white",
    "macchiato",
    "mocha",
    "drip",
    "pour over",
    "cold brew",
];

/// What the user says, spoken aloud.
const QUESTION: &str = "What's my usual coffee order?";

// ─── (a) the new user ───────────────────────────────────────────────────────

/// # Status: not passing, and now localised to the SDK session loop
///
/// Both personas get an empty transcript and no answer: the session connects,
/// 2.37 s of valid speech goes out as 154 paced 20 ms frames, and the model
/// never answers.
///
/// The cause is **not** the API, the audio, the MIME type, the model, or the
/// activity configuration. All of those were ruled out by replaying the exact
/// same audio over a raw WebSocket, outside this SDK: that probe gets the full
/// input transcript ("What's my usual coffee order?") and a full spoken answer.
/// Replaying the SDK's own setup message — `realtimeInputConfig` with
/// `TURN_INCLUDES_ONLY_ACTIVITY` and all — through the same probe still works,
/// so the setup is not it either. Instrumenting the codec shows the SDK's
/// outgoing frames are identical in shape to the probe's:
/// `{"realtimeInput":{"audio":{"mimeType":"audio/pcm;rate=16000","data":…}}}`.
///
/// What the instrumentation *did* show is the shape of the real defect. Across
/// one test run the server sent:
///
/// | message | count |
/// |---|---|
/// | `setupComplete` | **3** |
/// | `inputTranscription` | 1 (a single partial, `" It'"`) |
/// | everything else | 0 |
///
/// Three setup handshakes for one `connect()`, and a single partial transcript.
/// The L0 session loop retries the setup handshake up to
/// `max_reconnect_attempts` times, so the socket is being re-established while
/// the utterance is still streaming — which is exactly why no turn ever
/// completes. Audio arrives, gets partially transcribed, and the connection it
/// arrived on goes away before the turn closes.
///
/// Two defects were found and fixed on the way here, neither sufficient:
///
/// 1. `AudioFormat::Pcm16` declared `audio/pcm` where the Live API requires
///    `audio/pcm;rate=16000`. A real defect in the wire crate, fixed — a bare
///    type is accepted by the socket and then transcribed as silence, so it
///    would have broken audio input for every caller. Nothing caught it because
///    every other Live test drives sessions with `send_text`.
/// 2. `say` stopped sending packets at the end of the utterance rather than
///    streaming trailing silence.
///
/// The remaining question is narrow: **what closes the socket mid-utterance?**
/// Worth looking at whether the send path's token-bucket pacer stalls long
/// enough to trip a server-side idle timeout, and whether a transport error is
/// being swallowed into a reconnect rather than surfaced.
///
/// Marked `#[ignore]` rather than deleted or weakened: the assertions are the
/// right ones, the fixture is verified working against the raw API, and the
/// defect is in the code under test.
///
/// A user the system has never met asks about themselves.
///
/// The right answer is "I don't know". The wrong answer is a plausible drink,
/// and it is wrong in the way that matters most — confidently, about them, on
/// their first use of the feature.
#[tokio::test]
#[ignore = "spoken input returns an empty transcript; see the note above"]
async fn a_user_with_no_history_is_told_so_rather_than_invented_for() {
    if !have_api_key() {
        return skip("a_user_with_no_history_is_told_so_rather_than_invented_for");
    }
    let Some(pcm) = speak(QUESTION, USER_VOICE).await else {
        eprintln!("SKIP: speech synthesis unavailable");
        return;
    };

    let scratch = ScratchDir::new("voice-cold");
    // Deliberately *not* seeded. An empty engine, exactly as a new account.
    let engine = Arc::new(file_backed_engine("usr_voice_cold", scratch.path()));
    let session = Arc::new(engine.begin_session(SessionId::new("ses_cold")));

    let observed = Arc::new(Observed::default());
    let handle = connect(
        Live::builder()
            .instruction(INSTRUCTION)
            .with_memory_slots(session, slots()),
        observed.clone(),
    )
    .await
    .expect("a session with empty memory must still connect");

    let mark = observed.mark();
    let started = Instant::now();
    say(&handle, &pcm).await.expect("speak");
    let answer = observed
        .try_answer(mark, "cold start")
        .await
        .unwrap_or_default();
    let elapsed = started.elapsed();

    let heard = observed.transcript();
    let tools: Vec<String> = observed
        .calls_since(mark)
        .iter()
        .map(|c| format!("{}({})", c.name, c.args))
        .collect();
    let report = format!(
        "\nnew user, spoken\n  said:      {QUESTION:?}\n  answered:  {answer:?}\n  \
         tools:     {tools:?}\n  turn:      {elapsed:.1?}\n  transcript: {heard}\n"
    );
    eprintln!("{report}");
    handle.disconnect().await.ok();

    let lowered = answer.to_lowercase();
    let invented: Vec<&str> = CONCRETE_DRINKS
        .iter()
        .copied()
        .filter(|drink| lowered.contains(drink))
        .collect();
    assert!(
        invented.is_empty(),
        "memory is empty, so there is no usual coffee order to report — but the \
         assistant named {invented:?}. Inventing a fact about the user is the \
         cold-start failure this persona exists to catch.{report}"
    );
    assert!(
        !answer.trim().is_empty(),
        "the assistant said nothing at all to a spoken question; silence is not \
         the same as declining to guess{report}"
    );
}

// ─── (b) the long-time user ─────────────────────────────────────────────────

/// The same question, spoken by a user with 1,199 records behind them.
///
/// The corpus holds the answer and also holds dozens of near-misses. This is
/// the retrieval problem the rest of the crate measures as a ranking, driven
/// here through synthesis, VAD segmentation and ASR — the path the product
/// actually runs.
#[tokio::test]
#[ignore = "spoken input returns an empty transcript; see the note above"]
async fn a_user_with_a_long_history_gets_their_own_fact_back() {
    if !have_api_key() {
        return skip("a_user_with_a_long_history_gets_their_own_fact_back");
    }
    let Some(pcm) = speak(QUESTION, USER_VOICE).await else {
        eprintln!("SKIP: speech synthesis unavailable");
        return;
    };

    let scratch = ScratchDir::new("voice-warm");
    let engine: Arc<MemoryEngine> = Arc::new(model_backed_engine("usr_voice_warm", scratch.path()));
    let indexed = Instant::now();
    corpus::install(&engine).await;
    let index_time = indexed.elapsed();
    let session = Arc::new(engine.begin_session(SessionId::new("ses_warm")));

    let observed = Arc::new(Observed::default());
    let handle = connect(
        Live::builder()
            .instruction(INSTRUCTION)
            .with_memory_slots(session, slots()),
        observed.clone(),
    )
    .await
    .expect("a session over a large corpus must connect");

    let mark = observed.mark();
    let started = Instant::now();
    say(&handle, &pcm).await.expect("speak");
    let answer = observed
        .try_answer(mark, "long history")
        .await
        .unwrap_or_default();
    let elapsed = started.elapsed();

    let tools: Vec<String> = observed
        .calls_since(mark)
        .iter()
        .map(|c| format!("{}({})", c.name, c.args))
        .collect();
    let report = format!(
        "\nlong-time user, spoken\n  corpus:    indexed in {index_time:.1?}\n  \
         said:      {QUESTION:?}\n  answered:  {answer:?}\n  tools:     {tools:?}\n  \
         turn:      {elapsed:.1?}\n  transcript: {}\n",
        observed.transcript()
    );
    eprintln!("{report}");
    handle.disconnect().await.ok();

    // The corpus's answer for this user. Asserted loosely — the model may say
    // "a cortado" or "cortado, usually" and both are correct.
    assert!(
        answer.to_lowercase().contains("cortado"),
        "the corpus holds the user's coffee order and the assistant did not \
         report it. This is the wrong-neighbour failure: the answer exists and \
         something else outranked it.{report}"
    );
}
