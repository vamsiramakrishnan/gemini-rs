//! The retrieval pipeline: plan in, prepared snapshot out.
//!
//! Search order is L0 prepared-query cache → session overlay → canonical BM25 →
//! optional semantic fallback. Each stage is cheaper than the next, and the
//! expensive one is reached only when the cheap ones genuinely could not serve
//! the query.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::assembler::ContextAssembler;
use super::fusion::{reciprocal_rank_fusion, FusedCandidate};
use super::plan::RetrievalPlan;
use super::snapshot::PreparedMemorySnapshot;
use crate::bm25::{MemoryIndex, Query, SearchHit};
use crate::core::{MemoryError, MemoryId, RetrievalConfig, TurnId};

/// A request to prepare context for a turn.
#[derive(Debug, Clone)]
pub struct RetrievalRequest {
    /// The plan to execute.
    pub plan: RetrievalPlan,
    /// Time to evaluate expiry and recency against.
    pub now: DateTime<Utc>,
}

impl RetrievalRequest {
    /// A request to run `plan` now.
    pub fn new(plan: RetrievalPlan) -> Self {
        Self {
            plan,
            now: Utc::now(),
        }
    }
}

/// A time budget for a retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalBudget {
    /// Milliseconds allowed for lexical search.
    pub lexical_ms: u64,
    /// Milliseconds allowed for semantic fallback; zero disables it.
    pub semantic_ms: u64,
}

impl RetrievalBudget {
    /// The budget for a synchronous fallback on the tool-call path.
    ///
    /// Semantic search is disabled here: this path is already the unhappy one,
    /// and a network round trip would turn a slow answer into a late one.
    pub fn interactive() -> Self {
        let config = RetrievalConfig::default();
        Self {
            lexical_ms: config.immediate_lexical_timeout_ms,
            semantic_ms: 0,
        }
    }

    /// The budget for speculative preparation, where semantic fallback is worth
    /// attempting because nothing is waiting on it.
    pub fn speculative() -> Self {
        let config = RetrievalConfig::default();
        Self {
            lexical_ms: config.immediate_lexical_timeout_ms * 4,
            semantic_ms: config.semantic_fallback_timeout_ms,
        }
    }
}

/// Prepares and serves memory context.
#[async_trait]
pub trait MemoryRetriever: Send + Sync {
    /// Run a plan and produce a snapshot.
    async fn prepare(
        &self,
        request: RetrievalRequest,
    ) -> Result<PreparedMemorySnapshot, MemoryError>;

    /// Answer a query directly, for when speculation missed.
    async fn retrieve_immediate(
        &self,
        query: &str,
        turn_id: TurnId,
        budget: RetrievalBudget,
    ) -> Result<PreparedMemorySnapshot, MemoryError>;
}

/// An optional paraphrase-tolerant backend.
///
/// Reached only when lexical search finds too little — an indirect question, a
/// pronoun-heavy reference, or a fact the user is describing rather than naming.
#[async_trait]
pub trait SemanticFallback: Send + Sync {
    /// Return record ids in descending relevance.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryId>, MemoryError>;
}

/// Shared, swappable index state.
///
/// Held behind an `RwLock` rather than rebuilt per query: retrieval runs while
/// the user is still speaking, so it must never wait on a rebuild.
#[derive(Debug, Default)]
pub struct IndexHandle {
    index: RwLock<MemoryIndex>,
    /// Monotonic across replacements.
    ///
    /// A rebuilt index derives its revision from how many documents were
    /// inserted, so swapping one active fact for another can land on the same
    /// number. Retrieval caches key on this value, so an equal revision after a
    /// correction would serve a stale-but-fresh-looking snapshot.
    generation: std::sync::atomic::AtomicU64,
}

