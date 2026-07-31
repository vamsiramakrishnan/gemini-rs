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
    /// Hash of the text this vector was built from.
    ///
    /// The whole basis of persistence being safe. A vector is only reusable
    /// while the text that produced it is unchanged, and a record's text
    /// changes whenever its statement or frontmatter does — which is exactly
    /// what a correction is. Keying on the id alone would restore a vector for
    /// the old wording of a fact and never notice.
    hash: String,
    /// Sign bits of the vector, packed 64 to a word. What the scan reads.
    packed: Vec<u64>,
    /// The full vector, for the exact rerank. Empty when reranking is off.
    exact: Vec<f32>,
}

/// A semantic index built ahead of time and searched in process.
pub struct PrecomputedSemanticIndex {
    /// Behind a lock so [`SemanticFallback::reconcile`] can bring the index in
    /// line with the corpus after a correction, without the engine having to
    /// rebuild and swap the whole thing.
    entries: parking_lot::RwLock<Vec<Entry>>,
    /// Words per packed code, learned from the first vector the index sees.
    ///
    /// Not fixed at construction because an index legitimately starts empty —
    /// a new user's first fact arrives through
    /// [`SemanticFallback::reconcile`], not through the constructor. Deriving
    /// the width only from the constructor left such an index with a zero-word
    /// code, which packs every vector to nothing and scores every record
    /// identically: a cold-start index that silently ranked at random.
    words: std::sync::atomic::AtomicUsize,
    embedder: std::sync::Arc<dyn Embedder>,
    rerank: bool,
    /// Where vectors survive a restart, if anywhere.
    store: Option<std::sync::Arc<dyn VectorStore>>,
    /// Serialises [`SemanticFallback::reconcile`] against itself.
    ///
    /// `reconcile` takes a *whole desired state*: it reads what is held, awaits
    /// embedding for what is missing, then replaces the set. One engine hands
    /// the same backend to every session it opens, so two sessions finishing
    /// turns at once run that read-await-replace concurrently — and the one
    /// that finishes last wins with a corpus snapshot it took first. An older
    /// snapshot landing second `retain`s away records the newer one added, and
    /// removes their vectors from the store too, leaving the semantic index
    /// behind the repository until something happens to reconcile again.
    ///
    /// An async mutex rather than a sync one because the guarded region awaits
    /// the network. Searches are deliberately *not* behind it — they take the
    /// `entries` read lock as before, so a recall never waits on an embedding
    /// round trip.
    ///
    /// It guards `applied` as well as the read-await-replace, because the two
    /// have to be atomic together: check the revision, then apply, with nothing
    /// landing in between.
    reconciling: tokio::sync::Mutex<u64>,
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
            active
                .iter()
                .zip(vectors)
                .map(|(m, v)| (m.id.clone(), embedding_text(m), v))
                .collect(),
            embedder,
        ))
    }

    /// Build from vectors that were embedded elsewhere.
    ///
    /// The path for a caller that already batches its embedding — concurrently,
    /// or in a nightly job — rather than awaiting one record at a time as
    /// [`build`](Self::build) does.
    ///
    /// Each entry is `(id, the text that was embedded, the vector)`. The text is
    /// required rather than convenient: the index hashes it so that
    /// [`SemanticFallback::reconcile`] can tell an unchanged record from one
    /// whose wording has moved. Without it every reconcile would re-embed the
    /// whole corpus, which is the cost this type exists to avoid.
    pub fn from_vectors(
        vectors: Vec<(MemoryId, String, Vec<f32>)>,
        embedder: std::sync::Arc<dyn Embedder>,
    ) -> Self {
        let width = vectors.first().map(|(_, _, v)| v.len()).unwrap_or(0);
        let words = width.div_ceil(64);
        let entries = vectors
            .into_iter()
            .map(|(id, text, vector)| Entry {
                id,
                hash: crate::core::stable_hash(&text),
                packed: pack(&vector, words),
                exact: vector,
            })
            .collect();
        Self {
            entries: parking_lot::RwLock::new(entries),
            words: std::sync::atomic::AtomicUsize::new(words),
            embedder,
            rerank: true,
            store: None,
            reconciling: tokio::sync::Mutex::new(0),
        }
    }

    /// Keep vectors in `store`, and load whatever it already holds.
    ///
    /// This is the constructor a long-lived process wants. Without it every
    /// start pays one embedding round trip per record — 259 ms each, so an hour
    /// and a quarter at 16,000 records before the first semantic answer, again
    /// on every deploy and every replica.
    ///
    /// A stored vector is only trusted while the text that produced it is
    /// unchanged; [`SemanticFallback::reconcile`] checks the hash and
    /// re-embeds anything that has moved. So a restore is a fast start, never a
    /// stale one.
    pub async fn restore(
        store: std::sync::Arc<dyn VectorStore>,
        embedder: std::sync::Arc<dyn Embedder>,
    ) -> Result<Self, MemoryError> {
        let saved = store.load().await?;
        let width = saved.first().map(|(_, _, v)| v.len()).unwrap_or(0);
        let words = width.div_ceil(64);
        let entries = saved
            .into_iter()
            .map(|(id, hash, vector)| Entry {
                id,
                hash,
                packed: pack(&vector, words),
                exact: vector,
            })
            .collect();
        Ok(Self {
            entries: parking_lot::RwLock::new(entries),
            words: std::sync::atomic::AtomicUsize::new(words),
            embedder,
            rerank: true,
            store: Some(store),
            reconciling: tokio::sync::Mutex::new(0),
        })
    }

    /// Attach a store to an index built in memory.
    pub fn with_store(mut self, store: std::sync::Arc<dyn VectorStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Drop the float vectors, keeping only the packed codes.
    ///
    /// Trades about one question in 93 for roughly 24× less memory — 2 MB
    /// against 49 MB at 16,000 records. Worth it when the index is resident per
    /// user and there are many users; not worth it otherwise.
    pub fn without_rerank(mut self) -> Self {
        self.rerank = false;
        for entry in self.entries.get_mut().iter_mut() {
            entry.exact = Vec::new();
            entry.exact.shrink_to_fit();
        }
        self
    }

    /// How many records are indexed.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Whether the index holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Bytes held per record, packed codes plus float vectors if reranking.
    ///
    /// Exposed because the memory figure is the reason to quantize at all, and
    /// a number a caller can assert on is more useful than a claim in a doc
    /// comment.
    pub fn bytes_per_record(&self) -> usize {
        let words = self.words.load(std::sync::atomic::Ordering::Acquire);
        let floats = if self.rerank {
            self.entries
                .read()
                .first()
                .map(|e| e.exact.len() * 4)
                .unwrap_or(0)
        } else {
            0
        };
        words * 8 + floats
    }

    /// Rank ids against an already-embedded query.
    ///
    /// Separated from [`search`](Self::search) so the scan can be measured, and
    /// used, without a network call in the way.
    pub fn search_vector(&self, query: &[f32], limit: usize) -> Vec<MemoryId> {
        let entries = self.entries.read();
        let words = self.words.load(std::sync::atomic::Ordering::Acquire);
        if entries.is_empty() || limit == 0 || words == 0 {
            return Vec::new();
        }
        let probe = pack(query, words);

        // Agreeing bits, which for sign-quantized unit vectors ranks the same
        // way cosine does up to the quantization error the rerank then undoes.
        let mut scored: Vec<(usize, u32)> = entries
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
                .map(|(i, _)| entries[i].id.clone())
                .collect();
        }

        // Exact rerank over the shortlist: restores the float32 ranking for the
        // cost of fifty dot products.
        let mut reranked: Vec<(usize, f32)> = scored
            .into_iter()
            .map(|(i, _)| {
                let score = entries[i]
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
            .map(|(i, _)| entries[i].id.clone())
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

// ─── persistence ────────────────────────────────────────────────────────────

/// Somewhere to keep vectors between processes.
///
/// Without this the index is rebuilt from nothing on every start, and rebuilding
/// means one embedding round trip per record: 259 ms each, measured, so a
/// 16,000-record corpus is over an hour of wall-clock before the first question
/// can be answered semantically. That is not a slow start, it is an unusable
/// one, and it recurs on every deploy and every replica.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Every vector held, as `(id, text hash, vector)`.
    async fn load(&self) -> Result<Vec<(MemoryId, String, Vec<f32>)>, MemoryError>;

    /// Persist one vector against the hash of the text that produced it.
    async fn save(&self, id: &MemoryId, hash: &str, vector: &[f32]) -> Result<(), MemoryError>;

    /// Forget a record's vector.
    async fn remove(&self, id: &MemoryId) -> Result<(), MemoryError>;
}

/// Vectors kept beside the records, in the same store the OKF Markdown uses.
///
/// One document per record, holding the text hash and the vector as base64
/// `f16`. That encoding is worth explaining, because it is the largest thing
/// this type adds to a deployment and an earlier revision wrote it the obvious
/// way — lowercase hex `f32` — at three times the size.
///
/// | encoding | vector bytes | per record | at 16,000 | × the Markdown |
/// |---|---|---|---|---|
/// | hex `f32` (what this used to write) | 6,144 | 6,161 | 98.6 MB | 4.75× |
/// | base64 `f32` | 4,096 | 4,113 | 65.8 MB | 3.17× |
/// | hex `f16` | 3,072 | 3,089 | 49.4 MB | 2.38× |
/// | **base64 `f16` (what ships)** | **2,048** | **2,065** | **33.0 MB** | **1.59×** |
///
/// The Markdown column compares against the records these annotate, which
/// `memory_at_scale` measures at 20,259 KiB for the same 16,000. Hex `f32` made
/// the vectors nearly five times the corpus they describe; base64 `f16` makes
/// them about one and a half.
///
/// # Why `f16` is free here, measured rather than assumed
///
/// `f16` keeps 10 mantissa bits against `f32`'s 23, so the obvious worry is
/// that a lossy store quietly degrades retrieval. It does not, and the reason
/// is that [`PrecomputedSemanticIndex`] reads these floats for exactly two
/// things with very different sensitivities:
///
/// - **The packed scan is sign bits**, and `f16` preserves sign. Over 920,832
///   coordinates of a real corpus, zero flipped — so the shortlist the rerank
///   sees is bit-identical, not merely similar. The one way it could flip is a
///   coordinate below `f16`'s smallest subnormal (2⁻²⁴ ≈ 6e-8) underflowing to
///   `-0.0`, which the packer reads as non-negative; three coordinates underflowed
///   on that corpus and all three were positive. The probe asserts on the count
///   rather than trusting the argument.
/// - **The rerank is a dot product**, where rounding *could* reorder
///   candidates. Over 93 questions it did not move the answer's rank once.
///
/// `tests/storage_encoding_probe.rs` runs both stores through this crate's own
/// `search_vector` and reports identical top-1 (64/93), top-5 (71/93) and MRR
/// (0.727), and identical fused numbers against BM25 at 2:1 (65/93, 79/93,
/// 0.760). Worst coordinate movement is 1.2e-4. Five questions reorder *within*
/// the top five, all among non-answers.
///
/// So this is a 3× saving for no measured quality cost — but the measurement is
/// on one 1,199-record corpus at 768 dimensions, and the property it rests on
/// is that embedding coordinates sit comfortably inside `f16`'s normal range.
/// A model whose vectors are not L2-normalised, or are far wider, deserves a
/// re-run of that probe before the same conclusion is assumed.
///
/// # Reading what an older version wrote
///
/// A payload tagged `f16b64:` is base64 `f16`; anything else is the legacy
/// lowercase-hex `f32`, and is decoded as such. That matters more than a format
/// flag usually does: without it, upgrading would invalidate every stored
/// vector and re-embedding a 16,000-record corpus is over an hour of wall
/// clock. Existing stores therefore keep their old size until each record is
/// next rewritten, which happens when its text changes.
///
/// What all of this buys is the whole reason to pay it: without persistence a
/// restart re-embeds every record at 259 ms each, which is over an hour at this
/// corpus size, on every deploy and every replica.
pub struct OkfVectorStore<S: crate::okf::OkfStore> {
    store: std::sync::Arc<S>,
    prefix: String,
}

impl<S: crate::okf::OkfStore> OkfVectorStore<S> {
    /// Keep vectors under `vectors/` in the given store.
    pub fn new(store: std::sync::Arc<S>) -> Self {
        Self {
            store,
            prefix: "vectors".to_string(),
        }
    }

    fn path(&self, id: &MemoryId) -> String {
        format!("{}/{}.vec", self.prefix, id.as_str())
    }
}

/// Marks a payload as base64 `f16`. Anything without it is the lowercase-hex
/// `f32` an earlier version wrote, and is still readable.
const F16_B64: &str = "f16b64:";

/// Nearest `f16`, ties to even.
///
/// Written out rather than taking `half` as a dependency: it is one fixed
/// standard, it is forty lines, and it is exhaustively pinned over all 65,536
/// bit patterns by `every_f16_survives_a_round_trip` below.
fn to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x007f_ffff;

    if exponent == 0xff {
        // Infinity keeps its sign; NaN keeps a set mantissa bit so it stays NaN.
        return sign | 0x7c00 | if mantissa != 0 { 0x0200 } else { 0 };
    }
    let unbiased = exponent - 127;
    if unbiased > 15 {
        return sign | 0x7c00; // beyond f16's largest normal
    }
    if unbiased < -24 {
        return sign; // below its smallest subnormal, but the sign survives
    }

    // Normal and subnormal differ only in how many mantissa bits are dropped
    // and whether the implicit leading 1 has to be restored, so the rounding
    // below is written once.
    let (mut significand, shift, mut biased) = if unbiased < -14 {
        (mantissa | 0x0080_0000, (-unbiased - 1) as u32, 0i32)
    } else {
        (mantissa, 13u32, unbiased + 15)
    };
    let dropped = significand & ((1 << shift) - 1);
    significand >>= shift;
    let halfway = 1u32 << (shift - 1);
    if dropped > halfway || (dropped == halfway && significand & 1 == 1) {
        significand += 1;
        // Rounding up can carry out of the mantissa, which is a clean increment
        // of the exponent. For a subnormal it is promotion to the smallest
        // normal, and the bit lands in the exponent field on its own.
        if significand & 0x0400 != 0 && biased > 0 {
            significand = 0;
            biased += 1;
            if biased >= 0x1f {
                return sign | 0x7c00;
            }
        }
    }
    if biased == 0 {
        return sign | significand as u16;
    }
    sign | ((biased as u16) << 10) | (significand as u16 & 0x03ff)
}

/// The value read back. Exact in this direction — every `f16` is an `f32`.
fn from_f16(bits: u16) -> f32 {
    let sign = ((bits & 0x8000) as u32) << 16;
    let exponent = ((bits >> 10) & 0x1f) as u32;
    let mantissa = (bits & 0x03ff) as u32;

    if exponent == 0 {
        if mantissa == 0 {
            return f32::from_bits(sign);
        }
        // Subnormal: shift until the leading bit is explicit, charging each
        // shift to the exponent.
        let mut shifted = mantissa;
        let mut steps = 0u32;
        while shifted & 0x0400 == 0 {
            shifted <<= 1;
            steps += 1;
        }
        let biased = (127 - 14 - steps as i32) as u32;
        return f32::from_bits(sign | (biased << 23) | ((shifted & 0x03ff) << 13));
    }
    if exponent == 0x1f {
        return f32::from_bits(sign | 0x7f80_0000 | (mantissa << 13));
    }
    f32::from_bits(sign | ((exponent + 127 - 15) << 23) | (mantissa << 13))
}

/// The payload line: `f16` little-endian bytes, base64, behind [`F16_B64`].
fn encode(vector: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(vector.len() * 2);
    for value in vector {
        bytes.extend_from_slice(&to_f16(*value).to_le_bytes());
    }
    format!(
        "{F16_B64}{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
    )
}

/// The inverse, accepting either format. Returns `None` for anything malformed
/// rather than a partial vector — a truncated vector would rank silently
/// wrongly.
fn decode(payload: &str) -> Option<Vec<f32>> {
    let Some(body) = payload.strip_prefix(F16_B64) else {
        return from_hex(payload);
    };
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body.as_bytes()).ok()?;
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    Some(
        bytes
            .chunks_exact(2)
            .map(|pair| from_f16(u16::from_le_bytes([pair[0], pair[1]])))
            .collect(),
    )
}

