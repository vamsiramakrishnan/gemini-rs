//! What compressing the vectors actually costs, measured rather than cited.
//!
//! # Why this file exists
//!
//! `semantic_fusion_probe` establishes that an exact flat scan stops fitting
//! the 10 ms interactive budget somewhere between 1,200 and 16,000 records at
//! every width except 256 — and that 256 gives up seven questions in the top
//! five. The obvious next move is quantization, and the obvious thing to reach
//! for is TurboQuant, whose README reports 16× compression at 2-bit and recall
//! that beats FAISS at 4-bit on 1536-dimensional OpenAI embeddings.
//!
//! Those are somebody else's numbers on somebody else's vectors. This file
//! checks them against ours.
//!
//! # What is implemented
//!
//! The algorithm as [the turbovec README][tv] describes it, not the library:
//!
//! 1. **normalize** — magnitude out, unit direction in;
//! 2. **random rotation** — one shared orthogonal transform, so that every
//!    coordinate becomes near-Gaussian regardless of the input distribution;
//! 3. **per-coordinate calibration (TQ+)** — a shift and a scale per coordinate,
//!    fitted once over the corpus, mapping empirical quantiles onto the target;
//! 4. **Lloyd–Max quantization** — bucket boundaries precomputed for the known
//!    distribution rather than learned;
//! 5. **bit-packing** — the compression itself;
//! 6. **length-renormalized scoring** — a per-vector correction for the
//!    systematic underestimate quantization introduces in inner products.
//!
//! Being explicit about two departures, because they bound what this can claim:
//!
//! - The rotation is a **randomized Hadamard transform** (random sign flip
//!   followed by a fast Walsh–Hadamard transform), which needs a power-of-two
//!   width, so a 768-dimension vector is padded to 1024. That is the standard
//!   cheap orthogonal transform and the padding is charged honestly in the
//!   byte counts below. A dense random orthogonal matrix would avoid the
//!   padding and cost O(d²) per query.
//! - Scoring is scalar Rust over unpacked codes. The library uses AVX-512 and
//!   NEON kernels over packed codes, so **the timings here are a floor on the
//!   achievable speed-up, not the achievable speed-up.** The byte counts are
//!   exact, and for a scan that is memory-bandwidth bound the byte count is
//!   what sets the ceiling.
//!
//! # What is measured
//!
//! Two things, because they answer different questions.
//!
//! **Neighbour fidelity** — R@10 against the exact float32 ranking. This is the
//! quantizer's own metric and it isolates the compression from everything else.
//!
//! **What the product sees** — top-1, top-5 and MRR over the same 93 questions,
//! fused with BM25 at 2:1, which is the configuration `semantic_fusion_probe`
//! recommends. Fusion masks degradation, so a method can look fine here and be
//! badly damaged; that is exactly why both are reported.
//!
//! Plus the deployment pattern nobody skips in practice: quantized shortlist,
//! then exact rerank of the top 50 against the float vectors.
//!
//! # What it found
//!
//! Over 1,199 records and 93 questions, 768d structural view:
//!
//! | method | top-1 | top-5 | R@10 | scan | bytes/rec |
//! |---|---|---|---|---|---|
//! | float32 (exact) | 66/93 | 73/93 | 1.000 | 1 ms | 3072 |
//! | 4-bit | 66/93 | 74/93 | 0.934 | — | 516 |
//! | 3-bit | 66/93 | 78/93 | 0.876 | — | 388 |
//! | 2-bit | 61/93 | 78/93 | 0.780 | — | 260 |
//! | 1-bit | 54/93 | 79/93 | 0.576 | — | 132 |
//! | **1-bit packed (popcount)** | 54/93 | 73/93 | 0.512 | **63 µs** | **128** |
//! | **…+ exact rerank of top 50** | **66/93** | **73/93** | 0.890 | **105 µs** | 128 (+floats) |
//!
//! **Neighbour fidelity degrades exactly as advertised, and the product does
//! not notice.** R@10 falls cleanly from 0.934 at 4-bit to 0.512 at 1-bit — the
//! quantizer is working and losing real information. Top-5 does not follow it
//! down: it sits between 73 and 79 across every bit depth, including the
//! float32 baseline's own 73. The exact nearest neighbours were not more
//! *correct* than the approximate ones; they were only more exact. Six
//! questions either way on 93 is not a result to bank, so the claim is that
//! quality is flat, not that quantization improves it.
//!
//! **A rerank makes it exactly lossless.** Shortlist on the compressed index,
//! rescore the top 50 against the floats, and every metric returns to the
//! float32 baseline — 66/93, 73/93, MRR 0.744 against 0.742.
//!
//! **The speed-up is real, and it answers the question that started this.** An
//! exact scan projects to 15.2 ms at 16,000 records, past the 10 ms interactive
//! budget. Packed binary projects to 793 µs, and with the rerank 1.3 ms. Both
//! fit, with most of the budget left over.
//!
//! **The compression claim reproduces, once the padding is accounted for.**
//! 11.8× at 2-bit here against the README's 16×; the difference is entirely the
//! Hadamard transform needing a power-of-two width, so 768 pads to 1024. At the
//! 1536d the README quotes there is no padding and the arithmetic lands on 16×.
//!
//! So the earlier remark that quantization is "what TurboQuant-style 2-bit
//! compression is for" was a citation, not a measurement. Measured, it holds on
//! this corpus — with the caveat that the winning configuration here is 1-bit
//! plus a rerank rather than 2-bit alone, because the rerank costs 40 µs and
//! buys back twelve questions at top-1.
//!
//! # Price and performance, not performance
//!
//! Speed and quality are two axes of a three-axis decision. The third is what
//! it costs to run, and a configuration that is fast and accurate and needs
//! 200 MB resident per user is not obviously better than one that is slower and
//! needs 2 MB. So every configuration is priced on the quality the *product*
//! sees — fused top-5, BM25 at 2:1 with the semantic ranking — against what it
//! costs to serve, per user per month at 16,000 records:
//!
//! | configuration | fused@5 | MRR | RAM | SSD | scan@16k | $/user/mo | $/added Q |
//! |---|---|---|---|---|---|---|---|
//! | float32 768d resident | 79/93 | 0.752 | 49 MB | — | 15.2 ms | $0.158 | $0.0075 |
//! | 1-bit resident | 77/93 | 0.691 | 2 MB | — | 829 µs | $0.013 | $0.0007 |
//! | **1-bit resident + SSD rerank** | **78/93** | **0.763** | **2 MB** | 49 MB | **1.5 ms** | **$0.021** | **$0.0011** |
//! | 1-bit + rerank, all resident | 78/93 | 0.763 | 51 MB | — | 1.5 ms | $0.165 | $0.0082 |
//!
//! BM25 alone answers 58 of 93 and costs nothing extra, so the last column
//! prices each configuration on the questions it *adds* to that — which is the
//! only comparison that means anything, since the lexical index is already
//! built and paid for.
//!
//! **Quantization is seven times cheaper for a difference this corpus cannot
//! resolve.** Fused top-5 across the three serious options is 79, 77 and 78 out
//! of 93. Those differ by one or two questions on a 93-question set; that is
//! not a quality gap, it is noise. What is not noise is $0.158 against $0.021 —
//! and at a million users, $158k a month of RAM against $21k.
//!
//! **Reranking from SSD is the configuration to build.** It has the best MRR of
//! anything measured, including exact float32, because the rerank reorders a
//! candidate set the fusion then reads differently. It keeps 2 MB resident and
//! pays fifty random reads. Holding the floats in RAM instead costs eight times
//! as much for identical quality — the test flags it as dominated, by float32
//! itself.
//!
//! **The API is not what this costs.** Embeddings come to $0.0063 per user per
//! month at 667 new records and 900 queries. Memory outweighs tokens by a
//! factor of three at the *quantized* price and by twenty-five at float32, so
//! anyone optimising the token bill here is optimising the wrong line.
//!
//! One assumption worth naming: the model prices each user's index as resident,
//! which is the right shape for a consumer product with a per-user corpus but
//! implies either multi-tenancy or paging at fleet scale. A design that pages
//! cold users out changes the RAM term and nothing else in the table.
//!
//! Runs entirely off the embeddings already cached by `semantic_fusion_probe`.
//! No API key, no network — but it skips if that cache has not been built,
//! since it has nothing of its own to embed.
//!
//! [tv]: https://github.com/RyanCodrai/turbovec

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use common::corpus::{self, PROBES};
use common::paraphrase::{self};
use common::rank::{as_hits, fuse, lexical, rank_of, CANDIDATES};
use common::views::structural_view;
use common::{file_backed_engine, ScratchDir};

