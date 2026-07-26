//! What a semantic layer would buy, measured before building one.
//!
//! # The question
//!
//! `memory_paraphrase` establishes that lexical retrieval answers 17 of 40
//! questions when they are not phrased like the records that answer them, and
//! that recall tracks content-word overlap because overlap is the only signal
//! BM25 has. The obvious conclusion is "add embeddings" — but obvious is not
//! measured, and three things could go wrong that nobody would notice until
//! late:
//!
//! - embeddings might not recover the hard tiers either, because the corpus's
//!   statements are short and generic ("The user rides a Thornbury bicycle")
//!   and short text embeds poorly;
//! - they might recover the hard tiers and *lose* the easy ones, since a dense
//!   retriever has no notion of an exact rare token like `Fennelmark`;
//! - fusing the two might be worse than either, if one ranking is confident
//!   and wrong.
//!
//! So this asks all three questions at once: lexical alone, semantic alone, and
//! the two fused — over the same forty questions, against the same corpus.
//!
//! # Why the fusion is the engine's own
//!
//! The fusion here is [`reciprocal_rank_fusion`] from the crate itself, the
//! same function and the same `1/(60 + rank)` that `LocalMemoryRetriever` uses
//! to combine its lexical rankings today. A bespoke fusion would measure a
//! thing we would then have to build; this measures the thing that already
//! exists, so a good number transfers straight into `SemanticFallback` rather
//! than having to be re-earned.
//!
//! # Cost
//!
//! One embedding per record and per question — about 1,240 calls the first
//! time. Every vector is cached to disk keyed by a content hash, so re-runs
//! are free and the experiment stays cheap to iterate on. Skips entirely
//! without an API key.

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;
use std::time::Instant;

use common::corpus::{self, PROBES};
use common::paraphrase::{Tier, PHRASINGS};
use common::{file_backed_engine, have_api_key, skip, ScratchDir};

use gemini_memory_rs::bm25::{
    IndexedMemory, MemoryIndex, MemoryOrigin, Query, SearchExplanation, SearchHit,
};
use gemini_memory_rs::core::{stable_hash, MemoryId, MemoryStatus};
use gemini_memory_rs::retrieval::{deterministic::topical_terms, reciprocal_rank_fusion};

/// The embedding model, and the width its vectors are truncated to.
///
/// `gemini-embedding-2` is trained with Matryoshka representation learning, so
/// 3072 dimensions truncate to 768 without a linear quality drop, and outputs
/// below full width are normalized for us. 768 is the recommended efficient
/// width and cuts per-record storage fourfold — 12 MB rather than 49 MB for a
/// four-thousand-record corpus.
const EMBEDDING_MODEL: &str = "gemini-embedding-2";
const DIMENSIONS: usize = 768;

/// How many records and questions to embed at once.
const CONCURRENCY: usize = 16;

/// How many results each retriever proposes before fusion.
const CANDIDATES: usize = 20;

// ─── embedding ──────────────────────────────────────────────────────────────

/// A disk-backed cache of embeddings, keyed by task type and content.
///
/// The point of the cache is that this experiment is meant to be re-run while
/// the fusion is tuned. Paying for 1,240 embeddings once is cheap; paying every
/// time would make people stop measuring.
struct Embedder {
    client: reqwest::Client,
    key: String,
    cache: HashMap<String, Vec<f32>>,
    path: std::path::PathBuf,
    calls: usize,
}

