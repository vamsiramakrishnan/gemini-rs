//! A precomputed, quantized semantic index.
//!
//! [`SemanticFallback`] says *what* the engine needs —
//! ids in relevance order — and nothing about how to get them. This is the
//! in-process implementation, built once from the corpus and searched without a
//! network call.
//!
//! # Why it is precomputed
//!
//! Embedding is a network round trip: 259 ms at p50, measured, flat across
//! widths (`tests/serving_latency_probe.rs`). The interactive budget is 10 ms
//! and the speculative one 100 ms, so embedding *at recall time* is not a
//! tuning problem — it is three and a half orders of magnitude out.
//!
//! Document vectors therefore have to exist before the question does. Ingestion
//! embeds each record once, concurrently — the same probe measured 88 embeds/s
//! at ×32, so a 16,000-record corpus is about three minutes of wall-clock, not
//! an overnight job — and [`PrecomputedSemanticIndex`] holds the result.
//!
//! The *query* embedding is the round trip that remains, and this type does not
//! pretend otherwise: it takes an [`Embedder`], and whether that fits the
//! caller's budget is the caller's architecture decision. A local model fits; a
//! remote one is for the speculative path, where nobody is waiting, and even
//! then only if the budget is raised past 259 ms. See [`PrecomputedSemanticIndex::search`].
//!
//! # Why it is quantized
//!
//! An exact float32 scan over 16,000 records takes 15.2 ms — past the 10 ms
//! interactive budget on its own, before the query is even embedded. Packing
//! each vector to one bit per dimension and scoring with XOR and popcount takes
//! 812 µs, and reranking the top 50 against the float vectors restores the
//! exact ranking: 105 µs against 1 ms at 1,199 records, and identical top-1,
//! top-5 and MRR (`tests/quantization_probe.rs`).
//!
//! | configuration | fused top-5 | RAM at 16k | scan at 16k |
//! |---|---|---|---|
//! | float32 exact | 79/93 | 49 MB | 15.2 ms |
//! | 1-bit packed | 77/93 | 2 MB | 812 µs |
//! | **1-bit + exact rerank** | **78/93** | 2 MB + floats | **1.3 ms** |
//!
//! The quality differences across that table are one or two questions out of
//! 93 — noise. The cost differences are 24× in memory and 12× in scan time,
//! which are not. Priced out, that is $0.021 per user per month against $0.158.
//!
//! The float vectors are kept for the rerank. A deployment that cannot afford
//! them resident can drop to [`PrecomputedSemanticIndex::without_rerank`] and lose about one
//! question in 93, or hold them on SSD and fault in fifty per query.

use std::collections::HashMap;

use async_trait::async_trait;

use super::embedding::embedding_text;
use super::retriever::SemanticFallback;
use crate::core::{CanonicalMemory, MemoryError, MemoryId, MemoryStatus};

/// How many candidates the quantized scan proposes before the exact rerank.
///
/// Fifty recovers the exact float32 ranking on this corpus. Deeper costs scan
/// time for nothing; shallower starts losing the tail.
pub const RERANK_DEPTH: usize = 50;

/// Turns text into a vector.
///
/// Implement over whatever embedder is available. The engine never calls this
/// on the document side — [`PrecomputedSemanticIndex::build`] does that once —
/// so the latency that matters is the single query embedding per recall.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed one string. Vectors must be L2-normalised and all the same width.
    async fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError>;
}

/// One record's vector, in both representations.
struct Entry {
    id: MemoryId,
    /// Sign bits of the vector, packed 64 to a word. What the scan reads.
    packed: Vec<u64>,
    /// The full vector, for the exact rerank. Empty when reranking is off.
    exact: Vec<f32>,
}

/// A semantic index built ahead of time and searched in process.
pub struct PrecomputedSemanticIndex {
    entries: Vec<Entry>,
    words: usize,
    embedder: std::sync::Arc<dyn Embedder>,
    rerank: bool,
}

