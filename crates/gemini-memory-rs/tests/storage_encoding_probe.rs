//! What halving the vector store costs, measured rather than assumed.
//!
//! # The question
//!
//! [`OkfVectorStore`] used to write each 768-dimension vector as `f32`
//! little-endian bytes in lowercase hex: 6,144 characters, 6,161 bytes with the
//! text hash and newline, **98.6 MB at 16,000 records**. `memory_at_scale`
//! measures the OKF Markdown those vectors accompany at 20,259 KiB for the same
//! corpus, so the vectors were about 4.75× the records they describe. The store
//! dominated the footprint rather than merely adding to it.
//!
//! Two encodings would shrink it, and the doc comment on `OkfVectorStore` names
//! both without pricing either:
//!
//! - **base64 instead of hex** — 4 characters per 3 bytes instead of 2 per 1.
//!   Exactly lossless, a third off, costs one dependency.
//! - **`f16` instead of `f32`** — half the bytes again, at *some* precision
//!   loss. How much loss was never established, which is the gap this file
//!   closes.
//!
//! "Some precision loss" is not a number. `f16` carries 10 explicit mantissa
//! bits against `f32`'s 23, so a coordinate keeps roughly 3 decimal digits
//! instead of 7. Whether that costs a single question depends entirely on what
//! the stored floats are *used for*, and in this crate they are used for
//! exactly two things.
//!
//! # Where the stored floats are actually read
//!
//! [`PrecomputedSemanticIndex`] holds each record twice — sign bits packed 64
//! to a word for the scan, and the full vector for the exact rerank of the top
//! [`RERANK_DEPTH`] — and **both are derived from the stored vector at load**.
//! So a lossy store touches both, and it touches them very differently:
//!
//! - **The packed codes are sign bits.** `f16` preserves sign exactly for every
//!   value it can represent, so the scan is bit-identical — except where a
//!   coordinate is small enough to underflow `f16`'s smallest subnormal
//!   (2⁻²⁴ ≈ 6e-8) and lands on `-0.0`, which `pack` reads as non-negative
//!   because `-0.0 >= 0.0`. That is a real flip, and it is counted below rather
//!   than argued away.
//! - **The rerank is a dot product against the stored floats.** This is where
//!   rounding can actually reorder anything, and it is the whole risk.
//!
//! Which means the measurement has a narrow, checkable shape: the shortlist
//! should be unchanged, and any damage should show up as reordering *within*
//! the fifty candidates the rerank scores.
//!
//! # How it is measured
//!
//! Through the shipping code at both ends, so that nothing here can be right
//! about a reimplementation and wrong about the product. The lossy vectors come
//! from an actual [`OkfVectorStore`] — saved and loaded over an in-memory
//! backing store — and both sets are searched through the crate's own
//! [`PrecomputedSemanticIndex`] with `search_vector`, exactly as the retriever
//! calls it. Two indexes over the same 1,199 records, one from the cached `f32`
//! vectors and one from what the store hands back.
//!
//! The query vector stays `f32` in both, because a query is embedded at recall
//! time and never stored; only the document side goes through the store.
//!
//! Scored on the same 93 questions as `semantic_fusion_probe` and
//! `quantization_probe`, and reported both semantic-only and fused with BM25 at
//! 2:1 — the configuration the product actually serves.
//!
//! # What it found, and what changed because of it
//!
//! `f16` costs nothing measurable on this corpus, so [`OkfVectorStore`] now
//! writes base64 `f16` — 2,048 bytes a vector against 6,144, **33.0 MB at
//! 16,000 records against 98.6 MB**, from 4.75× the Markdown it annotates down
//! to 1.59×.
//!
//! | | top-1 | top-5 | MRR | fused@1 | fused@5 | fused MRR |
//! |---|---|---|---|---|---|---|
//! | `f32` | 64/93 | 71/93 | 0.727 | 65/93 | 79/93 | 0.760 |
//! | `f16` | 64/93 | 71/93 | 0.727 | 65/93 | 79/93 | 0.760 |
//!
//! Not "close" — identical, on every metric, to three decimals. The mechanism
//! is the one predicted above: of 920,832 coordinates, **zero changed sign**,
//! so the packed scan is bit-identical and the shortlist never moves. Worst
//! coordinate movement is 1.2e-4. The top result is the same on all 93
//! questions; five reorder within the top five, and on none of them does the
//! *answer's* own rank move — the churn is among records that were wrong
//! either way.
//!
//! Base64 came along in the same change because it is lossless by construction
//! and the dependency it was said to cost is already in the tree — L0 depends
//! on `base64` — so the objection was a manifest line, not a supply-chain
//! addition. Doing both at once also means one format migration rather than
//! two.
//!
//! The honest bound on all of this: one corpus, 1,199 records, 768 dimensions,
//! L2-normalised vectors whose coordinates sit around 1/√768 ≈ 0.036 — well
//! inside `f16`'s normal range, which is *why* it is free. A model that is not
//! normalised, or is far wider, deserves a re-run before inheriting the
//! conclusion.
//!
//! Runs entirely off the embeddings `semantic_fusion_probe` cached. No API key,
//! no network — it skips if that cache has not been built, having nothing of
//! its own to embed.
//!
//! [`OkfVectorStore`]: gemini_memory_rs::retrieval::semantic::OkfVectorStore

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::corpus::{self, PROBES};
use common::paraphrase;
use common::rank::{fuse, lexical, rank_of, CANDIDATES};
use common::views::structural_view;
use common::{file_backed_engine, ScratchDir};