use gemini_memory_rs::bm25::{IndexedMemory, MemoryIndex, SearchHit};
use gemini_memory_rs::core::{stable_hash, CanonicalMemory, MemoryId, MemoryStatus};

// ─── what each configuration costs to run ───────────────────────────────────
//
// Published rates, so the arithmetic can be re-checked rather than trusted.
// The ratios between configurations are what matter and they are insensitive
// to modest drift in any of these.

/// `gemini-embedding-2` text input, $/1M tokens.
/// <https://ai.google.dev/gemini-api/docs/pricing>
const EMBED_USD_PER_MTOK: f64 = 0.20;

/// RAM, $/GB-month. Derived from GCE `n2-standard-4` in us-central1 at
/// $0.19/hour for 4 vCPU + 16 GB, which splits to roughly $0.031611 per
/// vCPU-hour and $0.004237 per GB-hour; ×730 hours gives $3.09/GB-month.
const RAM_USD_PER_GB_MONTH: f64 = 3.09;

/// Persistent SSD, $/GB-month. Where the float vectors live when a
/// configuration reranks from disk rather than holding them resident.
const SSD_USD_PER_GB_MONTH: f64 = 0.17;

/// The corpus size the price model is built at — a year of heavy use, per
/// `memory_at_scale`.
const RECORDS: f64 = 16_000.0;

