//! What a semantic layer would buy, measured before building one.
//!
//! # The question
//!
//! `memory_paraphrase` establishes that lexical retrieval answers 42 of 93
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
//! So this asks all three at once — lexical alone, semantic alone, and the two
//! fused — over the same 93 questions and the same corpus.
//!
//! # The second question: what do you embed?
//!
//! An earlier run of this file established that *what* you embed matters more
//! than how you search it: the statement alone barely beat BM25, while the
//! statement plus the record's aliases and tags beat it by half again. But
//! those aliases were written by the same hand as the questions, so the number
//! was an upper bound rather than a measurement — an alias may simply have
//! contained its own question.
//!
//! This version separates the two by embedding each record six ways, and the
//! difference between them is *who wrote the text*:
//!
//! | view | what it embeds | authorship |
//! |---|---|---|
//! | `statement` | the sentence shown to the model | fixture |
//! | `curated` | statement + `retrieval.aliases` + `retrieval.tags` | fixture — **saw the query set** |
//! | `predicate` | statement + `Kind: Preference coffee order` | machine, from frontmatter |
//! | `structural` | `predicate` + subject, entities, scope | machine, from frontmatter |
//! | `generated` | statement + questions an LLM says it answers | model, **from the statement alone** |
//! | `full` | curated + structural + generated | mixed |
//!
//! `generated` is the authorship-clean one. A separate model is shown one
//! statement and asked what questions it answers; it never sees the corpus, the
//! probes, or a single line of [`common::paraphrase`]. Whatever it recovers is
//! recoverable in production, because an ingestion-time enricher has exactly
//! the same information.
//!
//! # What it found
//!
//! Over 93 questions and 1,199 records:
//!
//! | strategy | top-1 | top-5 | MRR |
//! |---|---|---|---|
//! | lexical only | 42/93 | 58/93 | 0.536 |
//! | semantic, statement | 41/93 | 48/93 | 0.486 |
//! | semantic, curated | 57/93 | 66/93 | 0.666 |
//! | semantic, predicate | 53/93 | 66/93 | 0.636 |
//! | **semantic, structural** | **66/93** | 73/93 | 0.748 |
//! | semantic, generated | 31/93 | 47/93 | 0.430 |
//! | semantic, full | 52/93 | 65/93 | 0.628 |
//! | RRF, lexical + structural | 61/93 | 79/93 | 0.734 |
//! | **RRF, structural at double weight** | 64/93 | **79/93** | **0.752** |
//! | RRF, generated at double weight | 38/93 | 57/93 | 0.509 |
//! | gated — semantics only when lexical is thin | 51/93 | 69/93 | 0.642 |
//!
//! **The frontmatter is the enrichment.** The best view costs nothing: no
//! author, no model call, no second pass at ingestion. It is the fields the
//! record already carries — subject, predicate, kind, entities, temporal scope
//! — written out as a few lines of prose and embedded alongside the statement.
//! It beats BM25 by half again (66 against 42), and it beats the hand-written
//! aliases that had the query set in view while they were written (57).
//!
//! The `predicate` row is the ablation that says where the gain comes from. A
//! statement gives the *value* — "The user's usual coffee order is a cortado" —
//! and only implies the *attribute*; a question asks by the attribute. Adding
//! the single line that names it is worth 12 of the 25 points (41 → 53), and
//! the subject, entities and scope lines are worth the other 13 (53 → 66). So
//! it is not that structured boilerplate helps the geometry — every one of
//! those fields carries retrievable signal that the statement had left implicit.
//!
//! **More context is not more signal.** The LLM-written view is the worst
//! strategy measured — 31/93, below the statement alone at 41 — and adding it
//! to everything else drags `full` (52) below `curated` (57). Six generated
//! questions run six times the length of the statement, and most of them are
//! generic: *book me a trim*, *remind me about my appointment*. Those collide
//! across a thousand records and drown the one sentence that distinguishes
//! them. This matters because "enrich each memory with an LLM" is the first
//! thing a reasonable person tries, and it is worse than doing nothing.
//!
//! **Fusion is still worth it, for the metric that matters.** Structural alone
//! has the best top-1 (66 against 64), but `max_memories` is 5 — the model
//! reads five records and picks, so the operative question is whether the
//! answer is in the top five at all. There, fusing BM25 back in is worth six
//! questions (79 against 73), and it has the best MRR of anything measured.
//! Weighting the stronger ranker 2:1 beats equal weight on top-1 by three and
//! ties it on top-5, so 2:1 over the structural view is the configuration to
//! build.
//!
//! Gating is the wrong shape (51/93, 69/93): the queries it declines to
//! escalate are exactly the ones where BM25 is confidently wrong rather than
//! obviously empty.
//!
//! What BM25 contributes is the exact-token case — `Fennelmark`, `Thornbury` —
//! which a dense retriever cannot represent and an inverted index gets for
//! free. It is not free in both directions: at 2:1 the `echo` tier slips from
//! 8/8 to 7/8, and "the user's barber" is one of four questions BM25 had at
//! rank 1 that fusion loses. Six recovered in the top five for four lost at
//! rank one is the trade, and it is the right one only because five records
//! reach the model rather than one.
//!
//! # Does the extra width earn its keep?
//!
//! 768 was picked off Google's own recommendation rather than anything measured
//! here. [`whether_wider_embeddings_earn_their_keep`] runs the winning
//! configuration — structural view, fused with BM25 at 2:1 — at four widths,
//! changing nothing else:
//!
//! | width | top-1 | top-5 | MRR | scan @1.2k | scan @16k | RAM @16k |
//! |---|---|---|---|---|---|---|
//! | 256 | 53/93 | 72/93 | 0.659 | 373µs | 5 ms | 16 MB |
//! | **768** | 64/93 | **79/93** | 0.752 | 1 ms | 15 ms | 49 MB |
//! | 1536 | 68/93 | 77/93 | 0.777 | 2 ms | 31 ms | 98 MB |
//! | 3072 | 67/93 | **79/93** | 0.778 | 5 ms | 61 ms | 196 MB |
//!
//! **3072 does not earn it.** Four times the memory and four times the scan for
//! the *same* 79 in the top five. The top-1 column moves by three or four
//! questions across 768, 1536 and 3072 — which on 93 questions is not a signal
//! anyone should spend 150 MB on — and top-5 is the metric that matters, since
//! `max_memories` is 5 and the model reads all five.
//!
//! Going the other way does cost. 256 gives up eleven questions at top-1 and
//! seven at top-5, so the width is doing real work below 768; it just stops
//! doing any above it.
//!
//! `serving_latency_probe` measures the embedding *call* as flat across widths
//! — 253ms at 768, 263ms at 3072 — so width is paid entirely in memory and scan
//! time, never in API latency. There is no latency argument for going narrow
//! and no quality argument for going wide. 768 stands.
//!
//! ## The finding that outlives the width question
//!
//! An exact flat scan stops fitting the 10 ms interactive budget somewhere
//! between 1,200 records and 16,000 — at *every* width except 256, which only
//! fits by giving up seven questions. That is the case for quantization or an
//! approximate index, and it is now a measurement rather than an assumption:
//! the goal is 768's quality at something nearer 256's scan cost.
//!
//! `quantization_probe` follows that through and finds it available: a packed
//! binary index over these same vectors scans in 63µs against float32's 1 ms,
//! and an exact rerank of its top 50 restores the float32 ranking exactly. At
//! 16,000 records that is 1.3 ms against the exact scan's 15.2 ms.
//!
//! # Why the fusion is the engine's own
//!
//! The fusion is [`reciprocal_rank_fusion`] from the crate itself — the same
//! function and the same `1/(60 + rank)` that `LocalMemoryRetriever` already
//! uses. A bespoke fusion would measure a thing we would then have to build.
//!
//! # Cost
//!
//! One embedding per record per view, one per question, and one flash-lite
//! completion per record for the `generated` view. Everything is cached to disk
//! by content hash, so re-runs are free and the fusion stays cheap to iterate
//! on. Skips entirely without an API key.

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;
use std::time::Instant;

