//! What happens to every layer when the user contradicts what memory holds.
//!
//! # Why this file exists
//!
//! A correction is the highest-value event a memory system sees. The user cared
//! enough to say "no, actually" — and if the system gets it wrong it does not
//! just fail to help, it confidently repeats something the user has explicitly
//! denied. That is the worst failure the product has.
//!
//! `memory_lifecycle_e2e` already checks the headline: a correction supersedes
//! rather than duplicating. This file asks the question underneath it, which is
//! *how far the correction propagates*. There are four places a fact lives:
//!
//! | layer | what holds the fact | updated by |
//! |---|---|---|
//! | resolution | `ResolutionKind::Supersede` on the proposal | `Resolver` |
//! | durable | the OKF Markdown on disk | `MemoryCommitter` |
//! | lexical | the BM25 inverted index | `recompile_canonical` |
//! | semantic | the embedding vectors | `SemanticFallback::reconcile` |
//!
//! Three of those four already had a reconciliation path. The fourth did not
//! until this file went looking, which mattered because
//! [`PrecomputedSemanticIndex`] had just made the semantic layer something this
//! crate ships rather than something a caller supplies.
//!
//! [`PrecomputedSemanticIndex`]: gemini_memory_rs::retrieval::PrecomputedSemanticIndex
//!
//! # The two failure modes, and why only one of them is guarded
//!
//! **Serving the stale fact.** The user corrected their coffee order and the
//! assistant still says cortado. Guarded, and by construction rather than by
//! diligence: `semantic_ranking` resolves every id the backend returns against
//! the canonical index and drops anything not `is_retrievable`. A superseded
//! record is gone from that index after recompile, so a stale id from a stale
//! vector store resolves to nothing and is filtered. [`a_stale_semantic_index_
//! cannot_serve_a_superseded_fact`] pins that, because it is the property that
//! makes an out-of-date vector store *safe* rather than merely inaccurate.
//!
//! **Losing the new fact.** The correction is committed, indexed lexically and
//! written to disk — and if the semantic layer never hears about it, paraphrased
//! recall cannot find it. This was unguarded when the file was written: the
//! vector store was built once, and no hook, revision check or trait method told
//! it anything had changed. It degraded *quietly and in one direction* — never
//! lying, just gradually not knowing things, starting with the facts a user had
//! cared enough to correct.
//!
//! [`SemanticFallback::reconcile`] closed it. The engine calls it from
//! `recompile_canonical`, immediately after reconciliation has decided what is
//! true, handing over the whole active set rather than a diff.
//! [`a_correction_reaches_the_semantic_index_and_the_old_fact_leaves_it`] is
//! that test, now inverted into the guarantee it was written to argue for.
//!
//! Extraction from free-form prose needs a model, so these skip without an API
//! key. The bundled deterministic extractors serve the scale tests, which
//! install pre-built records rather than parsing sentences.

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{have_api_key, model_backed_engine, skip, ScratchDir};

use gemini_memory_rs::core::{CanonicalMemory, MemoryId, MemoryStatus, SessionId, TurnId};
use gemini_memory_rs::retrieval::{
    embedding_text, PrecomputedSemanticIndex, SemanticFallback, StaticEmbedder,
};

/// The fact, and the correction that contradicts it.
const ORIGINAL: &str = "My usual coffee order is a cortado.";
const CORRECTION: &str = "Actually I switched, my usual coffee order is a flat white now.";

/// Drive one turn the way the runtime does.
async fn say(session: &gemini_memory_rs::engine::MemorySession, turn: u64, utterance: &str) {
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

/// Every record the repository holds, in whatever status.
async fn records_of(engine: &gemini_memory_rs::engine::MemoryEngine) -> Vec<CanonicalMemory> {
    engine
        .repository()
        .all(engine.user())
        .await
        .expect("the repository must be readable")
}

/// State a fact in one session, contradict it in the next.
///
/// Two sessions rather than two turns, because reconciliation runs when a
/// session is sealed: the correction has to arrive as new evidence against an
/// already-committed fact, which is the situation the product is in.
async fn state_then_contradict(
    engine: &gemini_memory_rs::engine::MemoryEngine,
) -> Vec<CanonicalMemory> {
    let first = engine.begin_session(SessionId::new("ses_original"));
    say(&first, 1, ORIGINAL).await;
    first.finish().await.expect("sealing the first session");
    engine.compile_index().await.expect("compiling the index");

    let second = engine.begin_session(SessionId::new("ses_correction"));
    say(&second, 1, CORRECTION).await;
    second.finish().await.expect("sealing the correction");
    engine.compile_index().await.expect("recompiling the index");

    records_of(engine).await
}

/// A deterministic embedder over the exact texts a test needs.
///
/// Vectors are seeded from the text, so two different statements are far apart
/// and the same statement always lands in the same place. That is enough for a
/// ranking to be meaningful without a network call.
fn embedder_over(texts: &[String]) -> Arc<StaticEmbedder> {
    let mut table = HashMap::new();
    for text in texts {
        table.insert(text.clone(), seeded_vector(text));
    }
    Arc::new(StaticEmbedder::new(table))
}

fn seeded_vector(text: &str) -> Vec<f32> {
    let mut state = gemini_memory_rs::core::stable_hash(text)
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64))
        | 1;
    let mut out: Vec<f32> = (0..64)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state as i32 as f32) / (i32::MAX as f32)
        })
        .collect();
    let norm = out.iter().map(|v| v * v).sum::<f32>().sqrt();
    for value in &mut out {
        *value /= norm;
    }
    out
}

