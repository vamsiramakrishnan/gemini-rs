//! The whole memory lifecycle, offline.
//!
//! Runs two conversations against an in-process engine and prints what the
//! model would have been given at each turn, then the canonical Markdown the
//! sessions left behind. No credentials, no network — this is the pipeline, not
//! the Live session.
//!
//! ```text
//! cargo run -p gemini-memory-rs --example memory_pipeline
//! ```

use std::sync::Arc;

use gemini_memory_rs::okf::{MemoryStore, OkfRepository, OkfStore};
use gemini_memory_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), MemoryError> {
    let store = Arc::new(MemoryStore::new());
    let engine = MemoryEngine::new(
        UserId::new("usr_72ab"),
        Arc::new(OkfRepository::new(store.clone())),
        Arc::new(gemini_memory_rs::core::InMemoryEventLog::new()),
        gemini_memory_rs::core::MemoryRuntimeConfig::default(),
    );

    println!("── session 1 ─────────────────────────────────────────────");
    let first = engine.begin_session(SessionId::new("ses_monday"));
    converse(
        &first,
        &[
            "I am vegetarian",
            "I always go to the gym before work",
            "what do you remember about my dietary preferences",
        ],
    )
    .await?;
    let report = first.finish().await?;
    println!(
        "\nreconciled: {} created, {} reinforced, {} superseded\n",
        report.creates, report.reinforces, report.supersedes
    );

    engine.compile_index().await?;

    println!("── session 2 ─────────────────────────────────────────────");
    let second = engine.begin_session(SessionId::new("ses_thursday"));
    converse(
        &second,
        &[
            // A correction. The old fact should be retired, not duplicated.
            "actually I am pescatarian",
            "I always go to the gym before work",
            "what do you remember about my dietary preferences",
        ],
    )
    .await?;
    let report = second.finish().await?;
    println!(
        "\nreconciled: {} created, {} reinforced, {} superseded\n",
        report.creates, report.reinforces, report.supersedes
    );

    println!("── canonical memory ──────────────────────────────────────");
    for path in store.paths() {
        if !path.ends_with(".md") {
            continue;
        }
        println!("\n\x1b[1m{path}\x1b[0m");
        if let Some(contents) = store.read(&path).await? {
            println!("{contents}");
        }
    }

    Ok(())
}

/// Drive one conversation, printing the context each turn would have received.
async fn converse(session: &MemorySession, turns: &[&str]) -> Result<(), MemoryError> {
    for (idx, utterance) in turns.iter().enumerate() {
        let turn = TurnId(idx as u64 + 1);
        session.begin_turn(turn);

        println!("\nuser: {utterance}");
        session.observe_final_transcript(turn, utterance).await?;

        let snapshot = session.prepare(turn, utterance).await?;
        if snapshot.is_empty() {
            println!("      (no memory needed)");
        } else {
            for fact in snapshot.facts.iter() {
                println!("      ↳ {}", fact.presented_statement());
            }
            println!("      [{} tokens]", snapshot.token_count);
        }

        session.on_turn_complete(turn).await?;
    }
    Ok(())
}