use common::corpus::{self, PROBES};
use common::paraphrase::{self, Mode, Tier};
use common::views::{curated, predicate, structural, structural_view};
use common::{ScratchDir, file_backed_engine, have_api_key, skip};

use gemini_memory_rs::bm25::{
    IndexedMemory, MemoryIndex, MemoryOrigin, Query, SearchExplanation, SearchHit,
};
use gemini_memory_rs::core::{CanonicalMemory, MemoryId, MemoryStatus, stable_hash};
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

/// The model that writes the `generated` view.
///
/// Deliberately the cheapest one that can write a sentence: this runs once per
/// record at ingestion time in production, so if it only pays for itself at
/// flash prices it does not pay for itself.
const ENRICHMENT_MODEL: &str = "gemini-2.5-flash-lite";

/// How many records and questions to embed at once, and how many to enrich.
const EMBED_CONCURRENCY: usize = 16;
const ENRICH_CONCURRENCY: usize = 8;

/// How many results each retriever proposes before fusion.
const CANDIDATES: usize = 20;

// ─── what gets embedded ─────────────────────────────────────────────────────

/// One way of turning a record into the text handed to the embedder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    /// The statement alone: one sentence, written to be read aloud.
    Statement,
    /// Statement plus the aliases and tags the fixture author wrote. Carries
    /// the leakage caveat — these were written with the query set in view.
    Curated,
    /// Statement plus the one line of frontmatter that names the *attribute*:
    /// `Kind: Preference coffee order`. The ablation that separates "the
    /// predicate carries topical signal" from "any structured boilerplate
    /// helps", since [`View::Structural`] adds both at once.
    Predicate,
    /// Statement plus everything the frontmatter already knows: subject,
    /// predicate, kind, temporal scope, entities. Nobody wrote any of it for
    /// retrieval; it is the record's own structure, spelled out in words.
    Structural,
    /// Statement plus questions a model says it answers, written from the
    /// statement alone. The authorship-clean view.
    Generated,
    /// Everything at once.
    Full,
}