impl Embedder {
    fn new() -> Self {
        let key = ["GEMINI_API_KEY", "GOOGLE_GENAI_API_KEY", "GOOGLE_API_KEY"]
            .iter()
            .find_map(|k| std::env::var(k).ok())
            .filter(|v| !v.trim().is_empty())
            .expect("an API key, checked by the caller");
        // Beside the build directory rather than in a scratch dir: this is
        // meant to survive between runs.
        let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("semantic-probe-embeddings.json");
        let cache = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            client: reqwest::Client::new(),
            key,
            cache,
            path,
            calls: 0,
        }
    }

    fn cache_key(task: &str, text: &str) -> String {
        stable_hash(&format!("{EMBEDDING_MODEL}|{DIMENSIONS}|{task}|{text}"))
    }

    /// Embed `texts`, filling the cache for anything not already held.
    ///
    /// `task` is `RETRIEVAL_DOCUMENT` for corpus statements and
    /// `RETRIEVAL_QUERY` for questions. The asymmetry matters: the model
    /// embeds a stored fact and a question about it into deliberately
    /// different points, and using one task type for both is the most common
    /// way to leave quality on the table.
    async fn embed_all(&mut self, texts: &[String], task: &str) {
        let missing: Vec<String> = texts
            .iter()
            .filter(|t| !self.cache.contains_key(&Self::cache_key(task, t)))
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }

        for chunk in missing.chunks(CONCURRENCY) {
            let mut set = tokio::task::JoinSet::new();
            for text in chunk {
                let (client, key, text, task) = (
                    self.client.clone(),
                    self.key.clone(),
                    text.clone(),
                    task.to_string(),
                );
                set.spawn(async move {
                    let vector = embed_one(&client, &key, &text, &task).await;
                    (Self::cache_key(&task, &text), vector)
                });
            }
            while let Some(joined) = set.join_next().await {
                let (key, vector) = joined.expect("embedding task");
                self.cache.insert(key, vector);
                self.calls += 1;
            }
        }

        if let Ok(raw) = serde_json::to_string(&self.cache) {
            let _ = std::fs::write(&self.path, raw);
        }
    }

    fn get(&self, task: &str, text: &str) -> &[f32] {
        self.cache
            .get(&Self::cache_key(task, text))
            .map(Vec::as_slice)
            .unwrap_or_else(|| panic!("embedding missing for {task} {text:?}"))
    }
}

/// One embedding call, with the retries an experiment needs and a production
/// client would do properly.
async fn embed_one(client: &reqwest::Client, key: &str, text: &str, task: &str) -> Vec<f32> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{EMBEDDING_MODEL}:embedContent"
    );
    let body = serde_json::json!({
        "model": format!("models/{EMBEDDING_MODEL}"),
        "content": { "parts": [{ "text": text }] },
        "taskType": task,
        "outputDimensionality": DIMENSIONS,
    });

    let mut backoff = std::time::Duration::from_millis(500);
    for attempt in 0..5 {
        let response = client
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                let json: serde_json::Value = response.json().await.expect("embedding json");
                let values = json["embedding"]["values"]
                    .as_array()
                    .unwrap_or_else(|| panic!("no embedding in {json}"));
                return normalize(values.iter().filter_map(|v| v.as_f64()).map(|v| v as f32));
            }
            Ok(response) if attempt < 4 => {
                let status = response.status();
                let detail = response.text().await.unwrap_or_default();
                eprintln!(
                    "embed {status} (attempt {}), retrying: {detail}",
                    attempt + 1
                );
            }
            Ok(response) => panic!("embedding failed: {}", response.status()),
            Err(e) if attempt < 4 => eprintln!("embed error (attempt {}): {e}", attempt + 1),
            Err(e) => panic!("embedding failed: {e}"),
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    unreachable!("loop either returns or panics")
}

