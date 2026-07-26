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
//! The fusion is [`reciprocal_rank_fusion`] from the crate itself — the same
//! function and the same `1/(60 + rank)` that `LocalMemoryRetriever` already
//! uses. A bespoke fusion would measure a thing we would then have to build.
//!
//! # What it found
//!
//! Over 61 questions and 1,199 records, answered by the top result:
//!
//! | strategy | top-1 | top-5 |
//! |---|---|---|
//! | lexical only | 29/61 | 40/61 |
//! | semantic, statement embedded | 30/61 | 33/61 |
//! | **semantic, enriched view embedded** | **43/61** | 46/61 |
//! | equal-weight RRF over the enriched view | 40/61 | 47/61 |
//! | **RRF with semantics at double weight** | 42/61 | **47/61** |
//! | gated — semantics only when lexical is thin | 34/61 | 45/61 |
//!
//! Three conclusions, in order of how much they matter.
//!
//! **What you embed matters more than how you search it.** Embedding the
//! statement alone is worth almost nothing over BM25 — 30 against 29. Embedding
//! the statement together with the aliases and tags that already sit beside it
//! in the record is worth 43. Same model, same index, same fusion; the only
//! change is the string handed to the embedder.
//!
//! **Gating is the wrong shape once the semantic side is good.** It was the
//! right answer when the two rankers were evenly matched — it fired on a ninth
//! of queries and cost nothing. Against an enriched semantic ranker it throws
//! away most of the gain, because the queries it declines to escalate are
//! exactly the ones where BM25 is confidently wrong rather than obviously
//! empty.
//!
//! **Equal-weight RRF dilutes the better ranker.** Fusing a 43 with a 29
//! produces a 40. Weighting semantics 2:1 recovers the top-1 to within one
//! question of semantic-alone while giving the best top-5 of any strategy —
//! and the top five is what the model actually reads, since `max_memories` is
//! 5. That is the configuration to build.
//!
//! # The caveat that matters
//!
//! The aliases in this corpus were written by the same hand as the questions,
//! so some of the enrichment gain is an alias containing its own question. The
//! extractor writes aliases in production, from a prompt that already asks for
//! "the words a FUTURE QUESTION would use" — so the mechanism is real, but the
//! honest version of 43/61 needs a corpus whose aliases were written without
//! sight of the query set. Treat the direction as established and the magnitude
//! as an upper bound.
//!
//! # Cost
//!
//! One embedding per record per representation, and one per question. Every
//! vector is cached to disk by content hash, so re-runs are free and the fusion
//! stays cheap to iterate on. Skips entirely without an API key.

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;
use std::time::Instant;

use common::corpus::{self, PROBES};
use common::paraphrase::{self, Mode, Tier};
use common::{file_backed_engine, have_api_key, skip, ScratchDir};

use gemini_memory_rs::bm25::{
    IndexedMemory, MemoryIndex, MemoryOrigin, Query, SearchExplanation, SearchHit,
};
use gemini_memory_rs::core::{stable_hash, CanonicalMemory, MemoryId, MemoryStatus};
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

