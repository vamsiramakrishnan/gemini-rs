//! The ranking primitives the retrieval experiments share.
//!
//! Three suites now fuse a lexical ranking with a semantic one, and they have
//! to do it *identically* or their numbers cannot be compared: one measures
//! which view retrieves best, one measures what width buys, and one measures
//! what compressing the vectors costs. A private copy in each would drift and
//! nobody would notice until two tables disagreed.
//!
//! Everything here mirrors what `LocalMemoryRetriever` does on the real path,
//! including using the crate's own [`reciprocal_rank_fusion`].

#![allow(dead_code)]

use gemini_memory_rs::bm25::{MemoryIndex, MemoryOrigin, Query, SearchExplanation, SearchHit};
use gemini_memory_rs::core::{MemoryId, MemoryKind};
use gemini_memory_rs::retrieval::{deterministic::topical_terms, reciprocal_rank_fusion};

/// How many results each retriever proposes before fusion.
pub const CANDIDATES: usize = 20;

/// Rank by BM25, exactly as the engine does on the tool path.
pub fn lexical(index: &MemoryIndex, question: &str) -> Vec<SearchHit> {
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

/// Wrap scored ids as search hits, so any ranker can be fused with any other.
pub fn as_hits(scored: &[(usize, f32)], ids: &[MemoryId]) -> Vec<SearchHit> {
    scored
        .iter()
        .map(|(index, score)| {
            let id = ids[*index].clone();
            SearchHit {
                id: id.clone(),
                score: *score,
                statement: String::new(),
                kind: MemoryKind::Preference,
                origin: MemoryOrigin::Canonical,
                explanation: SearchExplanation {
                    memory_id: id,
                    components: Vec::new(),
                    boosts: Vec::new(),
                    lexical_score: 0.0,
                    final_score: *score,
                },
            }
        })
        .collect()
}

/// Fuse rankings with the engine's own `1/(60 + rank)`.
///
/// Passing a ranking twice doubles its contribution, which is how the 2:1
/// weighting the experiments recommend is expressed.
pub fn fuse(rankings: &[&Vec<SearchHit>]) -> Vec<SearchHit> {
    let owned: Vec<Vec<SearchHit>> = rankings.iter().map(|r| (*r).clone()).collect();
    reciprocal_rank_fusion(&owned)
        .into_iter()
        .map(|c| c.hit)
        .collect()
}

/// Where the answer ranks, or `None` if it never appears.
pub fn rank_of(hits: &[SearchHit], target: &str) -> Option<usize> {
    hits.iter().position(|h| h.id.as_str() == target)
}