use gemini_memory_rs::bm25::{
    IndexedMemory, MemoryIndex, MemoryOrigin, SearchExplanation, SearchHit,
};
use gemini_memory_rs::core::{stable_hash, CanonicalMemory, MemoryId, MemoryKind, MemoryStatus};
use gemini_memory_rs::okf::MemoryStore;
use gemini_memory_rs::retrieval::semantic::{OkfVectorStore, VectorStore};
use gemini_memory_rs::retrieval::{PrecomputedSemanticIndex, StaticEmbedder};

/// Must match `semantic_fusion_probe`, since this reads its cache.
const EMBEDDING_MODEL: &str = "gemini-embedding-2";
const WIDTH: usize = 768;
const CACHE: &str = "semantic-width-embeddings.json";

/// The corpus size every storage figure is quoted at — a year of heavy use,
/// per `memory_at_scale`.
const RECORDS: usize = 16_000;

/// What `memory_at_scale` measures the OKF Markdown at for [`RECORDS`], in
/// bytes. Quoted so the vectors can be priced against the thing they annotate
/// rather than in isolation.
const MARKDOWN_BYTES: usize = 20_259 * 1024;

/// The text hash and its newline, which every encoding pays alike.
const ENVELOPE_BYTES: usize = 16 + 1;

/// What a vector looks like after a trip through the real store.
///
/// Deliberately the shipping code path rather than a local copy of its
/// converter: [`OkfVectorStore`] encodes and decodes, over an in-memory
/// [`MemoryStore`], so what is measured below is what a deployment would read
/// back. A reimplementation here could agree with itself and disagree with
/// production, and nothing would say so.
async fn through_the_store(vectors: &[(MemoryId, String, Vec<f32>)]) -> Vec<Vec<f32>> {
    let store = OkfVectorStore::new(Arc::new(MemoryStore::default()));
    for (id, text, vector) in vectors {
        store
            .save(id, &stable_hash(text), vector)
            .await
            .expect("an in-memory store cannot fail to write");
    }
    let loaded: HashMap<MemoryId, Vec<f32>> = store
        .load()
        .await
        .expect("an in-memory store cannot fail to read")
        .into_iter()
        .map(|(id, _, vector)| (id, vector))
        .collect();
    vectors
        .iter()
        .map(|(id, _, _)| {
            loaded
                .get(id)
                .unwrap_or_else(|| panic!("{id:?} was saved but did not load back"))
                .clone()
        })
        .collect()
}