impl View {
    const ALL: [View; 6] = [
        View::Statement,
        View::Curated,
        View::Predicate,
        View::Structural,
        View::Generated,
        View::Full,
    ];
    const COUNT: usize = 6;

    fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).expect("view")
    }

    fn label(self) -> &'static str {
        match self {
            View::Statement => "statement",
            View::Curated => "curated",
            View::Predicate => "predicate",
            View::Structural => "structural",
            View::Generated => "generated",
            View::Full => "full",
        }
    }
}

/// The text a record is embedded as, for one view.
fn render(view: View, memory: &CanonicalMemory, generated: &HashMap<String, String>) -> String {
    let questions = || {
        generated
            .get(&memory.statement)
            .cloned()
            .unwrap_or_default()
    };
    let mut parts = vec![memory.statement.clone()];
    match view {
        View::Statement => {}
        View::Curated => parts.push(curated(memory)),
        View::Predicate => parts.push(predicate(memory)),
        View::Structural => parts.push(structural(memory)),
        View::Generated => parts.push(questions()),
        View::Full => {
            parts.push(curated(memory));
            parts.push(structural(memory));
            parts.push(questions());
        }
    }
    parts.retain(|p| !p.trim().is_empty());
    parts.join("\n")
}

// ─── talking to the API ─────────────────────────────────────────────────────

fn api_key() -> String {
    ["GEMINI_API_KEY", "GOOGLE_GENAI_API_KEY", "GOOGLE_API_KEY"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())
        .expect("an API key, checked by the caller")
}

/// A disk-backed cache of API results, keyed by content.
///
/// The point of the cache is that this experiment is meant to be re-run while
/// the fusion is tuned. Paying for six thousand embeddings and twelve hundred
/// completions once is cheap; paying every time would make people stop
/// measuring.
struct Cache<T> {
    entries: HashMap<String, T>,
    path: std::path::PathBuf,
    calls: usize,
}

impl<T: serde::Serialize + serde::de::DeserializeOwned> Cache<T> {
    fn open(name: &str) -> Self {
        // Beside the build directory rather than in a scratch dir: this is
        // meant to survive between runs.
        let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            entries,
            path,
            calls: 0,
        }
    }

    fn flush(&self) {
        if let Ok(raw) = serde_json::to_string(&self.entries) {
            let _ = std::fs::write(&self.path, raw);
        }
    }
}

/// One embedding call, with the retries an experiment needs and a production
/// client would do properly.
///
/// `task` is passed as `RETRIEVAL_DOCUMENT` for records and `RETRIEVAL_QUERY`
/// for questions, and on this model that does nothing: `gemini-embedding-2`
/// returns byte-identical vectors whatever is passed, while
/// `gemini-embedding-001` honours it. Pinned in
/// `query_rewrite_probe::task_type_is_a_no_op_on_gemini_embedding_2`.
///
/// Left in because it costs nothing, is correct on the older model, and would
/// resume working if this one ever honoured it. No measurement here is
/// affected — both sides passed the same ignored parameter.
async fn embed_one(
    client: &reqwest::Client,
    key: &str,
    text: &str,
    task: &str,
    dims: usize,
) -> Vec<f32> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{EMBEDDING_MODEL}:embedContent"
    );
    let body = serde_json::json!({
        "model": format!("models/{EMBEDDING_MODEL}"),
        "content": { "parts": [{ "text": text }] },
        "taskType": task,
        "outputDimensionality": dims,
    });
    let json = call(client, key, &url, &body, "embed").await;
    let values = json["embedding"]["values"]
        .as_array()
        .unwrap_or_else(|| panic!("no embedding in {json}"));
    normalize(
        values
            .iter()
            .filter_map(serde_json::Value::as_f64)
            .map(|v| v as f32),
    )
}

/// The prompt that writes the `generated` view.
///
/// It is shown one statement and nothing else. No corpus, no probe list, no
/// sight of the query set — which is the entire point, because that is exactly
/// what an ingestion-time enricher would have.
fn enrichment_prompt(statement: &str) -> String {
    format!(
        "A voice assistant remembers this fact about its user:\n\n{statement}\n\n\
         Write six short questions or requests this fact would answer, as the user \
         would actually say them out loud — including clipped ones, ones that name \
         an occasion instead of the topic, and ones that refer to the thing \
         indirectly. Use different words from the fact wherever you can. \
         One per line, no numbering, no other text."
    )
}