/// `f32` little-endian bytes as lowercase hex — what earlier versions wrote.
///
/// Kept only so a legacy store can be constructed in a test and proved to still
/// load; nothing writes this format any more.
#[cfg(test)]
fn to_hex(vector: &[f32]) -> String {
    let mut out = String::with_capacity(vector.len() * 8);
    for value in vector {
        for byte in value.to_le_bytes() {
            out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
            out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
        }
    }
    out
}

/// Decode the legacy lowercase-hex `f32` payload.
fn from_hex(hex: &str) -> Option<Vec<f32>> {
    if !hex.len().is_multiple_of(8) {
        return None;
    }
    let bytes: Option<Vec<u8>> = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some(((hi << 4) | lo) as u8)
        })
        .collect();
    Some(
        bytes?
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

#[async_trait]
impl<S: crate::okf::OkfStore> VectorStore for OkfVectorStore<S> {
    async fn load(&self) -> Result<Vec<(MemoryId, String, Vec<f32>)>, MemoryError> {
        let mut out = Vec::new();
        for path in self.store.list(&self.prefix).await? {
            let Some(body) = self.store.read(&path).await? else {
                continue;
            };
            let Some((hash, payload)) = body.split_once('\n') else {
                continue;
            };
            let Some(vector) = decode(payload.trim()) else {
                // A corrupt file is skipped rather than failed on: the record
                // simply gets re-embedded, which is slow but correct. Failing
                // the load would make one bad file cost the whole index.
                continue;
            };
            let id = path
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".vec"))
                .map(MemoryId::new);
            if let Some(id) = id {
                out.push((id, hash.to_string(), vector));
            }
        }
        Ok(out)
    }

    async fn save(&self, id: &MemoryId, hash: &str, vector: &[f32]) -> Result<(), MemoryError> {
        self.store
            .write(&self.path(id), &format!("{hash}\n{}", encode(vector)))
            .await
    }

    async fn remove(&self, id: &MemoryId) -> Result<(), MemoryError> {
        self.store.remove(&self.path(id)).await
    }
}

