//! Measured cost of the two model calls, which is where the real time is.
//!
//! Neither is on the voice path — plan extraction races a deadline and the
//! rule plan is already in hand; observation extraction runs after the turn is
//! over. But "off the path" is a claim about ordering, not about cost, and the
//! numbers decide whether the deadlines are set anywhere near right.
//!
//! ```text
//! cargo test -p gemini-memory-rs --features gemini-llm --test language_probe -- --nocapture
//! ```

#![cfg(feature = "gemini-llm")]

mod common;

use std::time::Instant;

use common::{ScratchDir, have_api_key, model_backed_engine, skip};
use gemini_memory_rs::core::{SessionId, TurnId};

const UTTERANCES: &[&str] = &[
    "I am vegetarian and I don't eat meat.",
    "My wife Rhea hates loud restaurants.",
    "Main vegetarian hoon, main meat nahi khata.",
    "Enakku filter coffee romba pidikkum, daily morning venum.",
    "I go to the gym before work every morning.",
    "मैं शाकाहारी हूँ, मैं मांस नहीं खाता।",
];

const QUESTIONS: &[&str] = &[
    "what do you remember about my dietary preferences",
    "where should we eat dinner tonight",
    "Mujhe yaad dilao, mera khaana ka preference kya hai?",
    "what does my wife like about restaurants",
];

fn report(label: &str, mut ms: Vec<f64>) {
    ms.sort_by(f64::total_cmp);
    let at = |p: f64| ms[((ms.len() as f64 * p) as usize).min(ms.len() - 1)];
    println!(
        "{label:<34} n={:<3} p50={:>7.0}ms p95={:>7.0}ms max={:>7.0}ms",
        ms.len(),
        at(0.50),
        at(0.95),
        *ms.last().unwrap()
    );
}

#[tokio::test]
async fn model_call_latency() {
    if !have_api_key() {
        return skip("model_call_latency");
    }
    let scratch = ScratchDir::new("latency");
    let engine = model_backed_engine("usr_latency", scratch.path());
    let session = engine.begin_session(SessionId::new("ses_1"));

    // Ingestion: transcript in, candidate observations out. Runs after the
    // user's turn completes, concurrent with the model speaking.
    let mut ingest = Vec::new();
    for (i, line) in UTTERANCES.iter().enumerate() {
        let turn = TurnId(i as u64 + 1);
        session.begin_turn(turn);
        let start = Instant::now();
        session.observe_final_transcript(turn, line).await.unwrap();
        ingest.push(start.elapsed().as_secs_f64() * 1e3);
        session.on_turn_complete(turn).await.unwrap();
    }
    session.finish().await.unwrap();
    engine.compile_index().await.unwrap();

    // Preparation: rule plan first, model plan racing a deadline behind it.
    let second = engine.begin_session(SessionId::new("ses_2"));
    let mut prepare = Vec::new();
    for (i, q) in QUESTIONS.iter().enumerate() {
        let turn = TurnId(i as u64 + 100);
        second.begin_turn(turn);
        let start = Instant::now();
        second.prepare(turn, q).await.unwrap();
        prepare.push(start.elapsed().as_secs_f64() * 1e3);
    }

    println!();
    report("observation extraction (async)", ingest);
    report("prepare incl. model plan (async)", prepare);
}
