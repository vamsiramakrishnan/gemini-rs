//! Context assembly — turning a ranked candidate list into a bounded snapshot.
//!
//! The assembler receives ten to twenty candidates and emits three to five
//! facts. Everything it does is subtraction: drop weak matches, drop
//! near-duplicates of the same predicate, and stop at the token budget. A
//! memory block that grows without bound is a latency and a distraction
//! problem, not a helpfulness one.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::Arc;

use super::fusion::FusedCandidate;
use super::plan::{RetrievalIntent, RetrievalPlan};
use super::snapshot::{PreparedMemorySnapshot, RetrievedMemory, SNAPSHOT_TTL_SECONDS};
use crate::bm25::{MemoryIndex, MemoryOrigin};
use crate::core::{RetrievalConfig, SnapshotId, TurnId};

/// Assembles snapshots under a budget.
#[derive(Debug, Clone)]
pub struct ContextAssembler {
    config: RetrievalConfig,
}

impl ContextAssembler {
    /// An assembler using the given budgets.
    pub fn new(config: RetrievalConfig) -> Self {
        Self { config }
    }

    /// The budgets in force.
    pub fn config(&self) -> &RetrievalConfig {
        &self.config
    }

    /// Build a snapshot from fused candidates.
    ///
    /// `index` supplies the predicate and temporal metadata that ranking does
    /// not carry; candidates missing from it are dropped rather than guessed at.
    pub fn assemble(
        &self,
        plan: &RetrievalPlan,
        candidates: &[FusedCandidate],
        index: &MemoryIndex,
        overlay: Option<&MemoryIndex>,
        revisions: (u64, u64),
        now: DateTime<Utc>,
    ) -> PreparedMemorySnapshot {
        let mut facts: Vec<RetrievedMemory> = Vec::new();
        let mut tokens = 0usize;
        let mut per_predicate: HashMap<String, usize> = HashMap::new();

        // An explicit recall request is the user asking to hear what is stored,
        // so the diversity cap that stops ordinary turns being flooded with
        // near-identical preferences is relaxed.
        let per_predicate_cap = if plan.intent == RetrievalIntent::ExplicitRecall {
            self.config.max_memories
        } else {
            self.config.max_per_predicate
        };

        for candidate in candidates {
            if facts.len() >= self.config.max_memories {
                break;
            }
            if candidate.hit.score < self.config.minimum_candidate_score {
                continue;
            }
            let Some(doc) = overlay
                .and_then(|o| o.get(&candidate.hit.id))
                .or_else(|| index.get(&candidate.hit.id))
            else {
                continue;
            };

            let predicate = doc.predicate.to_string();
            let used = per_predicate.entry(predicate).or_insert(0);
            if *used >= per_predicate_cap {
                continue;
            }

            let fact = RetrievedMemory {
                memory_id: doc.id.clone(),
                statement: doc.statement.clone(),
                kind: doc.kind,
                temporal_scope: doc.temporal_scope,
                origin: doc.origin,
                score: candidate.hit.score,
            };
            let cost = fact.token_cost();
            if tokens + cost > self.config.max_tokens {
                // The budget is a hard cap: stop rather than truncate a
                // statement into something that reads as a different fact.
                break;
            }
            *used += 1;
            tokens += cost;
            facts.push(fact);

            if tokens >= self.config.target_tokens {
                break;
            }
        }

        // Committed facts first: the model should see settled memory before
        // anything still provisional.
        facts.sort_by_key(|f| match f.origin {
            MemoryOrigin::Canonical => 0,
            MemoryOrigin::SessionOverlay => 1,
        });

        PreparedMemorySnapshot {
            snapshot_id: SnapshotId::generate(),
            user_memory_revision: revisions.0,
            session_overlay_revision: revisions.1,
            retrieval_plan_id: Some(plan.plan_id.clone()),
            source_turn_id: plan.turn_id,
            eligible_from_turn: TurnId(plan.turn_id.0 + 1),
            token_count: tokens.min(u16::MAX as usize) as u16,
            facts: Arc::from(facts),
            cache_key: plan.cache_key(),
            created_at: now,
            expires_at: now + Duration::seconds(SNAPSHOT_TTL_SECONDS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::{IndexedMemory, SearchExplanation, SearchHit};
    use crate::core::{
        CanonicalMemory, CanonicalPredicate, EntityRef, EvidenceCounters, Explicitness, MemoryId,
        MemoryKind, MemorySource, MemoryStatus, MemoryValue, PlanId, PrivacyMetadata,
        RetrievalMetadata, SessionId, TemporalMetadata, TemporalScope, UserId,
    };

    fn record(id: &str, predicate: &str, statement: &str) -> CanonicalMemory {
        CanonicalMemory {
            id: MemoryId::new(id),
            owner: UserId::new("usr_1"),
            kind: MemoryKind::Preference,
            predicate: CanonicalPredicate::new(predicate),
            status: MemoryStatus::Active,
            confidence: 0.9,
            subject: EntityRef::user(),
            value: MemoryValue::Text(statement.into()),
            statement: statement.into(),
            evidence_summary: "stated".into(),
            source: MemorySource::from_explicitness(
                Explicitness::ExplicitStatement,
                SessionId::new("ses_1"),
                TurnId(1),
            ),
            temporal: TemporalMetadata::created_at(Utc::now()),
            retrieval: RetrievalMetadata {
                subject: "user".into(),
                ..Default::default()
            },
            evidence: EvidenceCounters::first(),
            privacy: PrivacyMetadata::default(),
            temporal_scope: TemporalScope::Persistent,
            supersedes: Vec::new(),
            superseded_by: None,
            qualifier: None,
        }
    }

    fn candidate(id: &str, score: f32) -> FusedCandidate {
        FusedCandidate {
            hit: SearchHit {
                id: MemoryId::new(id),
                score,
                statement: String::new(),
                kind: MemoryKind::Preference,
                origin: MemoryOrigin::Canonical,
                explanation: SearchExplanation {
                    memory_id: MemoryId::new(id),
                    components: Vec::new(),
                    boosts: Vec::new(),
                    lexical_score: score,
                    final_score: score,
                },
            },
            rrf_score: score,
            appearances: 1,
            best_rank: 0,
        }
    }

    fn plan() -> RetrievalPlan {
        RetrievalPlan {
            plan_id: PlanId::new("pln_1"),
            turn_id: TurnId(4),
            generation: 4,
            requires_memory: true,
            confidence: 0.9,
            intent: RetrievalIntent::PersonalRecommendation,
            entities: Vec::new(),
            topics: vec!["food".into()],
            predicates: Vec::new(),
            lexical_queries: vec!["food".into()],
            scopes: Vec::new(),
            kind_filter: Vec::new(),
            subject_hint: None,
            predicate_hint: None,
            temporal: None,
            source_transcript_hash: "h".into(),
        }
    }

    #[test]
    fn caps_the_number_of_memories_returned() {
        let records: Vec<_> = (0..10)
            .map(|i| record(&format!("mem_{i}"), &format!("pred_{i}"), "A preference."))
            .collect();
        let index = MemoryIndex::build(records.iter().map(IndexedMemory::from_canonical));
        let candidates: Vec<_> = (0..10)
            .map(|i| candidate(&format!("mem_{i}"), 5.0))
            .collect();

        let snapshot = ContextAssembler::new(RetrievalConfig::default()).assemble(
            &plan(),
            &candidates,
            &index,
            None,
            (1, 0),
            Utc::now(),
        );
        assert!(snapshot.facts.len() <= RetrievalConfig::default().max_memories);
    }

    #[test]
    fn never_exceeds_the_hard_token_cap() {
        let long = "The user has an extremely detailed preference ".repeat(20);
        let records: Vec<_> = (0..10)
            .map(|i| record(&format!("mem_{i}"), &format!("pred_{i}"), &long))
            .collect();
        let index = MemoryIndex::build(records.iter().map(IndexedMemory::from_canonical));
        let candidates: Vec<_> = (0..10)
            .map(|i| candidate(&format!("mem_{i}"), 5.0))
            .collect();

        let config = RetrievalConfig::default();
        let snapshot = ContextAssembler::new(config.clone()).assemble(
            &plan(),
            &candidates,
            &index,
            None,
            (1, 0),
            Utc::now(),
        );
        assert!(
            usize::from(snapshot.token_count) <= config.max_tokens,
            "token count {} exceeded cap",
            snapshot.token_count
        );
    }

    #[test]
    fn limits_near_duplicates_of_the_same_predicate() {
        let records: Vec<_> = (0..5)
            .map(|i| {
                record(
                    &format!("mem_{i}"),
                    "coffee_order",
                    "The user drinks flat whites.",
                )
            })
            .collect();
        let index = MemoryIndex::build(records.iter().map(IndexedMemory::from_canonical));
        let candidates: Vec<_> = (0..5)
            .map(|i| candidate(&format!("mem_{i}"), 5.0))
            .collect();

        let snapshot = ContextAssembler::new(RetrievalConfig::default()).assemble(
            &plan(),
            &candidates,
            &index,
            None,
            (1, 0),
            Utc::now(),
        );
        assert_eq!(
            snapshot.facts.len(),
            RetrievalConfig::default().max_per_predicate
        );
    }

    #[test]
    fn an_explicit_recall_request_relaxes_the_diversity_cap() {
        let records: Vec<_> = (0..5)
            .map(|i| {
                record(
                    &format!("mem_{i}"),
                    "coffee_order",
                    "The user drinks flat whites.",
                )
            })
            .collect();
        let index = MemoryIndex::build(records.iter().map(IndexedMemory::from_canonical));
        let candidates: Vec<_> = (0..5)
            .map(|i| candidate(&format!("mem_{i}"), 5.0))
            .collect();

        let mut plan = plan();
        plan.intent = RetrievalIntent::ExplicitRecall;
        let snapshot = ContextAssembler::new(RetrievalConfig::default()).assemble(
            &plan,
            &candidates,
            &index,
            None,
            (1, 0),
            Utc::now(),
        );
        assert!(snapshot.facts.len() > RetrievalConfig::default().max_per_predicate);
    }

    #[test]
    fn weak_candidates_are_dropped() {
        let index = MemoryIndex::build([IndexedMemory::from_canonical(&record(
            "mem_weak",
            "pred",
            "Barely relevant.",
        ))]);
        let snapshot = ContextAssembler::new(RetrievalConfig::default()).assemble(
            &plan(),
            &[candidate("mem_weak", 0.01)],
            &index,
            None,
            (1, 0),
            Utc::now(),
        );
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.to_tool_payload()["status"], "not_found");
    }

    #[test]
    fn committed_facts_are_presented_before_provisional_ones() {
        let canonical = IndexedMemory::from_canonical(&record("mem_c", "p1", "Committed fact."));
        let overlay_doc =
            IndexedMemory::from_canonical(&record("mem_o", "p2", "Provisional fact."))
                .as_session_overlay();
        let index = MemoryIndex::build([canonical]);
        let overlay = MemoryIndex::build([overlay_doc]);

        let snapshot = ContextAssembler::new(RetrievalConfig::default()).assemble(
            &plan(),
            // Overlay ranks higher, but presentation order still puts the
            // settled fact first.
            &[candidate("mem_o", 9.0), candidate("mem_c", 5.0)],
            &index,
            Some(&overlay),
            (1, 3),
            Utc::now(),
        );
        assert_eq!(snapshot.facts[0].memory_id.as_str(), "mem_c");
        assert_eq!(snapshot.facts[1].origin, MemoryOrigin::SessionOverlay);
    }

    #[test]
    fn the_snapshot_records_what_it_was_built_from() {
        let index = MemoryIndex::build([IndexedMemory::from_canonical(&record(
            "mem_a", "p", "A fact.",
        ))]);
        let snapshot = ContextAssembler::new(RetrievalConfig::default()).assemble(
            &plan(),
            &[candidate("mem_a", 5.0)],
            &index,
            None,
            (7, 3),
            Utc::now(),
        );
        assert_eq!(snapshot.user_memory_revision, 7);
        assert_eq!(snapshot.session_overlay_revision, 3);
        assert_eq!(snapshot.source_turn_id, TurnId(4));
        assert_eq!(snapshot.eligible_from_turn, TurnId(5));
        assert_eq!(
            snapshot.retrieval_plan_id.as_ref().unwrap().as_str(),
            "pln_1"
        );
    }
}