#[async_trait]
impl SemanticFallback for PrecomputedSemanticIndex {
    /// Bring the index in line with the active corpus.
    ///
    /// Idempotent by construction: `active` is the whole desired state, so this
    /// embeds the ids it does not hold, drops the ids no longer present, and
    /// leaves the rest alone. Calling it twice costs one pass over a hash set
    /// the second time.
    ///
    /// Only genuinely new records are embedded, which is the difference between
    /// a correction costing one 259 ms round trip and costing one per record in
    /// the corpus. The lock is not held across any of those awaits — embedding
    /// happens first, and the index is only taken for the swap at the end — so
    /// a recall running concurrently sees either the old set or the new one and
    /// never blocks on the network.
    async fn reconcile(
        &self,
        active: &[(MemoryId, String)],
        revision: u64,
    ) -> Result<(), MemoryError> {
        // Held for the whole operation, not just the write, and it carries the
        // newest revision applied so far.
        //
        // Two things have to be true and neither is free. The calls must not
        // interleave — `active` is a whole desired state, so a `retain` running
        // against a set read before another call finished deletes that call's
        // records from the index and from the store. And a call must not apply
        // a *stale* set even when it does not interleave, which serialising
        // alone does not prevent: two sessions sealing at once both snapshot
        // the corpus first, and if the older snapshot takes the lock second it
        // is still older. So the revision is checked here and recorded on
        // success, under the same guard.
        let mut applied = self.reconciling.lock().await;
        // `<` not `<=`: re-applying the same revision is idempotent and
        // harmless, and a caller with nothing to order by passes 0 every time.
        if revision != 0 && revision < *applied {
            return Ok(());
        }

        // What is already held, and for which wording. A record whose text has
        // changed is *not* a hit: its stored vector describes the old wording,
        // which is precisely the situation a correction creates.
        //
        // Read *after* taking the lock, so it reflects any reconcile that just
        // finished rather than a snapshot from before the wait.
        let held: std::collections::HashMap<MemoryId, String> = {
            let entries = self.entries.read();
            entries
                .iter()
                .map(|e| (e.id.clone(), e.hash.clone()))
                .collect()
        };
        let wanted: std::collections::HashSet<MemoryId> =
            active.iter().map(|(id, _)| id.clone()).collect();

        let mut fresh: Vec<(MemoryId, String, Vec<f32>)> = Vec::new();
        for (id, text) in active {
            let hash = crate::core::stable_hash(text);
            if held.get(id) == Some(&hash) {
                continue;
            }
            // Embedding is the expensive step — 259 ms of network — so it is the
            // last resort, after both the in-memory index and the store.
            let vector = self.embedder.embed(text).await?;
            if let Some(store) = &self.store {
                store.save(id, &hash, &vector).await?;
            }
            fresh.push((id.clone(), hash, vector));
        }

        // A record dropped from the corpus loses its stored vector too;
        // otherwise the store grows forever and a restored index would
        // resurrect facts the user has superseded.
        if let Some(store) = &self.store {
            for (id, _) in held.iter().filter(|(id, _)| !wanted.contains(id)) {
                store.remove(id).await?;
            }
        }

        // An index that started empty learns its width here, from the first
        // vector it is ever given.
        if let Some((_, _, first)) = fresh.first() {
            let _ = self.words.compare_exchange(
                0,
                first.len().div_ceil(64),
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            );
        }
        let words = self.words.load(std::sync::atomic::Ordering::Acquire);

        let mut entries = self.entries.write();
        entries.retain(|entry| wanted.contains(&entry.id));
        for (id, hash, vector) in fresh {
            // Replace rather than duplicate: a re-embedded record is already in
            // `entries` under its old hash.
            entries.retain(|e| e.id != id);
            let packed = pack(&vector, words);
            entries.push(Entry {
                id,
                hash,
                exact: if self.rerank { vector } else { Vec::new() },
                packed,
            });
        }
        *applied = (*applied).max(revision);
        Ok(())
    }