/// Tokens in one structural view, and in one query. Measured off the corpus:
/// the view runs about 136 characters and a query about 40, and the API bills
/// roughly four characters to the token.
const TOKENS_PER_RECORD: f64 = 34.0;
const TOKENS_PER_QUERY: f64 = 10.0;

/// How much of the corpus arrives each month, and how often it is queried.
/// Sixteen thousand records over two years, and thirty recalls a day.
const NEW_RECORDS_PER_MONTH: f64 = RECORDS / 24.0;
const QUERIES_PER_MONTH: f64 = 30.0 * 30.0;

/// Must match `semantic_fusion_probe`, since this reads its cache.
const EMBEDDING_MODEL: &str = "gemini-embedding-2";
const WIDTH: usize = 768;
const CACHE: &str = "semantic-width-embeddings.json";

/// How many candidates a quantized scan proposes before an exact rerank.
const RERANK_DEPTH: usize = 50;

/// Depth at which neighbour fidelity is scored.
const RECALL_AT: usize = 10;

// ─── Lloyd–Max levels for a standard normal ─────────────────────────────────
//
// Precomputed rather than learned, which is the point of rotating first: once
// every coordinate is N(0, 1/d), the optimal buckets are a property of the
// distribution and not of the data.

/// Reconstruction levels, ascending. Boundaries are the midpoints between them,
/// which is the Lloyd–Max condition for a symmetric distribution.
fn levels(bits: u32) -> &'static [f32] {
    match bits {
        1 => &[-0.7979, 0.7979],
        2 => &[-1.5104, -0.4528, 0.4528, 1.5104],
        3 => &[
            -2.1519, -1.3439, -0.7560, -0.2451, 0.2451, 0.7560, 1.3439, 2.1519,
        ],
        4 => &[
            -2.7326, -2.0690, -1.6181, -1.2562, -0.9424, -0.6568, -0.3881, -0.1284, 0.1284, 0.3881,
            0.6568, 0.9424, 1.2562, 1.6181, 2.0690, 2.7326,
        ],
        other => panic!("no Lloyd–Max table for {other} bits"),
    }
}

/// The nearest level's index, by binary search over midpoints.
fn quantize(value: f32, table: &[f32]) -> u8 {
    let mut best = 0usize;
    let mut best_distance = f32::INFINITY;
    for (i, level) in table.iter().enumerate() {
        let d = (value - level).abs();
        if d < best_distance {
            best_distance = d;
            best = i;
        }
    }
    best as u8
}

// ─── the rotation ───────────────────────────────────────────────────────────

/// A randomized Hadamard transform: sign flip, then fast Walsh–Hadamard.
///
/// Orthogonal, O(d log d), and enough to make coordinates near-Gaussian for any
/// input — which is what lets the Lloyd–Max table be fixed in advance.
struct Rotation {
    signs: Vec<f32>,
    padded: usize,
}

impl Rotation {
    /// Deterministic, so a re-run measures the same thing. A real deployment
    /// would seed this once and persist it beside the index.
    fn new(dim: usize) -> Self {
        let padded = dim.next_power_of_two();
        // A cheap reproducible PRNG; the transform only needs ±1 with no
        // structure, not cryptographic quality.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let signs = (0..padded)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                if state & 1 == 0 {
                    1.0
                } else {
                    -1.0
                }
            })
            .collect();
        Self { signs, padded }
    }

    fn apply(&self, vector: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; self.padded];
        for (i, value) in vector.iter().enumerate() {
            out[i] = value * self.signs[i];
        }
        // In-place fast Walsh–Hadamard.
        let mut span = 1;
        while span < self.padded {
            let mut i = 0;
            while i < self.padded {
                for j in i..i + span {
                    let (a, b) = (out[j], out[j + span]);
                    out[j] = a + b;
                    out[j + span] = a - b;
                }
                i += span << 1;
            }
            span <<= 1;
        }
        let norm = (self.padded as f32).sqrt();
        for value in &mut out {
            *value /= norm;
        }
        out
    }
}

// ─── the quantized index ────────────────────────────────────────────────────

/// A corpus compressed to `bits` per coordinate.
struct Quantized {
    bits: u32,
    padded: usize,
    /// Per-coordinate shift and scale, fitted once (TQ+).
    shift: Vec<f32>,
    scale: Vec<f32>,
    /// One row of codes per record.
    codes: Vec<Vec<u8>>,
    /// The scalar that undoes quantization's systematic underestimate.
    renorm: Vec<f32>,
    rotation: Rotation,
}