// ─── (a) the resolution and the durable record ──────────────────────────────

/// What a contradiction does to the record set and to the files on disk.
///
/// Asserted together because they are one question: the OKF Markdown *is* the
/// durable record, so "was it superseded" and "did the file change" cannot have
/// different answers without something being badly wrong.
#[tokio::test]
async fn a_contradiction_supersedes_the_old_fact_and_rewrites_it_on_disk() {
    if !have_api_key() {
        return skip("a_contradiction_supersedes_the_old_fact_and_rewrites_it_on_disk");
    }
    let scratch = ScratchDir::new("contradiction-durable");
    let engine = model_backed_engine("usr_contradiction", scratch.path());
    let records = state_then_contradict(&engine).await;

    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();
    let superseded: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Superseded)
        .collect();

    let on_disk = read_all_markdown(scratch.path());
    let report = format!(
        "\ncontradiction, durable layer\n  records:    {} total, {} active, {} superseded\n  \
         active:     {:?}\n  superseded: {:?}\n  disk:       {} bytes of Markdown, \
         mentions cortado: {}, mentions flat white: {}\n",
        records.len(),
        active.len(),
        superseded.len(),
        active.iter().map(|m| &m.statement).collect::<Vec<_>>(),
        superseded.iter().map(|m| &m.statement).collect::<Vec<_>>(),
        on_disk.len(),
        on_disk.to_lowercase().contains("cortado"),
        on_disk.to_lowercase().contains("flat white"),
    );
    eprintln!("{report}");

    assert!(
        active
            .iter()
            .any(|m| m.statement.to_lowercase().contains("flat white")),
        "the correction is not active — the user's newer statement did not \
         become the fact of record{report}"
    );
    assert!(
        !active
            .iter()
            .any(|m| m.statement.to_lowercase().contains("cortado")),
        "the contradicted fact is still active alongside its own correction. \
         Both cannot be true, and serving both is how the assistant ends up \
         reciting a fact the user has explicitly denied.{report}"
    );
    // The superseded record is *retained*, not deleted. That is deliberate: it
    // is the evidence trail for why the current value is what it is, and the
    // thing a "what did I used to drink" question reads.
    assert!(
        !superseded.is_empty(),
        "nothing was superseded — the correction was recorded without marking \
         what it replaced, so the history of the value is lost{report}"
    );
    assert!(
        on_disk.to_lowercase().contains("flat white"),
        "the correction never reached the OKF Markdown. Memory that does not \
         survive a restart is a cache.{report}"
    );
}

// ─── (b) the lexical layer ──────────────────────────────────────────────────

/// Recall after a contradiction returns the correction and not the original.
///
/// This is the layer that works, and it works because `recompile_canonical`
/// rebuilds the BM25 index from active records only — supersession removes a
/// record from the index rather than merely marking it.
#[tokio::test]
async fn lexical_recall_after_a_contradiction_serves_only_the_correction() {
    if !have_api_key() {
        return skip("lexical_recall_after_a_contradiction_serves_only_the_correction");
    }
    let scratch = ScratchDir::new("contradiction-lexical");
    let engine = model_backed_engine("usr_contradiction", scratch.path());
    state_then_contradict(&engine).await;

    let asking = engine.begin_session(SessionId::new("ses_ask"));
    asking.begin_turn(TurnId(3));
    let payload = asking
        .recall("what is my usual coffee order", TurnId(3))
        .await;
    let facts = common::corpus::payload_statements(&payload);

    let report = format!("\ncontradiction, lexical layer\n  recalled: {facts:?}\n");
    eprintln!("{report}");

    assert!(
        facts
            .iter()
            .any(|f| f.to_lowercase().contains("flat white")),
        "recall did not return the corrected value{report}"
    );
    assert!(
        !facts.iter().any(|f| f.to_lowercase().contains("cortado")),
        "recall served the superseded value; the model would speak it{report}"
    );
}

// ─── (c) the semantic layer: the safety property ────────────────────────────