impl PrecomputedSemanticIndex {
    /// Embed a corpus and build the index.
    ///
    /// Only active records are indexed: a superseded fact should not be
    /// retrievable by paraphrase when it is not retrievable by name.
    ///
    /// Each record is embedded as [`embedding_text`] renders it — the statement
    /// plus its frontmatter as prose — which is the text that measured best by
    /// a wide margin. Passing anything else is the single easiest way to lose
    /// most of what the semantic layer is worth.
    pub async fn build(
        records: &[CanonicalMemory],
        embedder: std::sync::Arc<dyn Embedder>,
    ) -> Result<Self, MemoryError> {
        let active: Vec<&CanonicalMemory> = records
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .collect();

        let mut vectors = Vec::with_capacity(active.len());
        for record in &active {
            vectors.push(embedder.embed(&embedding_text(record)).await?);
        }
        Ok(Self::from_vectors(
            active.iter().map(|m| m.id.clone()).zip(vectors).collect(),
            embedder,
        ))
    }

    /// Build from vectors that were embedded elsewhere.
    ///
    /// The path for a caller that already batches its embedding — concurrently,
    /// or in a nightly job — rather than awaiting one record at a time as
    /// [`build`](Self::build) does.
    pub fn from_vectors(
        vectors: Vec<(MemoryId, Vec<f32>)>,
        embedder: std::sync::Arc<dyn Embedder>,
    ) -> Self {
        let width = vectors.first().map(|(_, v)| v.len()).unwrap_or(0);
        let words = width.div_ceil(64);
        let entries = vectors
            .into_iter()
            .map(|(id, vector)| Entry {
                id,
                packed: pack(&vector, words),
                exact: vector,
            })
            .collect();
        Self {
            entries,
            words,
            embedder,
            rerank: true,
        }
    }

    /// Drop the float vectors, keeping only the packed codes.
    ///
    /// Trades about one question in 93 for roughly 24× less memory — 2 MB
    /// against 49 MB at 16,000 records. Worth it when the index is resident per
    /// user and there are many users; not worth it otherwise.
    pub fn without_rerank(mut self) -> Self {
        self.rerank = false;
        for entry in &mut self.entries {
            entry.exact = Vec::new();
            entry.exact.shrink_to_fit();
        }
        self
    }

    /// How many records are indexed.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes held per record, packed codes plus float vectors if reranking.
    ///
    /// Exposed because the memory figure is the reason to quantize at all, and
    /// a number a caller can assert on is more useful than a claim in a doc
    /// comment.
    pub fn bytes_per_record(&self) -> usize {
        let floats = if self.rerank {
            self.entries.first().map(|e| e.exact.len() * 4).unwrap_or(0)
        } else {
            0
        };
        self.words * 8 + floats
    }

    /// Rank ids against an already-embedded query.
    ///
    /// Separated from [`search`](Self::search) so the scan can be measured, and
    /// used, without a network call in the way.
    pub fn search_vector(&self, query: &[f32], limit: usize) -> Vec<MemoryId> {
        if self.entries.is_empty() || limit == 0 {
            return Vec::new();
        }
        let probe = pack(query, self.words);

        // Agreeing bits, which for sign-quantized unit vectors ranks the same
        // way cosine does up to the quantization error the rerank then undoes.
        let mut scored: Vec<(usize, u32)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let differing: u32 = entry
                    .packed
                    .iter()
                    .zip(&probe)
                    .map(|(a, b)| (a ^ b).count_ones())
                    .sum();
                (i, differing)
            })
            .collect();

        let depth = if self.rerank {
            RERANK_DEPTH.max(limit)
        } else {
            limit
        };
        let depth = depth.min(scored.len());
        scored.select_nth_unstable_by_key(depth - 1, |(_, d)| *d);
        scored.truncate(depth);
        scored.sort_unstable_by_key(|(_, d)| *d);

        if !self.rerank {
            return scored
                .into_iter()
                .take(limit)
                .map(|(i, _)| self.entries[i].id.clone())
                .collect();
        }

        // Exact rerank over the shortlist: restores the float32 ranking for the
        // cost of fifty dot products.
        let mut reranked: Vec<(usize, f32)> = scored
            .into_iter()
            .map(|(i, _)| {
                let score = self.entries[i]
                    .exact
                    .iter()
                    .zip(query)
                    .map(|(a, b)| a * b)
                    .sum::<f32>();
                (i, score)
            })
            .collect();
        reranked.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        reranked
            .into_iter()
            .take(limit)
            .map(|(i, _)| self.entries[i].id.clone())
            .collect()
    }
}

/// Sign bits, packed 64 to a word.
fn pack(vector: &[f32], words: usize) -> Vec<u64> {
    let mut packed = vec![0u64; words];
    for (j, value) in vector.iter().enumerate() {
        if *value >= 0.0 {
            packed[j / 64] |= 1 << (j % 64);
        }
    }
    packed
}