// ─── scoring ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Outcome {
    top_one: usize,
    top_five: usize,
    reciprocal: f64,
    fused_top_one: usize,
    fused_top_five: usize,
    fused_reciprocal: f64,
    asked: usize,
}

impl Outcome {
    fn row(&self, label: &str) -> String {
        let n = self.asked.max(1) as f64;
        format!(
            "{label:<28} {:<9} {:<9} {:<8.3} {:<9} {:<9} {:.3}\n",
            format!("{}/{}", self.top_one, self.asked),
            format!("{}/{}", self.top_five, self.asked),
            self.reciprocal / n,
            format!("{}/{}", self.fused_top_one, self.asked),
            format!("{}/{}", self.fused_top_five, self.asked),
            self.fused_reciprocal / n,
        )
    }
}

/// Wrap ranked ids as search hits so they can be fused with a lexical ranking.
///
/// Score descends with rank because fusion reads position, not magnitude —
/// which is the whole point of reciprocal rank fusion and why an index that
/// returns bare ids can still be fused faithfully.
fn as_hits(ids: &[MemoryId]) -> Vec<SearchHit> {
    ids.iter()
        .enumerate()
        .map(|(rank, id)| {
            let score = 1.0 / (rank + 1) as f32;
            SearchHit {
                id: id.clone(),
                score,
                statement: String::new(),
                kind: MemoryKind::Preference,
                origin: MemoryOrigin::Canonical,
                explanation: SearchExplanation {
                    memory_id: id.clone(),
                    components: Vec::new(),
                    boosts: Vec::new(),
                    lexical_score: 0.0,
                    final_score: score,
                },
            }
        })
        .collect()
}

fn score(outcome: &mut Outcome, ranked: &[MemoryId], target: &str, lexical_hits: &Vec<SearchHit>) {
    outcome.asked += 1;
    if let Some(rank) = ranked.iter().position(|id| id.as_str() == target) {
        outcome.reciprocal += 1.0 / (rank + 1) as f64;
        if rank == 0 {
            outcome.top_one += 1;
        }
        if rank < 5 {
            outcome.top_five += 1;
        }
    }
    let semantic = as_hits(ranked);
    let fused = fuse(&[lexical_hits, &semantic, &semantic]);
    if let Some(rank) = rank_of(&fused, target) {
        outcome.fused_reciprocal += 1.0 / (rank + 1) as f64;
        if rank == 0 {
            outcome.fused_top_one += 1;
        }
        if rank < 5 {
            outcome.fused_top_five += 1;
        }
    }
}

fn cached_vectors() -> Option<HashMap<String, Vec<f32>>> {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(CACHE);
    serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()
}

fn key(task: &str, text: &str) -> String {
    stable_hash(&format!("{EMBEDDING_MODEL}|{WIDTH}|{task}|{text}"))
}

fn index_over(vectors: &[(MemoryId, String, Vec<f32>)]) -> PrecomputedSemanticIndex {
    PrecomputedSemanticIndex::from_vectors(
        vectors.to_vec(),
        Arc::new(StaticEmbedder::new(HashMap::new())),
    )
}

// ─── the measurement ────────────────────────────────────────────────────────