/// A semantic index built before the correction cannot serve the stale fact.
///
/// This is the property that makes an out-of-date vector store *safe*. The
/// backend returns ids and nothing else; the retriever resolves each one
/// against the canonical index and drops whatever is no longer retrievable. So
/// a vector store that still points at a superseded record cannot put that
/// record in front of the model — the id simply fails to resolve.
///
/// Worth pinning rather than assuming, because the alternative design — a
/// backend that returns statements directly — would have no such check, and the
/// difference between the two is whether staleness is an inaccuracy or a lie.
#[tokio::test]
async fn a_stale_semantic_index_cannot_serve_a_superseded_fact() {
    if !have_api_key() {
        return skip("a_stale_semantic_index_cannot_serve_a_superseded_fact");
    }
    let scratch = ScratchDir::new("contradiction-stale-safe");
    let engine = model_backed_engine("usr_contradiction", scratch.path());

    // Index built from the corpus *before* the correction.
    let first = engine.begin_session(SessionId::new("ses_pre"));
    say(&first, 1, ORIGINAL).await;
    first.finish().await.expect("seal");
    engine.compile_index().await.expect("compile");
    let before = records_of(&engine).await;
    assert!(
        !before.is_empty(),
        "extraction produced no records from {ORIGINAL:?}; this test measures \
         what a correction does to an existing fact, so there has to be one"
    );
    let texts: Vec<String> = before.iter().map(embedding_text).collect();
    let index = PrecomputedSemanticIndex::from_vectors(
        before
            .iter()
            .zip(&texts)
            .map(|(m, t)| (m.id.clone(), t.clone(), seeded_vector(t)))
            .collect(),
        embedder_over(&texts),
    );
    let stale_ids: Vec<MemoryId> = before.iter().map(|m| m.id.clone()).collect();

    // Now contradict it. The index is not told.
    let second = engine.begin_session(SessionId::new("ses_post"));
    say(&second, 1, CORRECTION).await;
    second.finish().await.expect("seal");
    engine.compile_index().await.expect("compile");
    let after = records_of(&engine).await;

    // The index still returns the old id.
    let still_returned = index.search_vector(&seeded_vector(&texts[0]), 5);
    let superseded: Vec<&CanonicalMemory> = after
        .iter()
        .filter(|m| m.status == MemoryStatus::Superseded)
        .collect();

    // But recall does not serve it.
    let asking = engine.begin_session(SessionId::new("ses_ask"));
    asking.begin_turn(TurnId(3));
    let payload = asking
        .recall("what is my usual coffee order", TurnId(3))
        .await;
    let facts = common::corpus::payload_statements(&payload);

    let report = format!(
        "\ncontradiction, semantic staleness (safety)\n  \
         index still holds: {} ids, including {:?}\n  \
         now superseded:    {:?}\n  recall served:     {facts:?}\n",
        stale_ids.len(),
        still_returned
            .iter()
            .map(|i| i.as_str())
            .collect::<Vec<_>>(),
        superseded.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
    );
    eprintln!("{report}");

    assert!(
        !facts.iter().any(|f| f.to_lowercase().contains("cortado")),
        "a stale semantic index put a superseded fact in front of the model. \
         The id-resolution check in `semantic_ranking` is what should have \
         prevented this, and it is the only thing standing between an \
         out-of-date vector store and the assistant repeating something the \
         user explicitly corrected.{report}"
    );
}

// ─── (d) the semantic layer: the gap ────────────────────────────────────────

