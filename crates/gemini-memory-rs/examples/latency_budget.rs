//! Where the time actually goes.
//!
//! Measures the two paths separately, because only one of them is on the voice
//! path and the other one's cost is nearly irrelevant:
//!
//! - **Synchronous** (per turn, blocks nothing but must finish before the next
//!   user send): plan → BM25 → fuse → assemble.
//! - **Index compilation** (after a session, off the path): read the corpus,
//!   tokenize every record, build the inverted index.
//!
//! ```text
//! cargo run -p gemini-memory-rs --release --example latency_budget
//! ```
//! Release matters — the debug numbers are ~20× worse and mean nothing.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;

use gemini_memory_rs::bm25::{IndexedMemory, MemoryIndex};
use gemini_memory_rs::core::{
    CanonicalMemory, CanonicalPredicate, EntityRef, EvidenceCounters, Explicitness, MemoryId,
    MemoryKind, MemorySource, MemoryStatus, MemoryValue, PrivacyMetadata, RetrievalConfig,
    RetrievalMetadata, SessionId, TemporalMetadata, TemporalScope, TurnId, UserId,
};
use gemini_memory_rs::retrieval::{
    DeterministicPlanner, IndexHandle, KnownEntities, LocalMemoryRetriever, MemoryRetriever,
    RetrievalRequest,
};

/// A corpus that looks like a real one: varied subjects, predicates and tags.
fn corpus(n: usize) -> Vec<CanonicalMemory> {
    const SUBJECTS: &[&str] = &["user", "Rhea", "Kushal", "Anaya", "Dev"];
    const TOPICS: &[&str] = &[
        "vegetarian food and dietary restrictions",
        "loud restaurants and quiet cafes",
        "filter coffee every morning",
        "the gym before work each day",
        "window seats on long flights",
        "spicy south indian breakfast",
        "weekend trips to the hills",
        "reading before bed",
    ];
    (0..n)
        .map(|i| {
            let subject = SUBJECTS[i % SUBJECTS.len()];
            let topic = TOPICS[i % TOPICS.len()];
            let statement = format!("{subject} prefers {topic} (record {i}).");
            CanonicalMemory {
                id: MemoryId::new(format!("mem_{i:05}")),
                owner: UserId::new("usr_bench"),
                kind: if i % 3 == 0 {
                    MemoryKind::Preference
                } else {
                    MemoryKind::Identity
                },
                predicate: CanonicalPredicate::new(format!("prefers_{}", i % 40)),
                status: MemoryStatus::Active,
                confidence: 0.9,
                subject: EntityRef::named(subject),
                value: MemoryValue::Text(topic.into()),
                statement: statement.clone(),
                evidence_summary: "stated by the user".into(),
                source: MemorySource::from_explicitness(
                    Explicitness::ExplicitStatement,
                    SessionId::new("ses_1"),
                    TurnId(1),
                ),
                temporal: TemporalMetadata::created_at(Utc::now()),
                retrieval: RetrievalMetadata {
                    subject: gemini_memory_rs::core::normalize_token(subject),
                    tags: topic.split_whitespace().map(str::to_string).collect(),
                    ..Default::default()
                },
                evidence: EvidenceCounters::first(),
                privacy: PrivacyMetadata::default(),
                temporal_scope: TemporalScope::Persistent,
                supersedes: Vec::new(),
                superseded_by: None,
                qualifier: None,
            }
        })
        .collect()
}

const QUERIES: &[&str] = &[
    "what do you remember about my dietary preferences",
    "where should we eat dinner tonight",
    "what does Rhea like about restaurants",
    "Mujhe yaad dilao, mera khaana ka preference kya hai?",
    "do you remember what I said about the gym",
];

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

fn stats(mut samples: Vec<f64>) -> (f64, f64, f64) {
    samples.sort_by(f64::total_cmp);
    (
        percentile(&samples, 0.50),
        percentile(&samples, 0.95),
        *samples.last().unwrap(),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("corpus  stage                        p50 (µs)   p95 (µs)   max (µs)");
    println!("{}", "-".repeat(72));

    for size in [10usize, 100, 1_000, 10_000] {
        let records = corpus(size);

        // ── Index compilation, off the voice path ───────────────────────────
        let mut build = Vec::new();
        for _ in 0..if size >= 10_000 { 5 } else { 30 } {
            let start = Instant::now();
            let index = MemoryIndex::build(records.iter().map(IndexedMemory::from_canonical));
            build.push(start.elapsed().as_secs_f64() * 1e6);
            std::hint::black_box(index.len());
        }
        let (p50, p95, max) = stats(build);
        println!("{size:<8}index build (async)      {p50:>10.0} {p95:>10.0} {max:>10.0}");

        // ── The synchronous per-turn path ───────────────────────────────────
        let index = MemoryIndex::build(records.iter().map(IndexedMemory::from_canonical));
        let known = KnownEntities::from_index(&index);
        let planner = DeterministicPlanner::with_entities(known);
        let canonical = Arc::new(IndexHandle::new());
        canonical.replace(index);
        let retriever = LocalMemoryRetriever::new(
            canonical,
            Arc::new(IndexHandle::new()),
            RetrievalConfig::default(),
        );

        let (mut plan_us, mut prep_us) = (Vec::new(), Vec::new());
        for round in 0..40u64 {
            for (q, text) in QUERIES.iter().enumerate() {
                // A distinct turn each time so the prepared-query cache is not
                // measured instead of the search.
                let turn = TurnId(round * 100 + q as u64);
                let start = Instant::now();
                let plan = planner.plan(text, turn, 1, Utc::now());
                plan_us.push(start.elapsed().as_secs_f64() * 1e6);

                let start = Instant::now();
                let snapshot = retriever.prepare(RetrievalRequest::new(plan)).await?;
                prep_us.push(start.elapsed().as_secs_f64() * 1e6);
                std::hint::black_box(snapshot.facts.len());
                retriever.invalidate_cache();
            }
        }
        let (p50, p95, max) = stats(plan_us);
        println!("{size:<8}  plan (rules, sync)     {p50:>10.1} {p95:>10.1} {max:>10.1}");
        let (p50, p95, max) = stats(prep_us);
        println!("{size:<8}  search+fuse+assemble   {p50:>10.1} {p95:>10.1} {max:>10.1}");
        println!();
    }
    Ok(())
}