/// One completion, returning the raw text.
async fn enrich_one(client: &reqwest::Client, key: &str, statement: &str) -> String {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/\
         {ENRICHMENT_MODEL}:generateContent"
    );
    let body = serde_json::json!({
        "contents": [{ "role": "user", "parts": [{ "text": enrichment_prompt(statement) }] }],
        // No `temperature`. Gemini models are tuned for their own default and
        // pinning it — at 0 or anywhere else — measures a configuration nobody
        // should ship.
        "generationConfig": { "maxOutputTokens": 2048 },
    });
    let json = call(client, key, &url, &body, "enrich").await;
    json["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// A POST with backoff. Both endpoints rate-limit, and an experiment that dies
/// two thirds of the way through a corpus tells you nothing.
async fn call(
    client: &reqwest::Client,
    key: &str,
    url: &str,
    body: &serde_json::Value,
    what: &str,
) -> serde_json::Value {
    let mut backoff = std::time::Duration::from_millis(500);
    for attempt in 0..6 {
        let response = client
            .post(url)
            .header("x-goog-api-key", key)
            .json(body)
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                return response.json().await.expect("json body");
            }
            Ok(response) if attempt < 5 => {
                let status = response.status();
                let detail = response.text().await.unwrap_or_default();
                let detail: String = detail.chars().take(200).collect();
                eprintln!(
                    "{what} {status} (attempt {}), retrying: {detail}",
                    attempt + 1
                );
            }
            Ok(response) => panic!("{what} failed: {}", response.status()),
            Err(e) if attempt < 5 => eprintln!("{what} error (attempt {}): {e}", attempt + 1),
            Err(e) => panic!("{what} failed: {e}"),
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

// ─── the retrievers ─────────────────────────────────────────────────────────

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

fn fuse(rankings: &[&Vec<SearchHit>]) -> Vec<SearchHit> {
    let owned: Vec<Vec<SearchHit>> = rankings.iter().map(|r| (*r).clone()).collect();
    reciprocal_rank_fusion(&owned)
        .into_iter()
        .map(|c| c.hit)
        .collect()
}

// ─── the measurement ────────────────────────────────────────────────────────

/// The strategies, in report order. Views first, then what you can build from
/// them.
///
/// The fusions run over [`View::Structural`] rather than the richest view, and
/// the choice is not made by looking at the scores: structural is the only view
/// that costs nothing — no author, no model call, no second pass at ingestion —
/// so it is the one worth building whether or not it wins. That it also wins is
/// the finding.
///
/// `RRF 2:1 gen` is kept for the opposite reason. Fusing BM25 with the
/// LLM-written view is the thing a reasonable person tries first, and it is
/// worse than BM25 alone. A negative result nobody measures gets rediscovered
/// every six months.
const STRATEGIES: [&str; 11] = [
    "lexical",
    "sem: statement",
    "sem: curated",
    "sem: predicate",
    "sem: structural",
    "sem: generated",
    "sem: full",
    "RRF equal",
    "RRF 2:1 struct",
    "RRF 2:1 gen",
    "gated",
];
const LEXICAL: usize = 0;
const SEM: usize = 1; // .. SEM + View::COUNT
const RRF_EQUAL: usize = 7;
const RRF_STRUCT: usize = 8;
const RRF_GEN: usize = 9;
const GATED: usize = 10;

#[derive(Default, Clone, Copy)]
struct Tally {
    asked: usize,
    first: usize,
    top_five: usize,
    /// Summed reciprocal rank, for MRR.
    ///
    /// Top-1 is what the answer depends on and top-5 is what the model reads,
    /// but neither distinguishes rank 6 from rank 20 — and a fusion that moves
    /// answers from 14 to 6 is on its way somewhere, while one that leaves them
    /// at 14 is not.
    rr: f64,
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
        if let Some(r) = rank {
            self.rr += 1.0 / (r + 1) as f64;
        }
    }
    fn cell(&self) -> String {
        format!("{}/{}", self.first, self.asked)
    }
    fn mrr(&self) -> f64 {
        if self.asked == 0 {
            0.0
        } else {
            self.rr / self.asked as f64
        }
    }
}

/// Every strategy, tallied along one axis.
#[derive(Clone, Copy)]
struct Row {
    tallies: [Tally; STRATEGIES.len()],
}

impl Default for Row {
    fn default() -> Self {
        Self {
            tallies: [Tally::default(); STRATEGIES.len()],
        }
    }
}

/// The strategies the per-tier and per-mode tables show. The full grid is
/// unreadable at 12 modes; these are the ones that carry the argument.
const BREAKDOWN: [usize; 6] = [
    LEXICAL,
    SEM + 1, // curated
    SEM + 3, // structural
    SEM + 4, // generated
    SEM + 5, // full
    RRF_STRUCT,
];

impl Row {
    fn line(&self, label: &str) -> String {
        let mut out = format!("{:<15} {:<6}", label, self.tallies[LEXICAL].asked);
        for strategy in BREAKDOWN {
            out.push_str(&format!(" {:<11}", self.tallies[strategy].cell()));
        }
        out.push('\n');
        out
    }
}

fn breakdown_header() -> String {
    let mut out = format!("{:<15} {:<6}", "kind", "asked");
    for strategy in BREAKDOWN {
        out.push_str(&format!(" {:<11}", STRATEGIES[strategy]));
    }
    out.push('\n');
    out
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

    let client = reqwest::Client::new();
    let key = api_key();

    // ── write the generated view: one completion per record, from the
    //    statement alone ──
    let mut written: Cache<String> = Cache::open("semantic-probe-enrichment.json");
    let statements: Vec<String> = active.iter().map(|m| m.statement.clone()).collect();
    let enrich_started = Instant::now();
    let missing: Vec<String> = statements
        .iter()
        .filter(|s| !written.entries.contains_key(*s))
        .cloned()
        .collect();
    for chunk in missing.chunks(ENRICH_CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for statement in chunk {
            let (client, key, statement) = (client.clone(), key.clone(), statement.clone());
            set.spawn(async move {
                let text = enrich_one(&client, &key, &statement).await;
                (statement, text)
            });
        }
        while let Some(joined) = set.join_next().await {
            let (statement, text) = joined.expect("enrichment task");
            written.entries.insert(statement, text);
            written.calls += 1;
        }
        written.flush();
    }
    let enrich_time = enrich_started.elapsed();
    let generated = written.entries.clone();
    let blank = generated.values().filter(|v| v.trim().is_empty()).count();

    // ── embed the corpus once per view, and the questions once ──
    let mut vectors: Cache<Vec<f32>> = Cache::open("semantic-probe-embeddings.json");
    let cache_key = |task: &str, text: &str| {
        stable_hash(&format!("{EMBEDDING_MODEL}|{DIMENSIONS}|{task}|{text}"))
    };
    let views: Vec<Vec<String>> = View::ALL
        .iter()
        .map(|view| {
            active
                .iter()
                .map(|m| render(*view, m, &generated))
                .collect()
        })
        .collect();
    let questions: Vec<String> = paraphrase::all()
        .map(|(_, p)| p.query.to_string())
        .collect();

    let embed_started = Instant::now();
    let mut work: Vec<(String, String)> = Vec::new();
    for texts in &views {
        work.extend(
            texts
                .iter()
                .map(|t| ("RETRIEVAL_DOCUMENT".to_string(), t.clone())),
        );
    }
    work.extend(
        questions
            .iter()
            .map(|q| ("RETRIEVAL_QUERY".to_string(), q.clone())),
    );
    work.retain(|(task, text)| !vectors.entries.contains_key(&cache_key(task, text)));
    work.sort();
    work.dedup();
    for chunk in work.chunks(EMBED_CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for (task, text) in chunk {
            let (client, key, task, text) =
                (client.clone(), key.clone(), task.clone(), text.clone());
            set.spawn(async move {
                let vector = embed_one(&client, &key, &text, &task, DIMENSIONS).await;
                (task, text, vector)
            });
        }
        while let Some(joined) = set.join_next().await {
            let (task, text, vector) = joined.expect("embedding task");
            vectors.entries.insert(cache_key(&task, &text), vector);
            vectors.calls += 1;
        }
        vectors.flush();
    }
    let embed_time = embed_started.elapsed();

    let get = |task: &str, text: &str| -> &[f32] {
        vectors
            .entries
            .get(&cache_key(task, text))
            .map(Vec::as_slice)
            .unwrap_or_else(|| panic!("embedding missing for {task} {text:?}"))
    };
    let indexed: Vec<Vec<(MemoryId, String, Vec<f32>)>> = views
        .iter()
        .map(|texts| {
            active
                .iter()
                .zip(texts)
                .map(|(m, text)| {
                    (
                        m.id.clone(),
                        m.statement.clone(),
                        get("RETRIEVAL_DOCUMENT", text).to_vec(),
                    )
                })
                .collect()
        })
        .collect();

    // ── ask everything every way ──
    let mut by_tier: Vec<Row> = vec![Row::default(); Tier::COUNT];
    let mut by_mode: Vec<Row> = vec![Row::default(); Mode::COUNT];
    let mut overall = Row::default();
    let mut rescued: Vec<String> = Vec::new();
    let mut broken: Vec<String> = Vec::new();
    let mut gate_fired = 0usize;
    let mut search_time = std::time::Duration::ZERO;
    let floor = gemini_memory_rs::core::RetrievalConfig::default().minimum_candidate_score;

    for (probe_name, phrasing) in paraphrase::all() {
        let probe = PROBES
            .iter()
            .find(|p| p.name == probe_name)
            .unwrap_or_else(|| panic!("no probe named {probe_name}"));
        let question = phrasing.query;
        let vector = get("RETRIEVAL_QUERY", question);

        let lexical_hits = lexical(&index, question);
        let mut per_view: Vec<Vec<SearchHit>> = Vec::with_capacity(View::COUNT);
        for view in View::ALL {
            let started = Instant::now();
            per_view.push(semantic(vector, &indexed[view.index()]));
            search_time += started.elapsed();
        }
        let struct_hits = &per_view[View::Structural.index()];
        let gen_hits = &per_view[View::Generated.index()];

        // The engine's own rule: reach for semantics only when lexical search
        // found too little, so a query BM25 answered confidently never pays for
        // a second opinion — and never loses to one.
        let thin = lexical_hits
            .first()
            .is_none_or(|top| top.score < floor * 2.0);
        if thin {
            gate_fired += 1;
        }

        // Equal-weight RRF treats a strong ranker and a weak one alike. Passing
        // the semantic ranking twice doubles its reciprocal-rank contribution —
        // the cheapest way to ask whether the fusion wants weighting rather
        // than gating.
        let equal = fuse(&[&lexical_hits, struct_hits]);
        let weighted_struct = fuse(&[&lexical_hits, struct_hits, struct_hits]);
        let weighted_gen = fuse(&[&lexical_hits, gen_hits, gen_hits]);
        let gated = if thin { &equal } else { &lexical_hits };

        let mut ranks = [None; STRATEGIES.len()];
        ranks[LEXICAL] = rank_of(&lexical_hits, probe.target);
        for view in View::ALL {
            ranks[SEM + view.index()] = rank_of(&per_view[view.index()], probe.target);
        }
        ranks[RRF_EQUAL] = rank_of(&equal, probe.target);
        ranks[RRF_STRUCT] = rank_of(&weighted_struct, probe.target);
        ranks[RRF_GEN] = rank_of(&weighted_gen, probe.target);
        ranks[GATED] = rank_of(gated, probe.target);

        for row in [
            &mut by_tier[phrasing.tier.index()],
            &mut by_mode[phrasing.mode.index()],
            &mut overall,
        ] {
            for (tally, rank) in row.tallies.iter_mut().zip(ranks) {
                tally.record(rank);
            }
        }

        let tag = format!(
            "[{}/{}] {question:?}",
            phrasing.tier.label(),
            phrasing.mode.label()
        );
        match (ranks[LEXICAL] == Some(0), ranks[RRF_STRUCT] == Some(0)) {
            (false, true) => rescued.push(tag),
            (true, false) => broken.push(tag),
            _ => {}
        }
    }

    // ── report ──
    let asked = paraphrase::count();
    let mut report = format!(
        "\nwhat a semantic layer buys, over {asked} questions and {} records\n\
         {} views embedded at {DIMENSIONS}d ({EMBEDDING_MODEL}); \
         {} embedding calls this run, {embed_time:.1?}\n\
         {} enrichment completions ({ENRICHMENT_MODEL}), {enrich_time:.1?}, {blank} came back empty\n\
         flat exact search over {} vectors: {:?} per question per view\n\
         the gate fired on {gate_fired}/{asked} questions\n\n",
        active.len(),
        View::COUNT,
        vectors.calls,
        written.calls,
        active.len(),
        search_time / (asked * View::COUNT) as u32,
    );

    for view in View::ALL {
        let sample = render(view, active[0], &generated);
        report.push_str(&format!(
            "{:<11} {} chars/record, e.g. {:?}\n",
            view.label(),
            views[view.index()].iter().map(String::len).sum::<usize>() / active.len(),
            sample.replace('\n', " ⏎ "),
        ));
    }

    report.push_str(&format!(
        "\n{:<16} {:<9} {:<9} {}\n",
        "strategy", "top-1", "top-5", "MRR"
    ));
    for (i, name) in STRATEGIES.iter().enumerate() {
        let t = &overall.tallies[i];
        report.push_str(&format!(
            "{:<16} {:<9} {:<9} {:.3}\n",
            name,
            format!("{}/{asked}", t.first),
            format!("{}/{asked}", t.top_five),
            t.mrr(),
        ));
    }

    let header = breakdown_header();
    report.push_str(&format!(
        "\nby how far the question sits from the record's own words\n{header}"
    ));
    for tier in Tier::ALL {
        report.push_str(&by_tier[tier.index()].line(tier.label()));
    }
    report.push_str(&format!(
        "\nby what the person was doing when they said it\n{header}"
    ));
    for mode in Mode::ALL {
        report.push_str(&by_mode[mode.index()].line(mode.label()));
    }

    if !rescued.is_empty() {
        report.push_str(&format!(
            "\nrescued by weighted fusion over the structural view ({}):\n",
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

    // The negative result, said out loud rather than left in a table. If a
    // better enrichment prompt ever turns this line around, the module docs
    // above are stale and this is where you find out.
    let lex = overall.tallies[LEXICAL].first;
    let r#gen = overall.tallies[RRF_GEN].first;
    report.push_str(&format!(
        "\nfusing BM25 with the LLM-written view answers {gen}/{asked} against BM25's own \
         {lex}.\n\
         The generated text is six times the length of the statement and most of it is \
         generic —\n\"book me a trim\", \"remind me about my appointment\" — so it collides \
         across records and\ndrowns the one sentence that distinguishes them. More context \
         is not more signal.\n"
    ));
    eprintln!("{report}");

    for strategy in [SEM + View::Structural.index(), RRF_EQUAL, RRF_STRUCT, GATED] {
        assert!(
            overall.tallies[strategy].first >= lex,
            "{} answered {} of {asked} against lexical retrieval's {lex} — an \
             augmentation that costs you the queries you already had is not an \
             augmentation\n{report}",
            STRATEGIES[strategy],
            overall.tallies[strategy].first,
        );
    }

    // The metric the product actually runs on: `max_memories` is 5, so a
    // question whose answer is in the top five is a question the model can
    // answer. Guarding top-1 alone would let a change that quietly drops
    // answers from rank 4 to rank 9 pass.
    let built = &overall.tallies[RRF_STRUCT];
    let best_view = &overall.tallies[SEM + View::Structural.index()];
    assert!(
        built.top_five >= best_view.top_five && built.top_five > overall.tallies[LEXICAL].top_five,
        "the configuration this file recommends — RRF with the structural view at double \
         weight — put {} of {asked} answers in the top five, against {} for the structural \
         view alone and {} for BM25 alone. Fusing is only worth its second index while it \
         beats both.\n{report}",
        built.top_five,
        best_view.top_five,
        overall.tallies[LEXICAL].top_five,
    );
}

// ─── does the extra width earn its keep? ────────────────────────────────────

/// The widths worth comparing, and what each costs per record.
///
/// `gemini-embedding-2` is a Matryoshka model: the 3072-dimension vector is
/// trained so that its prefixes are themselves usable embeddings, and asking
/// for a narrower output is meant to cost quality gently rather than
/// catastrophically. "Gently" is a claim, and 768 was chosen off the back of
/// Google's own recommendation rather than anything measured here.
const SWEEP_WIDTHS: &[usize] = &[768, 256, 1536, 3072];

/// Whether paying four times the storage and four times the scan buys anything.
///
/// This runs the *winning* configuration — the structural view, fused with BM25
/// at 2:1 — at each width, over the same 93 questions and the same 1,199
/// records. Only the number of dimensions changes, so any difference is the
/// width and nothing else.
///
/// The costs it is being weighed against are concrete. At 16,000 records, which
/// `memory_at_scale` treats as a year of heavy use, `f32` vectors come to 16 MB
/// at 256d, 49 MB at 768d, 98 MB at 1536d and 197 MB at 3072d. An exact flat
/// scan is linear in the width, and `serving_latency_probe` measures the
/// embedding call itself as flat across widths — so the width is paid in memory
/// and in scan time, not in API latency.
#[tokio::test]
async fn whether_wider_embeddings_earn_their_keep() {
    if !have_api_key() {
        return skip("whether_wider_embeddings_earn_their_keep");
    }

    let scratch = ScratchDir::new("semantic-width");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();
    let index = MemoryIndex::build(active.iter().map(|m| IndexedMemory::from_canonical(m)));

    let client = reqwest::Client::new();
    let key = api_key();

    // The structural view needs no enrichment model, so this test is
    // independent of the flash-lite cache the main probe builds.
    let texts: Vec<String> = active.iter().map(|m| structural_view(m)).collect();
    let questions: Vec<String> = paraphrase::all()
        .map(|(_, p)| p.query.to_string())
        .collect();

    let mut vectors: Cache<Vec<f32>> = Cache::open("semantic-width-embeddings.json");
    let cache_key = |dims: usize, task: &str, text: &str| {
        stable_hash(&format!("{EMBEDDING_MODEL}|{dims}|{task}|{text}"))
    };

    let mut report = format!(
        "\ndoes width earn its keep? structural view fused with BM25 at 2:1,\n\
         {} questions over {} records, only the dimension count changing\n\n",
        questions.len(),
        active.len(),
    );
    report.push_str(&format!(
        "{:<8} {:<9} {:<9} {:<8} {:<12} {:<12} {:<9} {}\n",
        "width", "top-1", "top-5", "MRR", "scan@1.2k", "scan@16k", "RAM@16k", "vs 768 top-5"
    ));
    let mut measurements: Vec<(usize, usize, usize, std::time::Duration)> = Vec::new();

    let mut baseline_top_five = 0usize;
    for &dims in SWEEP_WIDTHS {
        // ── embed everything at this width, cached ──
        let mut work: Vec<(String, String)> = texts
            .iter()
            .map(|t| ("RETRIEVAL_DOCUMENT".to_string(), t.clone()))
            .chain(
                questions
                    .iter()
                    .map(|q| ("RETRIEVAL_QUERY".to_string(), q.clone())),
            )
            .collect();
        work.retain(|(task, text)| !vectors.entries.contains_key(&cache_key(dims, task, text)));
        work.sort();
        work.dedup();
        for chunk in work.chunks(EMBED_CONCURRENCY) {
            let mut set = tokio::task::JoinSet::new();
            for (task, text) in chunk {
                let (client, key, task, text) =
                    (client.clone(), key.clone(), task.clone(), text.clone());
                set.spawn(async move {
                    let vector = embed_one(&client, &key, &text, &task, dims).await;
                    (task, text, vector)
                });
            }
            while let Some(joined) = set.join_next().await {
                let (task, text, vector) = joined.expect("embedding task");
                vectors
                    .entries
                    .insert(cache_key(dims, &task, &text), vector);
                vectors.calls += 1;
            }
            vectors.flush();
        }

        let get = |task: &str, text: &str| -> &[f32] {
            vectors
                .entries
                .get(&cache_key(dims, task, text))
                .map(Vec::as_slice)
                .unwrap_or_else(|| panic!("embedding missing at {dims}d for {task} {text:?}"))
        };
        let indexed: Vec<(MemoryId, String, Vec<f32>)> = active
            .iter()
            .zip(&texts)
            .map(|(m, text)| {
                (
                    m.id.clone(),
                    m.statement.clone(),
                    get("RETRIEVAL_DOCUMENT", text).to_vec(),
                )
            })
            .collect();

        // ── ask everything, exactly as the recommended configuration would ──
        let mut tally = Tally::default();
        let mut scan_time = std::time::Duration::ZERO;
        for (probe_name, phrasing) in paraphrase::all() {
            let probe = PROBES
                .iter()
                .find(|p| p.name == probe_name)
                .unwrap_or_else(|| panic!("no probe named {probe_name}"));
            let vector = get("RETRIEVAL_QUERY", phrasing.query);

            let lexical_hits = lexical(&index, phrasing.query);
            let started = Instant::now();
            let semantic_hits = semantic(vector, &indexed);
            scan_time += started.elapsed();

            let fused = fuse(&[&lexical_hits, &semantic_hits, &semantic_hits]);
            tally.record(rank_of(&fused, probe.target));
        }

        let bytes_at_16k = 16_000u64 * dims as u64 * 4;
        let delta = if dims == 768 {
            baseline_top_five = tally.top_five;
            "baseline".to_string()
        } else if baseline_top_five == 0 {
            "—".to_string()
        } else {
            format!("{:+}", tally.top_five as i64 - baseline_top_five as i64)
        };
        // The scan is a full pass over every vector, so it is linear in both
        // the record count and the width. Projecting it to the corpus size
        // `memory_at_scale` treats as a year of heavy use is the number that
        // decides whether an exact scan is viable at all.
        let per_query = scan_time / tally.asked.max(1) as u32;
        let at_16k = per_query * (16_000 / active.len().max(1) as u32);
        measurements.push((dims, tally.first, tally.top_five, at_16k));
        report.push_str(&format!(
            "{:<8} {:<9} {:<9} {:<8.3} {:<12} {:<12} {:<9} {}\n",
            dims,
            format!("{}/{}", tally.first, tally.asked),
            format!("{}/{}", tally.top_five, tally.asked),
            tally.mrr(),
            format!("{per_query:.0?}"),
            format!("{at_16k:.0?}"),
            format!("{} MB", bytes_at_16k / 1_000_000),
            delta,
        ));
    }

    let interactive =
        gemini_memory_rs::core::RetrievalConfig::default().immediate_semantic_timeout_ms;
    report.push_str(&format!(
        "\nagainst the {interactive}ms interactive semantic budget, at 16,000 records:\n"
    ));
    for (dims, _, _, at_16k) in &measurements {
        report.push_str(&format!(
            "  {dims:<5} {:>8.0?}  {}\n",
            at_16k,
            if at_16k.as_millis() as u64 <= interactive {
                "fits"
            } else {
                "DOES NOT FIT — needs quantization or an approximate index"
            }
        ));
    }
    report.push_str(&format!(
        "\n{} embedding calls this run; the rest were cached.\n",
        vectors.calls
    ));
    eprintln!("{report}");

    // The claim the default rests on: above 768 the width stops buying
    // anything on the metric the product runs on. If a future embedding model
    // changes that, this fails and the default deserves revisiting.
    let best_top_five = measurements
        .iter()
        .map(|(_, _, t, _)| *t)
        .max()
        .unwrap_or(0);
    let at_768 = measurements
        .iter()
        .find(|(d, _, _, _)| *d == 768)
        .map(|(_, _, t, _)| *t)
        .expect("768 measured");
    assert!(
        at_768 + 1 >= best_top_five,
        "768d put {at_768} of {} answers in the top five while some wider setting \
         managed {best_top_five}. The default width is now leaving quality on the \
         table; the four-times storage and scan cost of 3072d may finally be \
         earning it.\n{report}",
        questions.len(),
    );
}