/// A correction reaches the semantic index, and the superseded fact leaves it.
///
/// This test was originally written the other way round. It demonstrated that
/// the vector store was built once and never told anything had changed — no
/// hook, no revision check, no trait method — so paraphrased recall silently
/// stopped working for exactly the facts a user had bothered to correct.
///
/// [`SemanticFallback::reconcile`] is the path that closed it. The engine calls
/// it from `recompile_canonical`, right after reconciliation has decided what
/// is true, and hands over the entire active set rather than a diff. The
/// backend embeds what it does not hold, drops what is no longer active, and
/// leaves the rest alone.
///
/// Both directions are asserted, because only checking that the new fact
/// arrived would pass on a backend that appends forever and never forgets.
#[tokio::test]
async fn a_correction_reaches_the_semantic_index_and_the_old_fact_leaves_it() {
    if !have_api_key() {
        return skip("a_correction_reaches_the_semantic_index_and_the_old_fact_leaves_it");
    }
    let scratch = ScratchDir::new("contradiction-resync");
    let engine = model_backed_engine("usr_contradiction", scratch.path());

    let first = engine.begin_session(SessionId::new("ses_pre"));
    say(&first, 1, ORIGINAL).await;
    first.finish().await.expect("seal");
    engine.compile_index().await.expect("compile");
    let before = records_of(&engine).await;
    assert!(
        !before.is_empty(),
        "extraction produced no records from {ORIGINAL:?}; this test measures \
         what a correction does to an existing fact, so there has to be one"
    );

    // Every text the engine could ask to embed, before and after — the static
    // embedder answers only for texts it was given, so a missing one surfaces
    // as an error rather than as a silently wrong vector.
    let original_id = before[0].id.clone();
    let original_text = embedding_text(&before[0]);

    let second = engine.begin_session(SessionId::new("ses_post"));
    say(&second, 1, CORRECTION).await;
    second.finish().await.expect("seal");
    let after = records_of(&engine).await;
    let correction = after
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .find(|m| m.statement.to_lowercase().contains("flat white"))
        .expect("the correction must have been committed");

    let all_texts: Vec<String> = after.iter().map(embedding_text).collect();
    let index = PrecomputedSemanticIndex::from_vectors(
        vec![(
            original_id.clone(),
            original_text.clone(),
            seeded_vector(&original_text),
        )],
        embedder_over(&all_texts),
    );
    let held_before: Vec<String> = vec![original_id.as_str().to_string()];

    // What the engine hands the backend after a commit.
    let active: Vec<(MemoryId, String)> = after
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .map(|m| (m.id.clone(), embedding_text(m)))
        .collect();
    // Revision `0` is the documented escape for a caller with nothing to order
    // by: this index is built here and reconciled once, so there is no second
    // snapshot that could arrive stale.
    index.reconcile(&active, 0).await.expect("reconcile");

    // Query by the correction's own text: it can only be found if it was
    // embedded and added.
    let found = index.search_vector(&seeded_vector(&embedding_text(correction)), 5);
    let found_ids: Vec<&str> = found.iter().map(|id| id.as_str()).collect();

    let report = format!(
        "\ncontradiction, semantic resync\n  held before:   {held_before:?}\n  \
         active after:  {:?}\n  held after:    {found_ids:?} ({} vectors)\n  \
         superseded id: {:?}\n",
        active.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        index.len(),
        original_id.as_str(),
    );
    eprintln!("{report}");

    assert!(
        found_ids.contains(&correction.id.as_str()),
        "the correction is still not in the semantic index after reconcile — \
         paraphrased recall cannot reach the fact the user just corrected{report}"
    );
    assert!(
        !found_ids.contains(&original_id.as_str()),
        "the superseded record is still in the semantic index. It cannot be \
         *served* — `semantic_ranking` drops ids that no longer resolve — but \
         it is occupying a candidate slot that a live record should have, so \
         reconcile is adding without retiring{report}"
    );
    assert_eq!(
        index.len(),
        active.len(),
        "the index and the active corpus disagree on how many facts exist{report}"
    );
}

// ─── (e) the vocabulary the model filters by ────────────────────────────────

/// The memory map tracks the corpus, including facts learned mid-conversation.
///
/// `recall_context` takes `about` and `attribute`, and those are worth nothing
/// unless the model can name the values. Measured, a model asked cold names the
/// right predicate 2% of the time — below the 8% at which a soft filter starts
/// paying for itself — and 69% when shown this list.
///
/// The map is delivered as an instruction amendment rather than in the tool
/// schema, because Live fixes tool declarations at connect while the corpus
/// keeps growing. This asserts the consequence of that choice: a fact committed
/// after connect is filterable, not merely retrievable.
#[tokio::test]
async fn the_memory_map_names_the_values_a_correction_introduces() {
    if !have_api_key() {
        return skip("the_memory_map_names_the_values_a_correction_introduces");
    }
    let scratch = ScratchDir::new("contradiction-map");
    let engine = model_backed_engine("usr_contradiction", scratch.path());

    let asking = engine.begin_session(SessionId::new("ses_map"));
    let before = asking.memory_map();
    assert!(
        before.is_empty(),
        "a user with no memory has no vocabulary to filter by, and offering an \
         empty list invites the model to narrow by nothing: {before:?}"
    );

    state_then_contradict(&engine).await;

    let after = asking.memory_map();
    let report = format!("\ncontradiction, memory map\n  before: {before:?}\n  after:  {after}\n");
    eprintln!("{report}");

    assert!(
        after.contains("about:"),
        "the map must name the subjects that exist{report}"
    );
    assert!(
        after.contains("attribute:"),
        "the map must name the predicates that exist{report}"
    );
    // The correction's predicate is what a follow-up question would filter by.
    let records = records_of(&engine).await;
    let live = records
        .iter()
        .find(|m| m.status == MemoryStatus::Active)
        .expect("an active record");
    assert!(
        after.contains(live.predicate.as_str()),
        "the active record's predicate {:?} is missing from the map, so the \
         model cannot filter by the fact it just learned{report}",
        live.predicate.as_str()
    );
}

/// Concatenate every Markdown file under a directory.
fn read_all_markdown(root: &std::path::Path) -> String {
    let mut out = String::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
                out.push('\n');
            }
        }
    }
    out
}