#[tokio::test]
async fn what_storing_the_vectors_as_f16_costs() {
    let Some(cache) = cached_vectors() else {
        eprintln!(
            "SKIP what_storing_the_vectors_as_f16_costs: no embedding cache at {CACHE}. \
             Run `semantic_fusion_probe` first — this test has nothing of its own to embed."
        );
        return;
    };

    let scratch = ScratchDir::new("storage-encoding");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();

    let mut exact: Vec<(MemoryId, String, Vec<f32>)> = Vec::with_capacity(active.len());
    for memory in &active {
        let view = structural_view(memory);
        let Some(vector) = cache.get(&key("RETRIEVAL_DOCUMENT", &view)) else {
            eprintln!(
                "SKIP: the cache is missing the structural view at {WIDTH}d — re-run \
                 `semantic_fusion_probe whether_wider_embeddings_earn_their_keep`."
            );
            return;
        };
        exact.push((memory.id.clone(), view, vector.clone()));
    }
    let stored = through_the_store(&exact).await;
    let rounded: Vec<(MemoryId, String, Vec<f32>)> = exact
        .iter()
        .zip(&stored)
        .map(|((id, text, _), vector)| (id.clone(), text.clone(), vector.clone()))
        .collect();

    let questions: Vec<(&'static str, &'static str)> = paraphrase::all()
        .map(|(probe, phrasing)| (probe, phrasing.query))
        .collect();
    let mut query_vectors = Vec::with_capacity(questions.len());
    for (_, query) in &questions {
        let Some(vector) = cache.get(&key("RETRIEVAL_QUERY", query)) else {
            eprintln!("SKIP: the cache is missing query vectors at {WIDTH}d.");
            return;
        };
        query_vectors.push(vector.clone());
    }

    // ── what rounding did to the numbers themselves ──
    //
    // Before any ranking: how far each coordinate moved, and how many sign bits
    // flipped. The sign count is the one that matters for the scan, because the
    // packed codes are nothing but signs.
    let mut worst_absolute = 0.0f32;
    let mut relative_sum = 0.0f64;
    let mut coordinates = 0usize;
    let mut sign_flips = 0usize;
    let mut underflows = 0usize;
    for ((_, _, before), (_, _, after)) in exact.iter().zip(&rounded) {
        for (a, b) in before.iter().zip(after) {
            worst_absolute = worst_absolute.max((a - b).abs());
            if *a != 0.0 {
                relative_sum += ((a - b) / a).abs() as f64;
            }
            coordinates += 1;
            if (*a >= 0.0) != (*b >= 0.0) {
                sign_flips += 1;
            }
            if *a != 0.0 && *b == 0.0 {
                underflows += 1;
            }
        }
    }

    // ── the two indexes, through the crate's own search path ──
    let f32_index = index_over(&exact);
    let f16_index = index_over(&rounded);
    let bm25 = MemoryIndex::build(active.iter().map(|m| IndexedMemory::from_canonical(m)));

    let mut baseline = Outcome::default();
    let mut halved = Outcome::default();
    let mut identical_top_five = 0usize;
    let mut identical_top_one = 0usize;
    let mut moved: Vec<String> = Vec::new();

    for (i, (probe_name, question)) in questions.iter().enumerate() {
        let target = PROBES
            .iter()
            .find(|p| p.name == *probe_name)
            .unwrap_or_else(|| panic!("no probe named {probe_name}"))
            .target;
        let lexical_hits = lexical(&bm25, question);

        let a = f32_index.search_vector(&query_vectors[i], CANDIDATES);
        let b = f16_index.search_vector(&query_vectors[i], CANDIDATES);
        score(&mut baseline, &a, target, &lexical_hits);
        score(&mut halved, &b, target, &lexical_hits);

        if a.first() == b.first() {
            identical_top_one += 1;
        }
        if a.iter().take(5).eq(b.iter().take(5)) {
            identical_top_five += 1;
        } else {
            let before = rank_position(&a, target);
            let after = rank_position(&b, target);
            if before != after {
                moved.push(format!(
                    "    {question:?}\n      answer moved from rank {before} to rank {after}"
                ));
            }
        }
    }

    // ── what each encoding costs on disk ──
    //
    // Arithmetic rather than a measurement, because these lengths are
    // determined: hex is 2 characters per byte and base64 is 4 per 3, with no
    // padding at either width since 3,072 and 1,536 are both multiples of 3.
    let encodings = [
        ("hex, f32 (the old store)", WIDTH * 4 * 2),
        ("base64, f32", WIDTH * 4 / 3 * 4),
        ("hex, f16", WIDTH * 2 * 2),
        ("base64, f16 (what ships)", WIDTH * 2 / 3 * 4),
    ];

    let mut report = format!(
        "\nwhat the vector store's encoding costs — {} records, {} questions, {WIDTH}d\n\n\
         rounding, per coordinate ({coordinates} of them):\n  \
         worst absolute move   {worst_absolute:.3e}\n  \
         mean relative move    {:.3e}\n  \
         coordinates underflowing f16 to zero   {underflows}\n  \
         sign bits flipped (these would change the packed scan)   {sign_flips}\n\n\
         ranking, through PrecomputedSemanticIndex::search_vector\n\
         semantic-only, then fused with BM25 at 2:1 — what the model is served\n\n\
         {:<28} {:<9} {:<9} {:<8} {:<9} {:<9} {}\n",
        active.len(),
        questions.len(),
        relative_sum / coordinates as f64,
        "store",
        "top-1",
        "top-5",
        "MRR",
        "fused@1",
        "fused@5",
        "fused MRR",
    );
    report.push_str(&baseline.row("f32 (the old store)"));
    report.push_str(&halved.row("f16 (what ships)"));

    report.push_str(&format!(
        "\nagreement with the f32 ranking:\n  \
         identical top result   {identical_top_one}/{}\n  \
         identical top five     {identical_top_five}/{}\n",
        questions.len(),
        questions.len(),
    ));
    if moved.is_empty() {
        report.push_str("  the answer's own rank never moved on any question\n");
    } else {
        report.push_str("  questions where the answer's rank moved:\n");
        for line in &moved {
            report.push_str(line);
            report.push('\n');
        }
    }

    report.push_str(&format!(
        "\non disk, per record and at {RECORDS} records\n\
         every encoding also pays {ENVELOPE_BYTES} bytes of hash and newline\n\n\
         {:<24} {:<12} {:<12} {:<10} {}\n",
        "encoding", "vector B", "record B", "total", "× the Markdown",
    ));
    for (label, vector_bytes) in encodings {
        let per_record = vector_bytes + ENVELOPE_BYTES;
        let total = per_record * RECORDS;
        report.push_str(&format!(
            "{label:<24} {vector_bytes:<12} {per_record:<12} {:<10} {:.2}×\n",
            format!("{:.1} MB", total as f64 / 1e6),
            total as f64 / MARKDOWN_BYTES as f64,
        ));
    }

    eprintln!("{report}");

    // ── the call ──
    //
    // Stated as assertions rather than prose so it cannot drift from the
    // numbers. Two conditions, and they are deliberately different in kind.
    //
    // The scan must be *bit-identical*, not merely similar: the packed codes
    // are sign bits, f16 preserves sign, and if that ever stops being true the
    // shortlist itself has changed and everything downstream is unmoored.
    assert_eq!(
        sign_flips, 0,
        "{sign_flips} coordinates changed sign through f16, so the packed scan is \
         no longer the same scan. The shortlist the rerank sees has changed, which \
         is a different and much larger claim than a rounding error.\n{report}"
    );

    // Quality is allowed to differ by one question — 93 questions cannot
    // resolve less than that — but not more. Anything larger is a real cost and
    // the halving is not free.
    let drop = baseline.fused_top_five as i64 - halved.fused_top_five as i64;
    assert!(
        drop <= 1,
        "f16 storage costs {drop} questions of fused top-5 ({} against {}), which is \
         more than {} questions can call noise. Halving the store is not free at this \
         precision.\n{report}",
        halved.fused_top_five,
        baseline.fused_top_five,
        questions.len(),
    );
    let top_one_drop = baseline.top_one as i64 - halved.top_one as i64;
    assert!(
        top_one_drop <= 1,
        "f16 storage costs {top_one_drop} questions of semantic top-1 ({} against {}). \
         The rerank is the only place rounding can reorder anything, and it did.\n{report}",
        halved.top_one,
        baseline.top_one,
    );
}

fn rank_position(ranked: &[MemoryId], target: &str) -> String {
    ranked
        .iter()
        .position(|id| id.as_str() == target)
        .map(|r| (r + 1).to_string())
        .unwrap_or_else(|| "unranked".to_string())
}