/// L2-normalize, so an inner product is a cosine.
///
/// The model already normalizes below full width; doing it again is free and
/// removes a dependency on that staying true.
fn normalize(values: impl Iterator<Item = f32>) -> Vec<f32> {
    let mut vector: Vec<f32> = values.collect();
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// ─── the three retrievers ───────────────────────────────────────────────────

/// Rank by BM25, exactly as the engine does on the tool path.
fn lexical(index: &MemoryIndex, question: &str) -> Vec<SearchHit> {
    let topical: std::collections::HashSet<String> = topical_terms(question).into_iter().collect();
    let boost_only: Vec<String> = gemini_memory_rs::bm25::tokenize(question)
        .into_iter()
        .filter(|t| !topical.contains(t))
        .collect();
    index.search(
        &Query::new(question)
            .with_boost_only(boost_only)
            .with_limit(CANDIDATES),
        chrono::Utc::now(),
    )
}

/// Rank by embedding similarity: a flat scan, which at this corpus size is
/// exact and costs less than a millisecond.
fn semantic(
    question_vector: &[f32],
    corpus_vectors: &[(MemoryId, String, Vec<f32>)],
) -> Vec<SearchHit> {
    let mut scored: Vec<(f32, &MemoryId, &String)> = corpus_vectors
        .iter()
        .map(|(id, statement, vector)| (dot(question_vector, vector), id, statement))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(CANDIDATES);

    scored
        .into_iter()
        .map(|(score, id, statement)| SearchHit {
            id: id.clone(),
            score,
            statement: statement.clone(),
            kind: gemini_memory_rs::core::MemoryKind::Preference,
            origin: MemoryOrigin::Canonical,
            explanation: SearchExplanation {
                memory_id: id.clone(),
                components: Vec::new(),
                boosts: Vec::new(),
                lexical_score: 0.0,
                final_score: score,
            },
        })
        .collect()
}

/// Where the answer ranks, or `None` if it never appears.
fn rank_of(hits: &[SearchHit], target: &str) -> Option<usize> {
    hits.iter().position(|h| h.id.as_str() == target)
}

// ─── the measurement ────────────────────────────────────────────────────────

#[derive(Default, Clone, Copy)]
struct Tally {
    asked: usize,
    first: usize,
    top_five: usize,
}

impl Tally {
    fn record(&mut self, rank: Option<usize>) {
        self.asked += 1;
        match rank {
            Some(0) => {
                self.first += 1;
                self.top_five += 1;
            }
            Some(r) if r < 5 => self.top_five += 1,
            _ => {}
        }
    }
    fn cell(&self) -> String {
        format!("{}/{}", self.first, self.asked)
    }
}

#[tokio::test]
async fn what_a_semantic_layer_would_buy() {
    if !have_api_key() {
        return skip("what_a_semantic_layer_would_buy");
    }

    // ── the corpus, and an index over it ──
    let scratch = ScratchDir::new("semantic-probe");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<_> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();
    let index = MemoryIndex::build(active.iter().map(|m| IndexedMemory::from_canonical(m)));

    // ── embed everything, once ──
    let mut embedder = Embedder::new();
    let statements: Vec<String> = active.iter().map(|m| m.statement.clone()).collect();
    let questions: Vec<String> = PHRASINGS
        .iter()
        .flat_map(|p| p.queries.iter().map(|q| (*q).to_string()))
        .collect();

    let started = Instant::now();
    embedder.embed_all(&statements, "RETRIEVAL_DOCUMENT").await;
    embedder.embed_all(&questions, "RETRIEVAL_QUERY").await;
    let embed_time = started.elapsed();

    let corpus_vectors: Vec<(MemoryId, String, Vec<f32>)> = active
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                m.statement.clone(),
                embedder.get("RETRIEVAL_DOCUMENT", &m.statement).to_vec(),
            )
        })
        .collect();

    // ── ask everything three ways ──
    let mut lex: [Tally; 5] = Default::default();
    let mut sem: [Tally; 5] = Default::default();
    let mut fused: [Tally; 5] = Default::default();
    let mut gated: [Tally; 5] = Default::default();
    let mut gate_fired = 0usize;
    let mut rescued: Vec<String> = Vec::new();
    let mut broken: Vec<String> = Vec::new();
    let mut search_time = std::time::Duration::ZERO;

    for phrasings in PHRASINGS {
        let probe = PROBES
            .iter()
            .find(|p| p.name == phrasings.probe)
            .unwrap_or_else(|| panic!("no probe named {}", phrasings.probe));

        for (tier, question) in Tier::ALL.iter().zip(phrasings.queries.iter()) {
            let i = Tier::ALL.iter().position(|t| t == tier).expect("tier");

            let lexical_hits = lexical(&index, question);
            let started = Instant::now();
            let semantic_hits =
                semantic(embedder.get("RETRIEVAL_QUERY", question), &corpus_vectors);
            search_time += started.elapsed();

            // The engine's own fusion, over the two rankings.
            let fused_hits: Vec<SearchHit> =
                reciprocal_rank_fusion(&[lexical_hits.clone(), semantic_hits.clone()])
                    .into_iter()
                    .map(|c| c.hit)
                    .collect();

            // The engine's own rule: reach for semantics only when lexical
            // search found too little. `needs_semantic_fallback` calls that
            // "every candidate below twice the score floor", so a query BM25
            // answered confidently never pays for a second opinion — and never
            // loses to one.
            let floor = gemini_memory_rs::core::RetrievalConfig::default().minimum_candidate_score;
            let thin = lexical_hits
                .first()
                .is_none_or(|top| top.score < floor * 2.0);
            if thin {
                gate_fired += 1;
            }
            let gated_hits = if thin { &fused_hits } else { &lexical_hits };

            let (l, s, f, g) = (
                rank_of(&lexical_hits, probe.target),
                rank_of(&semantic_hits, probe.target),
                rank_of(&fused_hits, probe.target),
                rank_of(gated_hits, probe.target),
            );
            lex[i].record(l);
            sem[i].record(s);
            fused[i].record(f);
            gated[i].record(g);

            match (l == Some(0), g == Some(0)) {
                (false, true) => rescued.push(format!("[{}] {question:?}", tier.label())),
                (true, false) => broken.push(format!("[{}] {question:?}", tier.label())),
                _ => {}
            }
        }
    }

    // ── report ──
    let mut report = format!(
        "\nwhat a semantic layer buys, on the questions lexical retrieval loses\n\
         {} records embedded at {DIMENSIONS}d ({EMBEDDING_MODEL}), {} API calls this run, \
         {embed_time:.1?}\n\
         flat search over {} vectors: {:?} per question\n\n\
         tier          asked  lexical  semantic  fused    gated\n",
        corpus_vectors.len(),
        embedder.calls,
        corpus_vectors.len(),
        search_time / 40,
    );
    for (i, tier) in Tier::ALL.iter().enumerate() {
        report.push_str(&format!(
            "{:<13} {:<6} {:<8} {:<9} {:<8} {}\n",
            tier.label(),
            lex[i].asked,
            lex[i].cell(),
            sem[i].cell(),
            fused[i].cell(),
            gated[i].cell(),
        ));
    }

    let total = |t: &[Tally; 5]| {
        (
            t.iter().map(|x| x.first).sum::<usize>(),
            t.iter().map(|x| x.top_five).sum::<usize>(),
            t.iter().map(|x| x.asked).sum::<usize>(),
        )
    };
    let (lf, lt, asked) = total(&lex);
    let (sf, st, _) = total(&sem);
    let (ff, ft, _) = total(&fused);
    let (gf, gt, _) = total(&gated);
    report.push_str(&format!(
        "\nanswered by the top result:  lexical {lf}/{asked}   semantic {sf}/{asked}   \
         fused {ff}/{asked}   gated {gf}/{asked}\n\
         answer in the top five:      lexical {lt}/{asked}   semantic {st}/{asked}   \
         fused {ft}/{asked}   gated {gt}/{asked}\n\
         the gate fired on {gate_fired}/{asked} questions — the rest never paid for a \
         second opinion.\n"
    ));
    if !rescued.is_empty() {
        report.push_str(&format!(
            "\nrescued by the gated strategy ({}):\n",
            rescued.len()
        ));
        for question in &rescued {
            report.push_str(&format!("  {question}\n"));
        }
    }
    if !broken.is_empty() {
        report.push_str(&format!(
            "\nBROKEN — lexical had these at rank 1 and the gated strategy lost them ({}):\n",
            broken.len()
        ));
        for question in &broken {
            report.push_str(&format!("  {question}\n"));
        }
    }
    eprintln!("{report}");

    // The experiment exists to produce a number, so it asserts only the thing
    // that would make the number meaningless: fusion must not be worse than
    // the lexical retriever it is supposed to be augmenting. Everything else
    // is reported for a human to judge.
    assert!(
        gf >= lf,
        "the gated strategy answered fewer questions than lexical retrieval alone \
         ({gf} vs {lf}) — a fallback that costs you the queries you already answered \
         is not a fallback\n{report}"
    );
}