    /// Embed the query, then scan.
    ///
    /// The embed is the only network call on this path, and on the interactive
    /// budget it is almost certainly too slow — 259 ms measured against 10 ms.
    /// The retriever bounds it with a timeout and treats a miss as "no semantic
    /// opinion", so an over-budget embedder degrades to lexical results rather
    /// than delaying the turn. That is a real degradation, not a free one: with
    /// a remote embedder the semantic layer effectively only runs on the
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
        let vectors: Vec<(MemoryId, String, Vec<f32>)> = (0..count)
            .map(|i| {
                (
                    MemoryId::new(format!("mem_{i}")),
                    format!("record {i}"),
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

    /// Reconcile is the path a correction takes into the index, so its
    /// contract is worth pinning directly rather than only through the
    /// end-to-end test.
    #[tokio::test]
    async fn reconcile_adds_what_is_new_and_drops_what_is_gone() {
        let mut table = HashMap::new();
        table.insert("fresh record".to_string(), vector(999, 768));
        let built = PrecomputedSemanticIndex::from_vectors(
            vec![
                (
                    MemoryId::new("mem_keep"),
                    "already held".into(),
                    vector(1, 768),
                ),
                (
                    MemoryId::new("mem_retire"),
                    "retired".into(),
                    vector(2, 768),
                ),
            ],
            std::sync::Arc::new(StaticEmbedder::new(table)),
        );
        assert_eq!(built.len(), 2);

        // The desired state: keep one, retire one, add one.
        built
            .reconcile(
                &[
                    (MemoryId::new("mem_keep"), "already held".into()),
                    (MemoryId::new("mem_new"), "fresh record".into()),
                ],
                0,
            )
            .await
            .expect("reconcile");

        assert_eq!(built.len(), 2, "one retired, one added");
        let ids = built.search_vector(&vector(999, 768), 10);
        let ids: Vec<&str> = ids.iter().map(|i| i.as_str()).collect();
        assert!(ids.contains(&"mem_new"), "the new record was not embedded");
        assert!(
            ids.contains(&"mem_keep"),
            "a still-active record was dropped"
        );
        assert!(
            !ids.contains(&"mem_retire"),
            "a record no longer active is still in the index"
        );
    }

    /// The already-held record's text is deliberately absent from the
    /// embedder's table, so this fails loudly if reconcile re-embeds it. That
    /// is the difference between a correction costing one round trip and one
    /// per record in the corpus.
    #[tokio::test]
    async fn reconcile_does_not_re_embed_what_it_already_holds() {
        let built = PrecomputedSemanticIndex::from_vectors(
            vec![(
                MemoryId::new("mem_keep"),
                "never embeddable".into(),
                vector(1, 768),
            )],
            std::sync::Arc::new(StaticEmbedder::new(HashMap::new())),
        );
        built
            .reconcile(&[(MemoryId::new("mem_keep"), "never embeddable".into())], 0)
            .await
            .expect("reconcile must not embed a record it already holds");
        assert_eq!(built.len(), 1);
    }

    /// A stale corpus snapshot must not undo a newer one.
    ///
    /// The scenario is two sessions on one engine sealing at nearly the same
    /// moment: each snapshots the corpus, then reconciles. Session A saw two
    /// records; session B, starting later, saw three. If A's call lands second
    /// — which is entirely ordinary, since the two race through an embedding
    /// round trip — a backend that simply applies whatever it is handed
    /// `retain`s away B's third record and deletes its vector from the store.
    ///
    /// Serialising the calls does not prevent this: A's snapshot is stale
    /// whenever it is applied, not only when it interleaves. Ordering by
    /// revision does.
    #[tokio::test]
    async fn a_reconcile_from_an_older_corpus_does_not_undo_a_newer_one() {
        let mut table = HashMap::new();
        for (i, text) in ["first fact", "second fact", "third fact"]
            .iter()
            .enumerate()
        {
            table.insert((*text).to_string(), vector(i as u64 + 1, 768));
        }
        let built = PrecomputedSemanticIndex::from_vectors(
            Vec::new(),
            std::sync::Arc::new(StaticEmbedder::new(table)),
        );

        let older = vec![
            (MemoryId::new("mem_first"), "first fact".to_string()),
            (MemoryId::new("mem_second"), "second fact".to_string()),
        ];
        let newer = vec![
            (MemoryId::new("mem_first"), "first fact".to_string()),
            (MemoryId::new("mem_second"), "second fact".to_string()),
            (MemoryId::new("mem_third"), "third fact".to_string()),
        ];

        // The newer corpus lands first...
        built.reconcile(&newer, 7).await.expect("newer");
        assert_eq!(built.len(), 3);

        // ...and the older one, still in flight, lands second.
        built.reconcile(&older, 5).await.expect("older");
        assert_eq!(
            built.len(),
            3,
            "a reconcile from revision 5 removed a record that revision 7 had \
             already added — the stale snapshot won"
        );

        // And the index is not frozen: a genuinely newer state still applies.
        let newest = vec![(MemoryId::new("mem_first"), "first fact".to_string())];
        built.reconcile(&newest, 9).await.expect("newest");
        assert_eq!(built.len(), 1, "revision 9 should apply");
    }

    /// Revision `0` means "nothing to order by" and must not gate anything —
    /// otherwise the first call would set a floor that silently rejects the
    /// rest.
    #[tokio::test]
    async fn revision_zero_disables_the_ordering_check() {
        let mut table = HashMap::new();
        table.insert("only fact".to_string(), vector(4, 768));
        let built = PrecomputedSemanticIndex::from_vectors(
            Vec::new(),
            std::sync::Arc::new(StaticEmbedder::new(table)),
        );
        let desired = vec![(MemoryId::new("mem_one"), "only fact".to_string())];
        built.reconcile(&desired, 12).await.expect("first");
        built
            .reconcile(&desired, 0)
            .await
            .expect("an unordered reconcile must still apply");
        assert_eq!(built.len(), 1);
    }

    /// Reconciling twice with the same desired state must be a no-op.
    #[tokio::test]
    async fn reconcile_is_idempotent() {
        let mut table = HashMap::new();
        table.insert("one".to_string(), vector(7, 768));
        let built = PrecomputedSemanticIndex::from_vectors(
            Vec::new(),
            std::sync::Arc::new(StaticEmbedder::new(table)),
        );
        let desired = [(MemoryId::new("mem_one"), "one".to_string())];
        built.reconcile(&desired, 0).await.expect("first");
        let after_first = built.len();
        built.reconcile(&desired, 0).await.expect("second");
        assert_eq!(
            built.len(),
            after_first,
            "the second pass changed the index"
        );
    }

    /// The cold-start path: an index that starts empty and receives its first
    /// fact through reconcile must be searchable afterwards.
    ///
    /// This failed when the code width was fixed at construction — an empty
    /// index had zero words, packed every vector to nothing, and scored every
    /// record identically. It ranked at random and said nothing about it, which
    /// is precisely the shape of bug a new user would hit and nobody would see.
    #[tokio::test]
    async fn an_index_that_starts_empty_becomes_searchable_after_its_first_fact() {
        let mut table = HashMap::new();
        table.insert("first fact".to_string(), vector(11, 768));
        table.insert("second fact".to_string(), vector(22, 768));
        let built = PrecomputedSemanticIndex::from_vectors(
            Vec::new(),
            std::sync::Arc::new(StaticEmbedder::new(table)),
        );
        assert!(built.is_empty());

        built
            .reconcile(
                &[
                    (MemoryId::new("mem_first"), "first fact".into()),
                    (MemoryId::new("mem_second"), "second fact".into()),
                ],
                0,
            )
            .await
            .expect("reconcile");

        assert_eq!(built.len(), 2);
        let hits = built.search_vector(&vector(22, 768), 1);
        assert_eq!(
            hits.first().map(|id| id.as_str()),
            Some("mem_second"),
            "an index built empty must rank properly once it has been filled"
        );
    }

    /// The stored format is lossy, so "round-trips exactly" is the wrong bar.
    /// What has to hold is that it round-trips to the *nearest `f16`* — the
    /// value is stable under a second trip, and every coordinate lands within
    /// one `f16` step of where it started.
    #[test]
    fn vectors_survive_the_round_trip_to_the_nearest_f16() {
        let original = vector(5, 768);
        let restored = decode(&encode(&original)).expect("valid payload");
        assert_eq!(restored.len(), original.len());
        for (before, after) in original.iter().zip(&restored) {
            // f16 carries 11 significant bits, so a relative step is 2⁻¹⁰.
            let tolerance = before.abs() * 2f32.powi(-10) + f32::MIN_POSITIVE;
            assert!(
                (before - after).abs() <= tolerance,
                "{before} stored and read back as {after}, further than one f16 step"
            );
            assert_eq!(
                before >= &0.0,
                after >= &0.0,
                "sign must survive: the packed scan is nothing but sign bits"
            );
        }
        // Idempotent, so a record rewritten without changing does not drift
        // further each time it is saved.
        assert_eq!(
            decode(&encode(&restored)).expect("valid payload"),
            restored,
            "storing an already-stored vector must not move it again"
        );
    }

    /// Every `f16` bit pattern has to survive `f16` → `f32` → `f16`, which is
    /// the property the idempotence above rests on. Exhaustive, because it can
    /// be: there are only 65,536 of them.
    #[test]
    fn every_f16_survives_a_round_trip() {
        for bits in 0u16..=u16::MAX {
            let value = from_f16(bits);
            if value.is_nan() {
                continue;
            }
            assert_eq!(
                to_f16(value),
                bits,
                "f16 {bits:#06x} ({value}) did not survive"
            );
        }
        // Ties to even, which is what keeps the rounding unbiased — a converter
        // that always rounded away from zero would pass the loop above and
        // quietly stretch every vector.
        assert_eq!(from_f16(to_f16(1.0 + 2f32.powi(-11))), 1.0);
        assert_eq!(from_f16(to_f16(0.1)), 0.099_975_586);
        // Out of range in both directions, sign intact at the bottom.
        assert!(from_f16(to_f16(70_000.0)).is_infinite());
        assert!(from_f16(to_f16(-1e-9)).is_sign_negative());
    }

    /// Rounding has to be unbiased, or every dot product bends the same way.
    ///
    /// Worth its own test because a converter that truncated toward zero would
    /// pass the exhaustive round trip above — truncation is still idempotent —
    /// while quietly shrinking every vector it stored.
    #[test]
    fn rounding_is_unbiased_rather_than_toward_zero() {
        // At the scale a normalised 768d coordinate actually occupies: around
        // 1/sqrt(768) ≈ 0.036.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let (mut drift, mut magnitude) = (0.0f64, 0.0f64);
        for _ in 0..100_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let value = ((state >> 11) as f64 / (1u64 << 53) as f64 - 0.5) as f32 * 0.1;
            let error = (from_f16(to_f16(value)) - value) as f64;
            drift += error;
            magnitude += error.abs();
        }
        assert!(
            drift.abs() < magnitude * 0.05,
            "rounding drifted {drift:.3e} against {magnitude:.3e} of total error — \
             that is a bias, not noise"
        );
    }

    /// A store written before the format changed must still load, or upgrading
    /// silently invalidates every vector and re-embedding a 16,000-record
    /// corpus is over an hour of wall clock.
    #[test]
    fn a_legacy_hex_payload_still_decodes() {
        let original = vector(7, 768);
        let restored = decode(&to_hex(&original)).expect("legacy hex must still load");
        assert_eq!(
            original, restored,
            "the legacy format was exact and must still decode exactly"
        );
    }

    #[test]
    fn malformed_payloads_are_rejected_rather_than_truncated() {
        assert!(from_hex("abc").is_none(), "odd length");
        assert!(from_hex("zzzzzzzz").is_none(), "not hex");
        assert!(from_hex("abcdef").is_none(), "not a whole f32");
        assert!(decode("f16b64:!!!!").is_none(), "not base64");
        assert!(
            decode(&format!("{F16_B64}{}", "AAAAA")).is_none(),
            "not a whole f16"
        );
    }

    /// The saving that motivated the change, asserted rather than claimed.
    #[test]
    fn the_encoding_is_a_third_of_what_hex_f32_cost() {
        let original = vector(11, 768);
        let now = encode(&original).len();
        let before = to_hex(&original).len();
        assert_eq!(before, 768 * 8, "hex f32 is 8 characters per coordinate");
        assert!(
            now * 3 <= before + F16_B64.len() * 3,
            "base64 f16 is {now} characters against hex f32's {before}; the point of \
             the change was a threefold saving"
        );
    }

    /// The point of persistence: a restart must not re-embed.
    ///
    /// The embedder's table is deliberately empty, so any embedding attempt is
    /// an error rather than a slow success — which is what makes this a test of
    /// the cache and not of the network.
    #[tokio::test]
    async fn a_restored_index_answers_without_embedding_anything() {
        let store = std::sync::Arc::new(OkfVectorStore::new(std::sync::Arc::new(
            crate::okf::MemoryStore::default(),
        )));
        let mut table = HashMap::new();
        table.insert("the only fact".to_string(), vector(3, 768));

        // First process: embeds once and persists.
        let first = PrecomputedSemanticIndex::from_vectors(
            Vec::new(),
            std::sync::Arc::new(StaticEmbedder::new(table)),
        )
        .with_store(store.clone());
        first
            .reconcile(&[(MemoryId::new("mem_one"), "the only fact".into())], 0)
            .await
            .expect("first reconcile");
        assert_eq!(first.len(), 1);

        // Second process: same store, an embedder that can embed nothing.
        let second = PrecomputedSemanticIndex::restore(
            store.clone(),
            std::sync::Arc::new(StaticEmbedder::new(HashMap::new())),
        )
        .await
        .expect("restore");
        assert_eq!(second.len(), 1, "the vector did not survive the restart");
        second
            .reconcile(&[(MemoryId::new("mem_one"), "the only fact".into())], 0)
            .await
            .expect("a restored vector must not be re-embedded");

        let hits = second.search_vector(&vector(3, 768), 1);
        assert_eq!(hits.first().map(|i| i.as_str()), Some("mem_one"));
    }

    /// And the safety property that makes the cache trustworthy: a record whose
    /// text has changed must be re-embedded, not restored from its old vector.
    ///
    /// This is exactly a correction. Keying the store on the id alone would
    /// restore the vector for the wording the user just replaced.
    #[tokio::test]
    async fn a_record_whose_text_changed_is_re_embedded_rather_than_restored() {
        let store = std::sync::Arc::new(OkfVectorStore::new(std::sync::Arc::new(
            crate::okf::MemoryStore::default(),
        )));
        let mut table = HashMap::new();
        table.insert("the original wording".to_string(), vector(11, 768));
        table.insert("the corrected wording".to_string(), vector(22, 768));
        let embedder = std::sync::Arc::new(StaticEmbedder::new(table));

        let index = PrecomputedSemanticIndex::from_vectors(Vec::new(), embedder.clone())
            .with_store(store.clone());
        index
            .reconcile(
                &[(MemoryId::new("mem_one"), "the original wording".into())],
                0,
            )
            .await
            .expect("first");

        // Same id, new text — a correction.
        index
            .reconcile(
                &[(MemoryId::new("mem_one"), "the corrected wording".into())],
                0,
            )
            .await
            .expect("second");

        assert_eq!(index.len(), 1, "the record must not be duplicated");
        let hits = index.search_vector(&vector(22, 768), 1);
        assert_eq!(
            hits.first().map(|i| i.as_str()),
            Some("mem_one"),
            "the index still holds the vector for the superseded wording"
        );
    }

    /// A record dropped from the corpus must lose its stored vector, or a
    /// restore resurrects facts the user superseded.
    #[tokio::test]
    async fn retiring_a_record_removes_it_from_the_store_too() {
        let backing = std::sync::Arc::new(crate::okf::MemoryStore::default());
        let store = std::sync::Arc::new(OkfVectorStore::new(backing.clone()));
        let mut table = HashMap::new();
        table.insert("kept".to_string(), vector(1, 768));
        table.insert("dropped".to_string(), vector(2, 768));

        let index = PrecomputedSemanticIndex::from_vectors(
            Vec::new(),
            std::sync::Arc::new(StaticEmbedder::new(table)),
        )
        .with_store(store.clone());
        index
            .reconcile(
                &[
                    (MemoryId::new("mem_kept"), "kept".into()),
                    (MemoryId::new("mem_dropped"), "dropped".into()),
                ],
                0,
            )
            .await
            .expect("first");
        assert_eq!(store.load().await.expect("load").len(), 2);

        index
            .reconcile(&[(MemoryId::new("mem_kept"), "kept".into())], 0)
            .await
            .expect("second");
        let remaining = store.load().await.expect("load");
        assert_eq!(remaining.len(), 1, "the dropped vector is still on disk");
        assert_eq!(remaining[0].0.as_str(), "mem_kept");
    }

    #[tokio::test]
    async fn an_unknown_query_is_an_error_rather_than_a_silent_zero_vector() {
        let built = index(10, 768);
        assert!(built.search("never embedded", 5).await.is_err());
    }
}