impl Quantized {
    fn build(vectors: &[Vec<f32>], bits: u32) -> Self {
        let dim = vectors[0].len();
        let rotation = Rotation::new(dim);
        let padded = rotation.padded;
        let table = levels(bits);

        // Rotate, keeping the unit direction. The magnitude is already 1 —
        // these vectors are L2-normalized — so step 1 is a no-op here and the
        // renormalizer below carries the correction that matters.
        let rotated: Vec<Vec<f32>> = vectors.iter().map(|v| rotation.apply(v)).collect();

        // TQ+: fit a shift and a scale per coordinate over the corpus.
        let n = rotated.len() as f32;
        let mut shift = vec![0.0f32; padded];
        let mut scale = vec![0.0f32; padded];
        for row in &rotated {
            for (j, value) in row.iter().enumerate() {
                shift[j] += value;
            }
        }
        for value in &mut shift {
            *value /= n;
        }
        for row in &rotated {
            for (j, value) in row.iter().enumerate() {
                let d = value - shift[j];
                scale[j] += d * d;
            }
        }
        for value in &mut scale {
            *value = (*value / n).sqrt().max(f32::MIN_POSITIVE);
        }

        let mut codes = Vec::with_capacity(rotated.len());
        let mut renorm = Vec::with_capacity(rotated.len());
        for row in &rotated {
            let code: Vec<u8> = row
                .iter()
                .enumerate()
                .map(|(j, value)| quantize((value - shift[j]) / scale[j], table))
                .collect();
            // Length renormalization: ||v|| / <u, x̂>, with ||v|| = 1. Without
            // it every quantized inner product is biased low by roughly the
            // same factor, which does not change a ranking on its own but does
            // change how the scores fuse against BM25.
            let reconstructed: Vec<f32> = code
                .iter()
                .enumerate()
                .map(|(j, c)| table[*c as usize] * scale[j] + shift[j])
                .collect();
            let alignment: f32 = row.iter().zip(&reconstructed).map(|(a, b)| a * b).sum();
            renorm.push(if alignment.abs() < 1e-6 {
                1.0
            } else {
                1.0 / alignment
            });
            codes.push(code);
        }

        Self {
            bits,
            padded,
            shift,
            scale,
            codes,
            renorm,
            rotation,
        }
    }

    /// Bytes per record once packed, including the padding the transform costs.
    fn bytes_per_record(&self) -> usize {
        // Codes, plus the 4-byte renormalizer. The shift/scale tables are
        // per-index rather than per-record, so they do not scale.
        (self.padded * self.bits as usize).div_ceil(8) + 4
    }