#[async_trait]
impl SemanticFallback for PrecomputedSemanticIndex {
    /// Embed the query, then scan.
    ///
    /// The embed is the only network call, and on the interactive path it is
    /// almost certainly too slow — 259 ms measured against a 10 ms budget. The
    /// retriever bounds this with a timeout and treats a miss as "no semantic
    /// opinion", so an over-budget embedder degrades to lexical results rather
    /// than delaying the turn. That is a real degradation, not a free one:
    /// with a remote embedder the semantic layer effectively only runs on the
    /// speculative path, and only if that budget is raised past the round trip.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryId>, MemoryError> {
        let vector = self.embedder.embed(query).await?;
        Ok(self.search_vector(&vector, limit))
    }
}

/// An embedder backed by a fixed table, for tests and offline replay.
///
/// Returns an error for text it has never seen, rather than a zero vector: a
/// silently-wrong vector ranks silently-wrong results, and a test that does
/// that passes while measuring nothing.
pub struct StaticEmbedder {
    table: HashMap<String, Vec<f32>>,
}

impl StaticEmbedder {
    /// Build from text/vector pairs.
    pub fn new(table: HashMap<String, Vec<f32>>) -> Self {
        Self { table }
    }
}

#[async_trait]
impl Embedder for StaticEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        self.table.get(text).cloned().ok_or_else(|| {
            MemoryError::Retrieval(format!(
                "StaticEmbedder has no vector for {text:?} — the table must cover \
                 every text the test embeds, or the result measures nothing"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-vectors, normalised, so ranking is meaningful
    /// without a network call.
    fn vector(seed: u64, width: usize) -> Vec<f32> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut out: Vec<f32> = (0..width)
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

    fn index(count: usize, width: usize) -> PrecomputedSemanticIndex {
        let vectors: Vec<(MemoryId, Vec<f32>)> = (0..count)
            .map(|i| {
                (
                    MemoryId::new(format!("mem_{i}")),
                    vector(i as u64 + 1, width),
                )
            })
            .collect();
        PrecomputedSemanticIndex::from_vectors(
            vectors,
            std::sync::Arc::new(StaticEmbedder::new(HashMap::new())),
        )
    }

    #[test]
    fn a_vector_finds_itself_first() {
        let built = index(200, 768);
        let query = vector(43, 768);
        let hits = built.search_vector(&query, 5);
        assert_eq!(
            hits.first().map(|id| id.as_str()),
            Some("mem_42"),
            "a record queried by its own vector must rank first"
        );
    }

    /// The rerank is the reason the float vectors are kept, so it has to be
    /// doing something: without it, sign quantization alone should sometimes
    /// order the shortlist differently.
    #[test]
    fn reranking_restores_the_exact_ordering() {
        let built = index(500, 768);
        let query = vector(77, 768);
        let exact_first = built.search_vector(&query, 1);

        let packed_only = index(500, 768).without_rerank();
        assert!(
            !packed_only.search_vector(&query, 10).is_empty(),
            "the packed scan must still return candidates"
        );
        assert_eq!(
            exact_first.first().map(|id| id.as_str()),
            Some("mem_76"),
            "the reranked top hit must be the exact nearest neighbour"
        );
    }

    #[test]
    fn dropping_the_rerank_drops_the_memory_it_was_costing() {
        let with = index(100, 768);
        let without = index(100, 768).without_rerank();
        assert_eq!(with.bytes_per_record(), 96 + 768 * 4);
        assert_eq!(
            without.bytes_per_record(),
            96,
            "packed codes only: 768 bits is 96 bytes, a 32x reduction"
        );
        assert!(without.bytes_per_record() * 30 < with.bytes_per_record());
    }

    #[test]
    fn an_empty_index_answers_without_panicking() {
        let empty = index(0, 768);
        assert!(empty.is_empty());
        assert!(empty.search_vector(&vector(1, 768), 5).is_empty());
    }

    #[test]
    fn asking_for_more_than_exists_returns_what_exists() {
        let built = index(3, 768);
        assert_eq!(built.search_vector(&vector(1, 768), 50).len(), 3);
    }

    #[tokio::test]
    async fn an_unknown_query_is_an_error_rather_than_a_silent_zero_vector() {
        let built = index(10, 768);
        assert!(built.search("never embedded", 5).await.is_err());
    }
}