/// The text a record is embedded as.
///
/// Not the statement alone, which is what the first version of this probe used
/// and what left `synonym` at 2/8. A statement is one short sentence written for
/// a model to *read aloud*; the fields that say what a fact might be *asked by*
/// live beside it. `retrieval.aliases` is documented as "paraphrases the fact
/// may be asked for by" and BM25 already weights it 2.5 — the embedding was
/// throwing it away.
///
/// A caveat this experiment cannot remove: the aliases in the fixture were
/// written by the same hand as the questions, so some gain here is the alias
/// containing the question. The extractor writes these in production, and the
/// honest version of this number needs a corpus whose aliases were written
/// without sight of the query set.
fn enriched(memory: &CanonicalMemory) -> String {
    let mut text = memory.statement.clone();
    if !memory.retrieval.aliases.is_empty() {
        text.push_str("\nAlso asked as: ");
        text.push_str(&memory.retrieval.aliases.join(", "));
    }
    if !memory.retrieval.tags.is_empty() {
        text.push_str("\nTopics: ");
        text.push_str(&memory.retrieval.tags.join(", "));
    }
    text
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

/// The four strategies, tallied along one axis.
#[derive(Default)]
struct Row {
    lexical: Tally,
    statement: Tally,
    enriched: Tally,
    fused: Tally,
    gated: Tally,
}

impl Row {
    fn line(&self, label: &str) -> String {
        format!(
            "{:<15} {:<6} {:<9} {:<11} {:<11} {:<11} {}\n",
            label,
            self.lexical.asked,
            self.lexical.cell(),
            self.statement.cell(),
            self.enriched.cell(),
            self.fused.cell(),
            self.gated.cell(),
        )
    }
}

#[tokio::test]
async fn what_a_semantic_layer_would_buy() {
    if !have_api_key() {
        return skip("what_a_semantic_layer_would_buy");
    }

    let scratch = ScratchDir::new("semantic-probe");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();
    let index = MemoryIndex::build(active.iter().map(|m| IndexedMemory::from_canonical(m)));

    // ── embed the corpus twice: as written, and as enriched ──
    let mut embedder = Embedder::new();
    let statements: Vec<String> = active.iter().map(|m| m.statement.clone()).collect();
    let enrichments: Vec<String> = active.iter().map(|m| enriched(m)).collect();
    let questions: Vec<String> = paraphrase::all()
        .map(|(_, p)| p.query.to_string())
        .collect();

    let started = Instant::now();
    embedder.embed_all(&statements, "RETRIEVAL_DOCUMENT").await;
    embedder.embed_all(&enrichments, "RETRIEVAL_DOCUMENT").await;
    embedder.embed_all(&questions, "RETRIEVAL_QUERY").await;
    let embed_time = started.elapsed();

    let vectors = |texts: &[String]| -> Vec<(MemoryId, String, Vec<f32>)> {
        active
            .iter()
            .zip(texts)
            .map(|(m, text)| {
                (
                    m.id.clone(),
                    m.statement.clone(),
                    embedder.get("RETRIEVAL_DOCUMENT", text).to_vec(),
                )
            })
            .collect()
    };
    let plain_vectors = vectors(&statements);
    let rich_vectors = vectors(&enrichments);

    // ── ask everything four ways ──
    let mut by_tier: Vec<Row> = (0..Tier::COUNT).map(|_| Row::default()).collect();
    let mut by_mode: Vec<Row> = (0..Mode::COUNT).map(|_| Row::default()).collect();
    let mut rescued: Vec<String> = Vec::new();
    let mut broken: Vec<String> = Vec::new();
    let mut gate_fired = 0usize;
    let mut weighted = Tally::default();
    let mut search_time = std::time::Duration::ZERO;
    let floor = gemini_memory_rs::core::RetrievalConfig::default().minimum_candidate_score;

    for (probe_name, phrasing) in paraphrase::all() {
        let probe = PROBES
            .iter()
            .find(|p| p.name == probe_name)
            .unwrap_or_else(|| panic!("no probe named {probe_name}"));
        let question = phrasing.query;
        let vector = embedder.get("RETRIEVAL_QUERY", question);

        let lexical_hits = lexical(&index, question);
        let plain_hits = semantic(vector, &plain_vectors);
        let started = Instant::now();
        let rich_hits = semantic(vector, &rich_vectors);
        search_time += started.elapsed();

        // The engine's own rule: reach for semantics only when lexical search
        // found too little, so a query BM25 answered confidently never pays for
        // a second opinion — and never loses to one.
        let thin = lexical_hits
            .first()
            .is_none_or(|top| top.score < floor * 2.0);
        if thin {
            gate_fired += 1;
        }
        let fused: Vec<SearchHit> =
            reciprocal_rank_fusion(&[lexical_hits.clone(), rich_hits.clone()])
                .into_iter()
                .map(|c| c.hit)
                .collect();
        let gated_hits = if thin { &fused } else { &lexical_hits };

        // Equal-weight RRF treats a strong ranker and a weak one alike. Passing
        // the semantic ranking twice doubles its reciprocal-rank contribution —
        // the cheapest way to ask whether the fusion wants weighting rather
        // than gating.
        let weighted_hits: Vec<SearchHit> =
            reciprocal_rank_fusion(&[lexical_hits.clone(), rich_hits.clone(), rich_hits.clone()])
                .into_iter()
                .map(|c| c.hit)
                .collect();
        weighted.record(rank_of(&weighted_hits, probe.target));

        let (l, s, e, f, g) = (
            rank_of(&lexical_hits, probe.target),
            rank_of(&plain_hits, probe.target),
            rank_of(&rich_hits, probe.target),
            rank_of(&fused, probe.target),
            rank_of(gated_hits, probe.target),
        );
        for row in [
            &mut by_tier[phrasing.tier.index()],
            &mut by_mode[phrasing.mode.index()],
        ] {
            row.lexical.record(l);
            row.statement.record(s);
            row.enriched.record(e);
            row.fused.record(f);
            row.gated.record(g);
        }

        let tag = format!(
            "[{}/{}] {question:?}",
            phrasing.tier.label(),
            phrasing.mode.label()
        );
        match (l == Some(0), f == Some(0)) {
            (false, true) => rescued.push(tag),
            (true, false) => broken.push(tag),
            _ => {}
        }
    }

    // ── report ──
    let asked = paraphrase::count();
    let header =
        "kind            asked  lexical   sem(plain)  sem(rich)   fused(rich) gated(rich)\n";
    let mut report = format!(
        "\nwhat a semantic layer buys, over {asked} questions\n\
         {} records embedded twice at {DIMENSIONS}d ({EMBEDDING_MODEL}); \
         {} API calls this run, {embed_time:.1?}\n\
         flat exact search over {} vectors: {:?} per question\n\
         the gate fired on {gate_fired}/{asked} questions\n\n\
         by how far the question sits from the record's own words\n{header}",
        active.len(),
        embedder.calls,
        active.len(),
        search_time / asked as u32,
    );
    for tier in Tier::ALL {
        report.push_str(&by_tier[tier.index()].line(tier.label()));
    }
    report.push_str(&format!(
        "\nby what the person was doing when they said it\n{header}"
    ));
    for mode in Mode::ALL {
        report.push_str(&by_mode[mode.index()].line(mode.label()));
    }

    let sum = |pick: fn(&Row) -> &Tally| {
        (
            by_tier.iter().map(|r| pick(r).first).sum::<usize>(),
            by_tier.iter().map(|r| pick(r).top_five).sum::<usize>(),
        )
    };
    let (lf, lt) = sum(|r| &r.lexical);
    let (sf, st) = sum(|r| &r.statement);
    let (ef, et) = sum(|r| &r.enriched);
    let (ff, ft) = sum(|r| &r.fused);
    let (gf, gt) = sum(|r| &r.gated);
    report.push_str(&format!(
        "\nanswered by the top result:  lexical {lf}/{asked}   sem(plain) {sf}/{asked}   \
         sem(rich) {ef}/{asked}   fused {ff}/{asked}   gated {gf}/{asked}\n\
         answer in the top five:      lexical {lt}/{asked}   sem(plain) {st}/{asked}   \
         sem(rich) {et}/{asked}   fused {ft}/{asked}   gated {gt}/{asked}\n\
         \nweighted RRF, semantics at double weight: {}/{asked} by the top result, \
         {}/{asked} in the top five.\n",
        weighted.first, weighted.top_five
    ));
    if !rescued.is_empty() {
        report.push_str(&format!(
            "\nrescued by unconditional fusion ({}):\n",
            rescued.len()
        ));
        for question in &rescued {
            report.push_str(&format!("  {question}\n"));
        }
    }
    if !broken.is_empty() {
        report.push_str(&format!(
            "\nBROKEN — lexical had these at rank 1 and fusion lost them ({}):\n",
            broken.len()
        ));
        for question in &broken {
            report.push_str(&format!("  {question}\n"));
        }
    }
    eprintln!("{report}");

    assert!(
        ff >= lf && gf >= lf,
        "a strategy answered fewer questions than lexical retrieval alone (fused \
         {ff}, gated {gf}, lexical {lf}) — an augmentation that costs you the queries \
         you already had is not an augmentation\n{report}"
    );
}