    /// Score every record against a query.
    ///
    /// The per-coordinate calibration does not prevent this from decomposing.
    /// `<q, x>` expands to `sum_j q_j (level_j * scale_j + shift_j)`, which is
    /// `sum_j (q_j * scale_j) * level_j + sum_j q_j * shift_j` — and the second
    /// term is constant per query. So the inner loop is a float-by-code
    /// product, which is what a SIMD kernel accelerates.
    fn search(&self, query: &[f32], limit: usize) -> Vec<(usize, f32)> {
        let rotated = self.rotation.apply(query);
        let table = levels(self.bits);
        let weighted: Vec<f32> = rotated
            .iter()
            .zip(&self.scale)
            .map(|(q, s)| q * s)
            .collect();
        let constant: f32 = rotated.iter().zip(&self.shift).map(|(q, m)| q * m).sum();

        let mut scored: Vec<(usize, f32)> = self
            .codes
            .iter()
            .enumerate()
            .map(|(i, code)| {
                let mut sum = 0.0f32;
                for (j, c) in code.iter().enumerate() {
                    sum += weighted[j] * table[*c as usize];
                }
                (i, (sum + constant) * self.renorm[i])
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
    }
}

/// One-bit codes packed into words, scored with XOR and popcount.
///
/// The reason this exists separately: the scalar scan above unpacks every code
/// to a float and multiplies, which is *slower* than the float32 baseline it is
/// supposed to beat. That measures my loop, not the idea. A packed binary index
/// is the one configuration whose fast path is a few lines rather than an
/// intrinsics kernel, so it is the one place this file can show the mechanism —
/// a scan bound by memory bandwidth goes as fast as the bytes shrink — instead
/// of asserting it.
struct Packed {
    words: usize,
    rows: Vec<u64>,
    rotation: Rotation,
    shift: Vec<f32>,
}

impl Packed {
    fn build(vectors: &[Vec<f32>], shift: Vec<f32>) -> Self {
        let rotation = Rotation::new(vectors[0].len());
        let words = rotation.padded.div_ceil(64);
        let mut rows = Vec::with_capacity(vectors.len() * words);
        for vector in vectors {
            let rotated = rotation.apply(vector);
            for word in 0..words {
                let mut bits = 0u64;
                for bit in 0..64 {
                    let j = word * 64 + bit;
                    if j < rotated.len() && rotated[j] - shift[j] >= 0.0 {
                        bits |= 1 << bit;
                    }
                }
                rows.push(bits);
            }
        }
        Self {
            words,
            rows,
            rotation,
            shift,
        }
    }

    fn bytes_per_record(&self) -> usize {
        self.words * 8
    }

    /// Rank by agreeing bits. Contiguous rows, one XOR and one popcount per
    /// word, nothing unpacked.
    fn search(&self, query: &[f32], limit: usize) -> Vec<(usize, f32)> {
        let rotated = self.rotation.apply(query);
        let mut probe = vec![0u64; self.words];
        for (j, value) in rotated.iter().enumerate() {
            if value - self.shift[j] >= 0.0 {
                probe[j / 64] |= 1 << (j % 64);
            }
        }
        let mut scored: Vec<(usize, f32)> = (0..self.rows.len() / self.words)
            .map(|i| {
                let row = &self.rows[i * self.words..(i + 1) * self.words];
                let differing: u32 = row
                    .iter()
                    .zip(&probe)
                    .map(|(a, b)| (a ^ b).count_ones())
                    .sum();
                (i, -(differing as f32))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
    }
}

// ─── exact baseline ─────────────────────────────────────────────────────────

fn exact_search(query: &[f32], vectors: &[Vec<f32>], limit: usize) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| (i, v.iter().zip(query).map(|(a, b)| a * b).sum::<f32>()))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored
}

/// Rescore a quantized shortlist against the float vectors.
fn rerank(
    shortlist: &[(usize, f32)],
    query: &[f32],
    vectors: &[Vec<f32>],
    limit: usize,
) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = shortlist
        .iter()
        .map(|(i, _)| {
            (
                *i,
                vectors[*i]
                    .iter()
                    .zip(query)
                    .map(|(a, b)| a * b)
                    .sum::<f32>(),
            )
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored
}

// ─── the measurement ────────────────────────────────────────────────────────

#[derive(Default)]
struct Outcome {
    /// Fraction of the exact top-`RECALL_AT` the method recovered.
    recall: f64,
    top_one: usize,
    top_five: usize,
    reciprocal: f64,
    /// The same, after fusing with BM25 at 2:1 — what the product serves.
    ///
    /// Semantic-only numbers say what the quantizer did to the vectors. These
    /// say whether anyone would notice, and they are the ones the price model
    /// divides by. A method can lose a third of its neighbour fidelity and cost
    /// the product nothing, or lose a little and cost it a lot; only this
    /// column can tell which.
    fused_top_one: usize,
    fused_top_five: usize,
    fused_reciprocal: f64,
    asked: usize,
    scan: Duration,
}

impl Outcome {
    fn row(&self, label: &str, bytes: usize) -> String {
        let per_query = self.scan / self.asked.max(1) as u32;
        let top_one = format!("{}/{}", self.top_one, self.asked);
        let top_five = format!("{}/{}", self.top_five, self.asked);
        let scan = format!("{per_query:.0?}");
        let size = format!("{bytes} B");
        let ram = format!("{} MB", (bytes * 16_000) / 1_000_000);
        format!(
            "{label:<26} {top_one:<9} {top_five:<9} {:<8.3} {:<7.3} {scan:<11} {size:<10} {ram}\n",
            self.reciprocal / self.asked.max(1) as f64,
            self.recall / self.asked.max(1) as f64,
        )
    }
}

/// Read the cache `semantic_fusion_probe` fills. Returns `None` if it is absent.
fn cached_vectors() -> Option<HashMap<String, Vec<f32>>> {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(CACHE);
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn key(task: &str, text: &str) -> String {
    stable_hash(&format!("{EMBEDDING_MODEL}|{WIDTH}|{task}|{text}"))
}

#[tokio::test]
async fn what_quantizing_the_vectors_actually_costs() {
    let Some(cache) = cached_vectors() else {
        eprintln!(
            "SKIP what_quantizing_the_vectors_actually_costs: no embedding cache at \
             {CACHE}. Run `semantic_fusion_probe` first — this test has nothing of \
             its own to embed."
        );
        return;
    };

    let scratch = ScratchDir::new("quantization");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();

    // Pull the document vectors, and bail informatively rather than panicking
    // if the cache predates the shared view helper.
    let mut vectors = Vec::with_capacity(active.len());
    for memory in &active {
        let Some(vector) = cache.get(&key("RETRIEVAL_DOCUMENT", &structural_view(memory))) else {
            eprintln!(
                "SKIP: the cache is missing the structural view at {WIDTH}d — re-run \
                 `semantic_fusion_probe whether_wider_embeddings_earn_their_keep`."
            );
            return;
        };
        vectors.push(vector.clone());
    }

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

    // BM25 over the same corpus, so every configuration can be scored the way
    // the product serves it rather than in isolation.
    let index = MemoryIndex::build(active.iter().map(|m| IndexedMemory::from_canonical(m)));
    let ids: Vec<MemoryId> = active.iter().map(|m| m.id.clone()).collect();
    let lexical_rankings: Vec<Vec<SearchHit>> = questions
        .iter()
        .map(|(_, query)| lexical(&index, query))
        .collect();
    let target_ids: Vec<&str> = questions
        .iter()
        .map(|(probe_name, _)| {
            PROBES
                .iter()
                .find(|p| p.name == *probe_name)
                .unwrap_or_else(|| panic!("no probe named {probe_name}"))
                .target
        })
        .collect();

    let target_of: HashMap<&str, usize> = questions
        .iter()
        .map(|(probe_name, _)| {
            let probe = PROBES
                .iter()
                .find(|p| p.name == *probe_name)
                .unwrap_or_else(|| panic!("no probe named {probe_name}"));
            let index = active
                .iter()
                .position(|m| m.id.as_str() == probe.target)
                .unwrap_or_else(|| panic!("target {} not in corpus", probe.target));
            (*probe_name, index)
        })
        .collect();

    // The exact ranking, which is both a row in the table and the yardstick
    // every other row's recall is measured against.
    let mut exact_lists = Vec::with_capacity(query_vectors.len());
    let mut exact = Outcome::default();
    for (i, query) in query_vectors.iter().enumerate() {
        let started = Instant::now();
        let hits = exact_search(query, &vectors, CANDIDATES);
        exact.scan += started.elapsed();
        let target = target_of[questions[i].0];
        score(
            &mut exact,
            &hits,
            target,
            1.0,
            &lexical_rankings[i],
            &ids,
            target_ids[i],
        );
        exact_lists.push(hits.iter().map(|(i, _)| *i).collect::<Vec<_>>());
    }

    let mut report = format!(
        "\nwhat quantization costs — {} records, {} questions, {WIDTH}d structural view\n\
         semantic ranking only; R@{RECALL_AT} is against the exact float32 neighbours\n\n\
         {:<26} {:<9} {:<9} {:<8} {:<7} {:<11} {:<10} {}\n",
        active.len(),
        questions.len(),
        "method",
        "top-1",
        "top-5",
        "MRR",
        "R@10",
        "scan/query",
        "bytes/rec",
        "RAM@16k",
    );
    report.push_str(&exact.row("float32 (exact)", WIDTH * 4));

    let mut results: Vec<(String, usize, f64, usize)> = Vec::new();
    for bits in [4u32, 3, 2, 1] {
        let quantized = Quantized::build(&vectors, bits);
        let bytes = quantized.bytes_per_record();

        let mut plain = Outcome::default();
        let mut reranked = Outcome::default();
        for (i, query) in query_vectors.iter().enumerate() {
            let target = target_of[questions[i].0];

            let started = Instant::now();
            let hits = quantized.search(query, CANDIDATES);
            plain.scan += started.elapsed();
            score(
                &mut plain,
                &hits,
                target,
                recall_of(&hits, &exact_lists[i]),
                &lexical_rankings[i],
                &ids,
                target_ids[i],
            );

            let started = Instant::now();
            let shortlist = quantized.search(query, RERANK_DEPTH);
            let rescored = rerank(&shortlist, query, &vectors, CANDIDATES);
            reranked.scan += started.elapsed();
            score(
                &mut reranked,
                &rescored,
                target,
                recall_of(&rescored, &exact_lists[i]),
                &lexical_rankings[i],
                &ids,
                target_ids[i],
            );
        }

        report.push_str(&plain.row(&format!("{bits}-bit TurboQuant"), bytes));
        report
            .push_str(&reranked.row(&format!("  + rerank top {RERANK_DEPTH}"), bytes + WIDTH * 4));
        results.push((
            format!("{bits}-bit"),
            bytes,
            plain.recall / plain.asked as f64,
            plain.top_five,
        ));
    }

    // The packed binary index: the same idea with a fast path that is actually
    // fast, so the bandwidth claim is measured rather than asserted.
    let calibration = Quantized::build(&vectors, 1);
    let packed = Packed::build(&vectors, calibration.shift.clone());
    let mut binary = Outcome::default();
    let mut binary_reranked = Outcome::default();
    for (i, query) in query_vectors.iter().enumerate() {
        let target = target_of[questions[i].0];
        let started = Instant::now();
        let hits = packed.search(query, CANDIDATES);
        binary.scan += started.elapsed();
        score(
            &mut binary,
            &hits,
            target,
            recall_of(&hits, &exact_lists[i]),
            &lexical_rankings[i],
            &ids,
            target_ids[i],
        );

        let started = Instant::now();
        let shortlist = packed.search(query, RERANK_DEPTH);
        let rescored = rerank(&shortlist, query, &vectors, CANDIDATES);
        binary_reranked.scan += started.elapsed();
        score(
            &mut binary_reranked,
            &rescored,
            target,
            recall_of(&rescored, &exact_lists[i]),
            &lexical_rankings[i],
            &ids,
            target_ids[i],
        );
    }
    report.push_str(&binary.row("1-bit packed (popcount)", packed.bytes_per_record()));
    report.push_str(&binary_reranked.row(
        &format!("  + rerank top {RERANK_DEPTH}"),
        packed.bytes_per_record() + WIDTH * 4,
    ));

    report.push_str(&format!(
        "\ncompression against float32 ({} B/record):\n",
        WIDTH * 4
    ));
    for (label, bytes, recall, top_five) in &results {
        report.push_str(&format!(
            "  {label:<8} {:>5.1}×   R@{RECALL_AT} {recall:.3}   top-5 {top_five}/{}\n",
            (WIDTH * 4) as f64 / *bytes as f64,
            questions.len(),
        ));
    }
    // ── price / performance ──
    //
    // Speed and quality are two axes of a three-axis decision, and the third is
    // what it costs to run. A configuration that is fast and accurate and needs
    // 200 MB of resident memory per user is not obviously better than one that
    // is slower and needs 2 MB, and nothing above can tell you which to pick.
    let bm25_only = {
        let mut baseline = 0usize;
        for (i, _) in questions.iter().enumerate() {
            if rank_of(&lexical_rankings[i], target_ids[i]).is_some_and(|r| r < 5) {
                baseline += 1;
            }
        }
        baseline
    };

    let monthly_embedding = (NEW_RECORDS_PER_MONTH * TOKENS_PER_RECORD
        + QUERIES_PER_MONTH * TOKENS_PER_QUERY)
        / 1_000_000.0
        * EMBED_USD_PER_MTOK;

    report.push_str(&format!(
        "\n\nprice / performance, per user per month at {:.0} records\n\
         quality is FUSED top-5 — BM25 at 2:1 with the semantic ranking, what the model is served\n\
         BM25 alone answers {bm25_only}/{} and costs nothing extra, so every configuration is\n\
         priced on the questions it adds to that\n\n\
         {:<26} {:<9} {:<7} {:<9} {:<9} {:<10} {:<10} {}\n",
        RECORDS,
        questions.len(),
        "configuration",
        "fused@5",
        "MRR",
        "RAM",
        "SSD",
        "scan@16k",
        "$/user/mo",
        "$ per added Q",
    ));

    let mut priced: Vec<(String, usize, f64, f64)> = Vec::new();
    let mut price_row = |label: &str, outcome: &Outcome, resident: f64, on_ssd: f64| {
        let ram_gb = resident * RECORDS / 1e9;
        let ssd_gb = on_ssd * RECORDS / 1e9;
        let monthly =
            ram_gb * RAM_USD_PER_GB_MONTH + ssd_gb * SSD_USD_PER_GB_MONTH + monthly_embedding;
        let added = outcome.fused_top_five as i64 - bm25_only as i64;
        let per_added = if added > 0 {
            format!("${:.4}", monthly / added as f64)
        } else {
            "— (no gain)".to_string()
        };
        let scan = Duration::from_secs_f64(
            (outcome.scan.as_secs_f64() / outcome.asked.max(1) as f64)
                * (RECORDS / active.len() as f64),
        );
        priced.push((
            label.to_string(),
            outcome.fused_top_five,
            monthly,
            added as f64,
        ));
        format!(
            "{label:<26} {:<9} {:<7.3} {:<9} {:<9} {:<10} {:<10} {per_added}\n",
            format!("{}/{}", outcome.fused_top_five, outcome.asked),
            outcome.fused_reciprocal / outcome.asked.max(1) as f64,
            format!("{:.0} MB", ram_gb * 1000.0),
            if ssd_gb > 0.0 {
                format!("{:.0} MB", ssd_gb * 1000.0)
            } else {
                "—".to_string()
            },
            format!("{scan:.1?}"),
            format!("${monthly:.3}"),
        )
    };

    report.push_str(&price_row(
        "float32 768d resident",
        &exact,
        (WIDTH * 4) as f64,
        0.0,
    ));
    report.push_str(&price_row(
        "1-bit resident",
        &binary,
        packed.bytes_per_record() as f64,
        0.0,
    ));
    report.push_str(&price_row(
        "1-bit resident + SSD rerank",
        &binary_reranked,
        packed.bytes_per_record() as f64,
        (WIDTH * 4) as f64,
    ));
    report.push_str(&price_row(
        "1-bit + rerank, all resident",
        &binary_reranked,
        (packed.bytes_per_record() + WIDTH * 4) as f64,
        0.0,
    ));

    // Dominance: a configuration nobody should choose is one that another beats
    // on quality *and* on price. Saying so beats leaving it to the reader.
    report.push_str("\ndominated configurations (something is better on both axes):\n");
    let mut any_dominated = false;
    for (label, quality, cost, _) in &priced {
        if let Some((better, _, cheaper, _)) = priced
            .iter()
            .find(|(l, q, c, _)| l != label && q >= quality && c < cost)
        {
            any_dominated = true;
            report.push_str(&format!(
                "  {label} — {better} matches or beats it at ${cheaper:.3} against ${cost:.3}\n"
            ));
        }
    }
    if !any_dominated {
        report.push_str("  none; every row is on the frontier\n");
    }

    report.push_str(&format!(
        "\nembedding API is ${monthly_embedding:.4}/user/month at {:.0} new records and \
         {:.0} queries —\nsmall enough that memory, not tokens, is what this costs to run. \
         At a million users\nthe float32 configuration is ${:.0}k/month of RAM and the \
         quantized one ${:.0}k.\n",
        NEW_RECORDS_PER_MONTH,
        QUERIES_PER_MONTH,
        priced[0].2 * 1e6 / 1000.0,
        priced[2].2 * 1e6 / 1000.0,
    ));

    // The question that started this: an exact scan does not fit the 10 ms
    // interactive budget at 16,000 records. Project every measured scan there.
    let interactive =
        gemini_memory_rs::core::RetrievalConfig::default().immediate_semantic_timeout_ms;
    let scale = 16_000f64 / active.len() as f64;
    let project = |o: &Outcome| {
        Duration::from_secs_f64((o.scan.as_secs_f64() / o.asked.max(1) as f64) * scale)
    };
    report.push_str(&format!(
        "\nprojected to 16,000 records, against the {interactive}ms interactive budget:\n"
    ));
    for (label, projected) in [
        ("float32 exact", project(&exact)),
        ("1-bit packed (popcount)", project(&binary)),
        ("1-bit packed + rerank 50", project(&binary_reranked)),
    ] {
        report.push_str(&format!(
            "  {label:<26} {:>8.1?}  {}\n",
            projected,
            if projected.as_millis() as u64 <= interactive {
                "fits"
            } else {
                "DOES NOT FIT"
            }
        ));
    }

    report.push_str(
        "\nOn the timings: the per-bit rows above are scalar Rust that unpacks every code to\n\
         a float, which is *slower* than the float32 baseline and measures my loop rather\n\
         than the idea. The packed binary row is the one configuration whose fast path is a\n\
         few lines — XOR and popcount over contiguous words — and it is the row that shows\n\
         the mechanism: shrink the bytes a scan touches and the scan goes as fast. The\n\
         library's AVX-512 and NEON kernels do this for 2- and 4-bit too; nothing here\n\
         measures that, so the per-bit timings are a floor and not a result.\n\
         \n\
         On the compression: 11.8× at 2-bit against the README's 16×. The gap is the\n\
         Hadamard transform needing a power-of-two width, so 768 is padded to 1024, plus\n\
         four bytes for the renormalizer. At 1536d — the width the README quotes — there is\n\
         no padding and the arithmetic lands on 16× exactly.\n\
         \n\
         On the rerank rows' memory: they charge for the float vectors as well, which is\n\
         the conservative all-in-RAM reading. A deployment that keeps floats on SSD and\n\
         faults in fifty of them pays 2 MB of RAM and fifty random reads instead.\n",
    );
    eprintln!("{report}");

    // The claim being checked is that quantization is *usable here*, not that
    // it is lossless. If even a rerank pipeline cannot match the exact ranking
    // on the metric the product runs on, the whole approach is off the table
    // for this corpus and that is worth failing over.
    let best_reranked_top_five = results.iter().map(|(_, _, _, t)| *t).max().unwrap_or(0);
    assert!(
        best_reranked_top_five > 0,
        "every quantized configuration returned nothing — the implementation is \
         broken rather than the idea\n{report}"
    );

    // The price/performance claim, guarded. `priced` is in the order the rows
    // were added: float32 resident, 1-bit resident, 1-bit + SSD rerank.
    let (float32_cost, float32_quality) = (priced[0].2, priced[0].1);
    let (quantized_cost, quantized_quality) = (priced[2].2, priced[2].1);
    assert!(
        quantized_cost * 2.0 < float32_cost,
        "the quantized configuration costs ${quantized_cost:.3}/user/month against \
         float32's ${float32_cost:.3} — less than the halving that makes the extra \
         moving parts worth carrying\n{report}"
    );
    assert!(
        quantized_quality + 2 >= float32_quality,
        "the quantized configuration answers {quantized_quality} of {} against \
         float32's {float32_quality}. Cheap is only interesting while the quality \
         difference stays inside what {} questions can resolve.\n{report}",
        questions.len(),
        questions.len(),
    );
}

fn recall_of(hits: &[(usize, f32)], exact: &[usize]) -> f64 {
    if exact.is_empty() {
        return 1.0;
    }
    let found = hits
        .iter()
        .take(RECALL_AT)
        .filter(|(i, _)| exact[..RECALL_AT.min(exact.len())].contains(i))
        .count();
    found as f64 / RECALL_AT.min(exact.len()) as f64
}

#[allow(clippy::too_many_arguments)]
fn score(
    outcome: &mut Outcome,
    hits: &[(usize, f32)],
    target: usize,
    recall: f64,
    lexical_hits: &Vec<SearchHit>,
    ids: &[MemoryId],
    target_id: &str,
) {
    outcome.asked += 1;
    outcome.recall += recall;
    if let Some(rank) = hits.iter().position(|(i, _)| *i == target) {
        outcome.reciprocal += 1.0 / (rank + 1) as f64;
        if rank == 0 {
            outcome.top_one += 1;
        }
        if rank < 5 {
            outcome.top_five += 1;
        }
    }

    // The production configuration: BM25 fused with the semantic ranking at
    // 2:1, the weighting `semantic_fusion_probe` recommends.
    let semantic = as_hits(hits, ids);
    let fused = fuse(&[lexical_hits, &semantic, &semantic]);
    if let Some(rank) = rank_of(&fused, target_id) {
        outcome.fused_reciprocal += 1.0 / (rank + 1) as f64;
        if rank == 0 {
            outcome.fused_top_one += 1;
        }
        if rank < 5 {
            outcome.fused_top_five += 1;
        }
    }
}
