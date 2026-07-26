//! Reciprocal rank fusion across independent lexical queries.
//!
//! A plan issues several queries because they fail differently: an entity query
//! finds everything about a person but ranks it arbitrarily; a topic query
//! ranks well but misses records that never name the topic. Fusing on *rank*
//! rather than score means one query's score scale cannot dominate another's.

use std::collections::HashMap;

use crate::bm25::SearchHit;
use crate::core::MemoryId;

/// The RRF smoothing constant. 60 is the value from the original TREC work and
/// behaves well when result lists are short.
pub const RRF_K: f32 = 60.0;

/// A candidate after fusion.
#[derive(Debug, Clone)]
pub struct FusedCandidate {
    /// The best-scoring hit for this record across all queries.
    pub hit: SearchHit,
    /// Fused rank score.
    pub rrf_score: f32,
    /// How many queries surfaced this record.
    pub appearances: usize,
    /// Best rank achieved in any single query (0-based).
    pub best_rank: usize,
}

impl FusedCandidate {
    /// The record's identity.
    pub fn id(&self) -> &MemoryId {
        &self.hit.id
    }
}

/// Fuse per-query result lists into one ranking.
pub fn reciprocal_rank_fusion(rankings: &[Vec<SearchHit>]) -> Vec<FusedCandidate> {
    let mut fused: HashMap<MemoryId, FusedCandidate> = HashMap::new();

    for ranking in rankings {
        for (rank, hit) in ranking.iter().enumerate() {
            let contribution = 1.0 / (RRF_K + rank as f32 + 1.0);
            fused
                .entry(hit.id.clone())
                .and_modify(|existing| {
                    existing.rrf_score += contribution;
                    existing.appearances += 1;
                    existing.best_rank = existing.best_rank.min(rank);
                    if hit.score > existing.hit.score {
                        existing.hit = hit.clone();
                    }
                })
                .or_insert_with(|| FusedCandidate {
                    hit: hit.clone(),
                    rrf_score: contribution,
                    appearances: 1,
                    best_rank: rank,
                });
        }
    }

    let mut out: Vec<FusedCandidate> = fused.into_values().collect();
    out.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.hit
                    .score
                    .partial_cmp(&a.hit.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.hit.id.as_str().cmp(b.hit.id.as_str()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::{MemoryOrigin, SearchExplanation};
    use crate::core::MemoryKind;

    fn hit(id: &str, score: f32) -> SearchHit {
        SearchHit {
            id: MemoryId::new(id),
            score,
            statement: format!("statement for {id}"),
            kind: MemoryKind::Preference,
            origin: MemoryOrigin::Canonical,
            explanation: SearchExplanation {
                memory_id: MemoryId::new(id),
                components: Vec::new(),
                boosts: Vec::new(),
                lexical_score: score,
                final_score: score,
            },
        }
    }

    #[test]
    fn a_record_found_by_several_queries_outranks_one_found_once() {
        // `mem_b` tops one list but appears nowhere else; `mem_a` is second in
        // both, and agreement across queries wins.
        let fused = reciprocal_rank_fusion(&[
            vec![hit("mem_b", 9.0), hit("mem_a", 5.0)],
            vec![hit("mem_c", 8.0), hit("mem_a", 5.0)],
        ]);
        assert_eq!(fused[0].id().as_str(), "mem_a");
        assert_eq!(fused[0].appearances, 2);
    }

    #[test]
    fn fusion_keeps_the_best_scoring_variant_of_a_record() {
        let fused = reciprocal_rank_fusion(&[vec![hit("mem_a", 2.0)], vec![hit("mem_a", 7.0)]]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].hit.score, 7.0);
        assert_eq!(fused[0].best_rank, 0);
    }

    #[test]
    fn fusion_of_nothing_is_nothing() {
        assert!(reciprocal_rank_fusion(&[]).is_empty());
        assert!(reciprocal_rank_fusion(&[vec![], vec![]]).is_empty());
    }

    #[test]
    fn ordering_is_deterministic_for_equal_scores() {
        let first = reciprocal_rank_fusion(&[vec![hit("mem_b", 1.0), hit("mem_a", 1.0)]]);
        let second = reciprocal_rank_fusion(&[vec![hit("mem_b", 1.0), hit("mem_a", 1.0)]]);
        let ids: Vec<_> = first.iter().map(|c| c.id().to_string()).collect();
        let ids2: Vec<_> = second.iter().map(|c| c.id().to_string()).collect();
        assert_eq!(ids, ids2);
    }
}