impl IndexHandle {
    /// An empty handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap an existing index.
    pub fn with_index(index: MemoryIndex) -> Self {
        Self {
            index: RwLock::new(index),
            generation: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Replace the index wholesale, e.g. after a corpus recompile.
    pub fn replace(&self, index: MemoryIndex) {
        *self.index.write() = index;
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Read the index.
    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, MemoryIndex> {
        self.index.read()
    }

    /// Mutate the index in place.
    pub fn write(&self) -> parking_lot::RwLockWriteGuard<'_, MemoryIndex> {
        self.index.write()
    }

    /// A revision that advances on every replacement and every mutation.
    pub fn revision(&self) -> u64 {
        // Both terms matter: in-place upserts move the inner revision, whole
        // replacements move the generation.
        self.generation
            .load(std::sync::atomic::Ordering::Acquire)
            .wrapping_mul(1_000_003)
            .wrapping_add(self.index.read().revision())
    }
}

/// The default retriever: local, in-process, no network on the happy path.
pub struct LocalMemoryRetriever {
    canonical: Arc<IndexHandle>,
    overlay: Arc<IndexHandle>,
    assembler: ContextAssembler,
    config: RetrievalConfig,
    semantic: Option<Arc<dyn SemanticFallback>>,
    cache: RwLock<HashMap<String, PreparedMemorySnapshot>>,
    suppressed: RwLock<HashSet<String>>,
}

impl LocalMemoryRetriever {
    /// Build a retriever over canonical and overlay indexes.
    pub fn new(
        canonical: Arc<IndexHandle>,
        overlay: Arc<IndexHandle>,
        config: RetrievalConfig,
    ) -> Self {
        Self {
            canonical,
            overlay,
            assembler: ContextAssembler::new(config.clone()),
            config,
            semantic: None,
            cache: RwLock::new(HashMap::new()),
            suppressed: RwLock::new(HashSet::new()),
        }
    }

    /// Hide canonical records in the given `subject|predicate` windows.
    ///
    /// When the user states something outright mid-conversation, the durable
    /// record it contradicts must stop being retrieved *now* — not after
    /// reconciliation. Otherwise B answers a correction by repeating the thing
    /// it was just corrected about.
    pub fn suppress_windows(&self, windows: HashSet<String>) {
        *self.suppressed.write() = windows;
        self.invalidate_cache();
    }

    /// The windows currently hidden from canonical retrieval.
    pub fn suppressed_windows(&self) -> HashSet<String> {
        self.suppressed.read().clone()
    }

    /// Attach a semantic fallback backend.
    pub fn with_semantic_fallback(mut self, fallback: Arc<dyn SemanticFallback>) -> Self {
        self.semantic = Some(fallback);
        self
    }

    /// Drop cached results — call after any index change.
    pub fn invalidate_cache(&self) {
        self.cache.write().clear();
    }

    /// How many cached snapshots are held.
    pub fn cached_len(&self) -> usize {
        self.cache.read().len()
    }

    fn run_lexical(&self, plan: &RetrievalPlan, now: DateTime<Utc>) -> Vec<Vec<SearchHit>> {
        let entity_forms = plan.entity_forms();
        // A plan's scopes are deliberately *not* applied as a filter.
        //
        // Kinds are assigned by the extraction model and scopes are proposed by
        // the planning model; making retrieval depend on those two agreeing on
        // a taxonomy loses recall for no benefit. A dietary fact the extractor
        // filed as `Identity` is exactly what a plan scoped to `Preference` is
        // looking for. Only an explicit caller scope — the `recall_context`
        // argument — restricts kinds, because that is a stated intent rather
        // than an inference.
        let mut queries: Vec<Query> = plan
            .lexical_queries
            .iter()
            .map(|text| {
                Query::new(text)
                    .with_entities(entity_forms.clone())
                    .with_kinds(plan.kind_filter.clone())
                    .with_limit(20)
            })
            .collect();
        if queries.is_empty() && !entity_forms.is_empty() {
            queries.push(
                Query::new("")
                    .with_entities(entity_forms.clone())
                    .with_kinds(plan.kind_filter.clone())
                    .with_limit(20),
            );
        }

        let canonical = self.canonical.read();
        let overlay = self.overlay.read();
        let mut rankings = Vec::with_capacity(queries.len() * 2);
        for query in &queries {
            // The overlay is searched first and fused as its own ranking, so a
            // fact learned seconds ago competes on rank rather than having to
            // out-score months of accumulated evidence.
            let overlay_hits = overlay.search(query, now);
            if !overlay_hits.is_empty() {
                rankings.push(overlay_hits);
            }
            let hits = canonical.search(query, now);
            if !hits.is_empty() {
                rankings.push(hits);
            }
        }
        rankings
    }

    /// Whether lexical results are thin enough to justify a semantic attempt.
    fn needs_semantic_fallback(&self, candidates: &[FusedCandidate]) -> bool {
        candidates.is_empty()
            || candidates
                .iter()
                .all(|c| c.hit.score < self.config.minimum_candidate_score * 2.0)
    }

    async fn extend_with_semantic(
        &self,
        plan: &RetrievalPlan,
        candidates: &mut Vec<FusedCandidate>,
        budget: RetrievalBudget,
        now: DateTime<Utc>,
    ) {
        let (Some(semantic), true) = (self.semantic.as_ref(), budget.semantic_ms > 0) else {
            return;
        };
        let Some(query) = plan.lexical_queries.first() else {
            return;
        };

        let deadline = std::time::Duration::from_millis(budget.semantic_ms);
        let result = tokio::time::timeout(deadline, semantic.search(query, 10)).await;
        let Ok(Ok(ids)) = result else {
            // A failed or slow fallback is not an error: the lexical results
            // stand, and the caller never learns the difference.
            return;
        };

        let canonical = self.canonical.read();
        for (rank, id) in ids.iter().enumerate() {
            if candidates.iter().any(|c| c.id() == id) {
                continue;
            }
            let Some(doc) = canonical.get(id) else {
                continue;
            };
            if !doc.is_retrievable(now) {
                continue;
            }
            // Ranked below every lexical hit: semantic recall is a safety net,
            // not a competing opinion about relevance.
            candidates.push(FusedCandidate {
                hit: SearchHit {
                    id: id.clone(),
                    score: self.config.minimum_candidate_score * 1.5,
                    statement: doc.statement.clone(),
                    kind: doc.kind,
                    origin: doc.origin,
                    explanation: crate::bm25::SearchExplanation {
                        memory_id: id.clone(),
                        components: Vec::new(),
                        boosts: Vec::new(),
                        lexical_score: 0.0,
                        final_score: self.config.minimum_candidate_score * 1.5,
                    },
                },
                rrf_score: 0.0,
                appearances: 1,
                best_rank: rank + 100,
            });
        }
    }

    /// Answer a query directly, restricted to certain memory kinds.
    ///
    /// The prepared-snapshot shortcut is deliberately bypassed when a scope is
    /// given: the snapshot was built speculatively and knows nothing about the
    /// restriction the model asked for, so serving it would answer a different
    /// question quickly.
    pub async fn retrieve_scoped(
        &self,
        query: &str,
        turn_id: TurnId,
        budget: RetrievalBudget,
        kinds: Vec<crate::core::MemoryKind>,
    ) -> Result<PreparedMemorySnapshot, MemoryError> {
        let now = Utc::now();
        let plan = RetrievalPlan {
            requires_memory: true,
            lexical_queries: vec![query.to_string()],
            kind_filter: kinds,
            ..RetrievalPlan::skip(turn_id, 0, query)
        }
        .normalized();
        Ok(self.execute(&plan, budget, now).await)
    }

    /// Drop canonical candidates the session has superseded in conversation.
    fn drop_suppressed(&self, candidates: &mut Vec<FusedCandidate>) {
        let suppressed = self.suppressed.read();
        if suppressed.is_empty() {
            return;
        }
        let canonical = self.canonical.read();
        candidates.retain(|candidate| {
            if candidate.hit.origin != crate::bm25::MemoryOrigin::Canonical {
                return true;
            }
            match canonical.get(&candidate.hit.id) {
                Some(doc) => {
                    !suppressed.contains(&format!("{}|{}", doc.subject_form, doc.predicate))
                }
                None => true,
            }
        });
    }

    async fn execute(
        &self,
        plan: &RetrievalPlan,
        budget: RetrievalBudget,
        now: DateTime<Utc>,
    ) -> PreparedMemorySnapshot {
        let rankings = self.run_lexical(plan, now);
        let mut candidates = reciprocal_rank_fusion(&rankings);
        self.drop_suppressed(&mut candidates);

        if self.needs_semantic_fallback(&candidates) {
            self.extend_with_semantic(plan, &mut candidates, budget, now)
                .await;
            // Re-applied: the fallback appends canonical ids of its own
            // choosing, and one of them may sit in a window the user corrected
            // seconds ago. Suppressing only before the extension would let the
            // superseded fact back in by the side door.
            self.drop_suppressed(&mut candidates);
        }

        let canonical = self.canonical.read();
        let overlay = self.overlay.read();
        self.assembler.assemble(
            plan,
            &candidates,
            &canonical,
            Some(&overlay),
            (canonical.revision(), overlay.revision()),
            now,
        )
    }
}

#[async_trait]
impl MemoryRetriever for LocalMemoryRetriever {
    async fn prepare(
        &self,
        request: RetrievalRequest,
    ) -> Result<PreparedMemorySnapshot, MemoryError> {
        let plan = &request.plan;
        if !plan.requires_memory {
            return Ok(PreparedMemorySnapshot::empty(plan.turn_id));
        }

        let key = format!(
            "{}|{}|{}",
            plan.cache_key(),
            self.canonical.revision(),
            self.overlay.revision()
        );
        if let Some(cached) = self.cache.read().get(&key) {
            if cached.is_fresh(request.now) {
                let mut snapshot = cached.clone();
                snapshot.source_turn_id = plan.turn_id;
                snapshot.eligible_from_turn = TurnId(plan.turn_id.0 + 1);
                return Ok(snapshot);
            }
        }

        let snapshot = self
            .execute(plan, RetrievalBudget::speculative(), request.now)
            .await;
        self.cache.write().insert(key, snapshot.clone());
        Ok(snapshot)
    }

    async fn retrieve_immediate(
        &self,
        query: &str,
        turn_id: TurnId,
        budget: RetrievalBudget,
    ) -> Result<PreparedMemorySnapshot, MemoryError> {
        self.retrieve_scoped(query, turn_id, budget, Vec::new())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::IndexedMemory;
    use crate::core::{
        CanonicalMemory, CanonicalPredicate, EntityRef, EvidenceCounters, Explicitness, MemoryKind,
        MemorySource, MemoryStatus, MemoryValue, PrivacyMetadata, RetrievalMetadata, SessionId,
        TemporalMetadata, TemporalScope, UserId,
    };
    use crate::retrieval::deterministic::{DeterministicPlanner, KnownEntities};

    fn record(id: &str, kind: MemoryKind, subject: &str, statement: &str) -> CanonicalMemory {
        CanonicalMemory {
            id: MemoryId::new(id),
            owner: UserId::new("usr_1"),
            kind,
            predicate: CanonicalPredicate::new(format!("pred_{id}")),
            status: MemoryStatus::Active,
            confidence: 0.9,
            subject: EntityRef::named(subject),
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
                subject: crate::core::normalize_token(subject),
                tags: vec!["restaurant".into(), "food".into()],
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

    fn retriever() -> (LocalMemoryRetriever, Arc<IndexHandle>, Arc<IndexHandle>) {
        let canonical = Arc::new(IndexHandle::with_index(MemoryIndex::build([
            IndexedMemory::from_canonical(&record(
                "mem_quiet",
                MemoryKind::RelationshipPreference,
                "Rhea",
                "Rhea prefers quiet restaurants.",
            )),
            IndexedMemory::from_canonical(&record(
                "mem_diet",
                MemoryKind::Preference,
                "user",
                "The user is pescatarian.",
            )),
        ])));
        let overlay = Arc::new(IndexHandle::new());
        let retriever = LocalMemoryRetriever::new(
            canonical.clone(),
            overlay.clone(),
            RetrievalConfig::default(),
        );
        (retriever, canonical, overlay)
    }

    fn plan_for(text: &str) -> RetrievalPlan {
        let mut known = KnownEntities::new();
        known.insert("Rhea", "rhea");
        known.insert("my wife", "rhea");
        DeterministicPlanner::with_entities(known).plan(text, TurnId(3), 3, Utc::now())
    }

    #[tokio::test]
    async fn prepares_relevant_context_for_a_recommendation_turn() {
        let (retriever, _, _) = retriever();
        let snapshot = retriever
            .prepare(RetrievalRequest::new(plan_for(
                "where should we eat with my wife tonight",
            )))
            .await
            .unwrap();
        assert!(!snapshot.is_empty());
        assert!(snapshot
            .facts
            .iter()
            .any(|f| f.statement.contains("quiet restaurants")));
    }

    #[tokio::test]
    async fn a_plan_with_nothing_to_search_with_returns_an_empty_snapshot() {
        let (retriever, _, _) = retriever();
        let snapshot = retriever
            .prepare(RetrievalRequest::new(plan_for("what do you think")))
            .await
            .unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(retriever.cached_len(), 0, "a skip plan is not cached");
    }

    #[tokio::test]
    async fn a_world_knowledge_question_searches_and_finds_nothing() {
        // The other half of the same contract, and the reason the planner does
        // not need to recognise "what is the capital of France" as general
        // knowledge: the query runs against a personal corpus that contains no
        // matching term, so it scores nothing. Identical observable outcome to
        // a skip, without needing to understand the sentence to get there.
        let (retriever, _, _) = retriever();
        let snapshot = retriever
            .prepare(RetrievalRequest::new(plan_for(
                "what is the capital of France",
            )))
            .await
            .unwrap();
        assert!(
            snapshot.is_empty(),
            "a question the corpus knows nothing about produced context: {:?}",
            snapshot.facts
        );
    }

    #[tokio::test]
    async fn identical_plans_hit_the_cache_but_an_index_change_invalidates_it() {
        let (retriever, canonical, _) = retriever();
        let plan = plan_for("what does my wife like about restaurants");
        retriever
            .prepare(RetrievalRequest::new(plan.clone()))
            .await
            .unwrap();
        assert_eq!(retriever.cached_len(), 1);

        retriever
            .prepare(RetrievalRequest::new(plan.clone()))
            .await
            .unwrap();
        assert_eq!(retriever.cached_len(), 1, "same plan reuses the entry");

        // A new revision keys differently, so stale context cannot be served.
        canonical
            .write()
            .upsert(IndexedMemory::from_canonical(&record(
                "mem_new",
                MemoryKind::Preference,
                "user",
                "The user likes rooftop restaurants.",
            )));
        retriever
            .prepare(RetrievalRequest::new(plan))
            .await
            .unwrap();
        assert_eq!(retriever.cached_len(), 2);
    }

    #[tokio::test]
    async fn overlay_facts_are_retrievable_immediately() {
        let (retriever, _, overlay) = retriever();
        overlay.write().upsert(
            IndexedMemory::from_canonical(&record(
                "mem_session",
                MemoryKind::Episodic,
                "user",
                "The user is meeting Kushal for dinner tonight.",
            ))
            .as_session_overlay(),
        );

        let snapshot = retriever
            .retrieve_immediate("dinner tonight", TurnId(4), RetrievalBudget::interactive())
            .await
            .unwrap();
        assert!(snapshot
            .facts
            .iter()
            .any(|f| f.memory_id.as_str() == "mem_session"));
    }

    #[tokio::test]
    async fn an_immediate_query_with_no_match_degrades_to_not_found() {
        let (retriever, _, _) = retriever();
        let snapshot = retriever
            .retrieve_immediate(
                "what medication is prescribed",
                TurnId(4),
                RetrievalBudget::interactive(),
            )
            .await
            .unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.to_tool_payload()["status"], "not_found");
    }

    struct StubSemantic {
        ids: Vec<MemoryId>,
    }

    #[async_trait]
    impl SemanticFallback for StubSemantic {
        async fn search(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryId>, MemoryError> {
            Ok(self.ids.clone())
        }
    }

    struct FailingSemantic;

    #[async_trait]
    impl SemanticFallback for FailingSemantic {
        async fn search(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryId>, MemoryError> {
            Err(MemoryError::Retrieval("backend down".into()))
        }
    }

    #[tokio::test]
    async fn semantic_fallback_rescues_a_query_lexical_search_misses() {
        let (retriever, canonical, overlay) = retriever();
        drop(retriever);
        let retriever = LocalMemoryRetriever::new(canonical, overlay, RetrievalConfig::default())
            .with_semantic_fallback(Arc::new(StubSemantic {
                ids: vec![MemoryId::new("mem_diet")],
            }));

        // No lexical overlap with any record, so only the fallback can find it.
        let snapshot = retriever
            .retrieve_immediate(
                "seafood eating habits",
                TurnId(4),
                RetrievalBudget::speculative(),
            )
            .await
            .unwrap();
        assert!(snapshot
            .facts
            .iter()
            .any(|f| f.memory_id.as_str() == "mem_diet"));
    }

    #[tokio::test]
    async fn a_failing_semantic_backend_degrades_to_lexical_results() {
        let (retriever, canonical, overlay) = retriever();
        drop(retriever);
        let retriever = LocalMemoryRetriever::new(canonical, overlay, RetrievalConfig::default())
            .with_semantic_fallback(Arc::new(FailingSemantic));

        let snapshot = retriever
            .retrieve_immediate(
                "quiet restaurants",
                TurnId(4),
                RetrievalBudget::speculative(),
            )
            .await
            .unwrap();
        assert!(!snapshot.is_empty(), "lexical results must still be served");
    }

    #[tokio::test]
    async fn the_interactive_budget_never_reaches_for_the_network() {
        assert_eq!(RetrievalBudget::interactive().semantic_ms, 0);
        assert!(RetrievalBudget::speculative().semantic_ms > 0);
    }
}
