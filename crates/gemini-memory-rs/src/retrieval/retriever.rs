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
use super::fusion::{FusedCandidate, reciprocal_rank_fusion};
use super::plan::RetrievalPlan;
use super::snapshot::PreparedMemorySnapshot;
use crate::bm25::{MemoryIndex, Query, SearchHit};
use crate::core::{MemoryError, MemoryId, RetrievalConfig, TurnId};
use crate::retrieval::deterministic::topical_terms;

/// The words of a query that carry no topic: what is left after the topical
/// terms are taken out.
///
/// These are the words a memory lookup is *always* phrased with — "what", "the
/// user's", "my" — plus the corpus's own subject form, which has a posting in
/// almost every record. They are excellent at saying whose memory to prefer and
/// useless at saying which memory is relevant, so the index is told to let them
/// rank a record but never admit one. See [`Query::boost_only`].
pub(crate) fn non_topical_terms(query: &str) -> Vec<String> {
    let topical: HashSet<String> = topical_terms(query).into_iter().collect();
    crate::bm25::tokenize(query)
        .into_iter()
        .filter(|term| !topical.contains(term))
        .collect()
}

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
    /// Semantic search gets a short deadline rather than none. It used to get
    /// none, on the reasoning that a network round trip would turn a slow
    /// answer into a late one — true of a remote backend, and the reason the
    /// deadline is short. But it also meant a *local* backend was never asked:
    /// measured against a perfect semantic oracle, this path answered exactly
    /// as many questions as BM25 alone, because the oracle was never once
    /// consulted. A deadline keeps the latency guarantee and stops throwing
    /// away the one path that could answer.
    pub fn interactive() -> Self {
        Self::interactive_with(&RetrievalConfig::default())
    }

    /// The interactive budget under a specific configuration.
    pub fn interactive_with(config: &RetrievalConfig) -> Self {
        Self {
            lexical_ms: config.immediate_lexical_timeout_ms,
            semantic_ms: config.immediate_semantic_timeout_ms,
        }
    }

    /// The budget for speculative preparation, where semantic fallback is worth
    /// attempting because nothing is waiting on it.
    pub fn speculative() -> Self {
        Self::speculative_with(&RetrievalConfig::default())
    }

    /// The speculative budget under a specific configuration.
    pub fn speculative_with(config: &RetrievalConfig) -> Self {
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

    /// Bring the backend in line with the active corpus.
    ///
    /// Called after the canonical index is recompiled — that is, after a
    /// session seals and reconciliation has decided what is true. `active` is
    /// the **entire desired state**: every active record paired with the text
    /// it should be findable by, from
    /// [`embedding_text`](crate::retrieval::embedding_text).
    ///
    /// Passing the whole set rather than a diff is deliberate. A diff has to be
    /// right, and the cost of getting it wrong is a vector store that quietly
    /// disagrees with the corpus — which is the failure this method exists to
    /// prevent, reintroduced by the fix for it. A backend given the full set
    /// can reconcile idempotently and embed only what it does not already hold.
    ///
    /// Default is a no-op, so an existing implementation keeps compiling and
    /// keeps its current behaviour: an index that never learns about
    /// corrections. That is the *safe* failure — `semantic_ranking` resolves
    /// every returned id against the canonical index and drops what is no
    /// longer retrievable, so a stale backend loses facts rather than serving
    /// wrong ones — but it is still a failure, and it lands hardest on
    /// corrected facts, which are the ones a user has shown they care about.
    ///
    /// # `revision` orders concurrent callers
    ///
    /// One engine hands the same backend to every session it opens, so two
    /// sessions sealing at the same moment both call this — each with a corpus
    /// snapshot taken before it started. Because `active` is a whole desired
    /// state rather than a diff, the call that *finishes* last wins, and if
    /// that is the one that *started* first it silently removes every record
    /// the other added, vectors and all.
    ///
    /// Serialising the calls does not fix that: the loser's snapshot is stale
    /// whenever it is applied, not only when it interleaves. So callers pass
    /// the canonical index revision the snapshot was taken from — monotonic per
    /// engine — and an implementation must ignore a revision it has already
    /// passed. Pass `0` if there is nothing meaningful to order by; that
    /// disables the check rather than breaking it.
    async fn reconcile(
        &self,
        _active: &[(MemoryId, String)],
        _revision: u64,
    ) -> Result<(), MemoryError> {
        Ok(())
    }
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

    /// The semantic backend, if one was installed.
    ///
    /// Exposed so the engine can call [`SemanticFallback::reconcile`] after a
    /// commit — the vector store has to be told what reconciliation decided, or
    /// it keeps answering from the corpus as it was before the correction.
    pub fn semantic(&self) -> Option<&Arc<dyn SemanticFallback>> {
        self.semantic.as_ref()
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
                    .with_boost_only(non_topical_terms(text))
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

    /// Ask the semantic backend, as a ranking that competes on merit.
    ///
    /// Two decisions here were reversed by measurement, and both were reversed
    /// for the same reason: the semantic side is the *stronger* ranker, not a
    /// weaker one worth consulting in emergencies.
    ///
    /// **It is no longer gated.** The gate asked the backend only when lexical
    /// search had found little, which fired on 13 of 93 paraphrased questions.
    /// All 13 were rescued — every single call was worth making — while the
    /// other 80 were declined because BM25 had returned something confident.
    /// Confident and wrong is the failure this was supposed to catch.
    ///
    /// **It is no longer appended below the lexical hits.** Embedding a
    /// record's own frontmatter answers 66 of those 93 questions against BM25's
    /// 42; ranking that beneath every lexical hit discards the better opinion
    /// by construction. The ranking is fused instead, weighted 2:1 — the
    /// configuration `semantic_fusion_probe` measured at 79/93 in the top five,
    /// against 73 for semantics alone and 58 for BM25 alone.
    ///
    /// Returns the semantic ranking, empty if there is no backend, no budget,
    /// no query, or the deadline passes.
    async fn semantic_ranking(
        &self,
        plan: &RetrievalPlan,
        budget: RetrievalBudget,
        now: DateTime<Utc>,
    ) -> Vec<SearchHit> {
        let (Some(semantic), true) = (self.semantic.as_ref(), budget.semantic_ms > 0) else {
            return Vec::new();
        };
        let Some(query) = plan.lexical_queries.first() else {
            return Vec::new();
        };

        let deadline = std::time::Duration::from_millis(budget.semantic_ms);
        let result = tokio::time::timeout(deadline, semantic.search(query, 10)).await;
        let Ok(Ok(ids)) = result else {
            // A failed or slow fallback is not an error: the lexical results
            // stand, and the caller never learns the difference.
            return Vec::new();
        };

        let canonical = self.canonical.read();
        ids.iter()
            .filter_map(|id| {
                let doc = canonical.get(id)?;
                if !doc.is_retrievable(now) {
                    return None;
                }
                // The score is nominal: RRF ranks by position, and a dense
                // similarity is not comparable with a BM25 score anyway.
                let score = self.config.minimum_candidate_score * 2.0;
                Some(SearchHit {
                    id: id.clone(),
                    score,
                    statement: doc.statement.clone(),
                    kind: doc.kind,
                    origin: doc.origin,
                    explanation: crate::bm25::SearchExplanation {
                        memory_id: id.clone(),
                        components: Vec::new(),
                        boosts: Vec::new(),
                        lexical_score: 0.0,
                        final_score: score,
                    },
                })
            })
            .collect()
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
        about: Option<String>,
        attribute: Option<String>,
    ) -> Result<PreparedMemorySnapshot, MemoryError> {
        let now = Utc::now();
        let blank = |value: Option<String>| value.filter(|v| !v.trim().is_empty());
        let plan = RetrievalPlan {
            requires_memory: true,
            lexical_queries: vec![query.to_string()],
            kind_filter: kinds,
            subject_hint: blank(about),
            predicate_hint: blank(attribute),
            ..RetrievalPlan::skip(turn_id, 0, query)
        }
        .normalized();
        Ok(self.execute(&plan, budget, now).await)
    }

    /// Boost records matching the caller's `about`/`attribute` hints.
    ///
    /// For each ranking already collected, this appends a copy containing only
    /// the records that match. A matching record therefore earns its
    /// `1/(60 + rank)` twice over and roughly doubles its fused weight, while a
    /// record that does not match keeps everything it had. Nothing is removed.
    ///
    /// That "nothing is removed" is the entire design, and it is the third
    /// place in this crate where the same lesson was learned by measurement.
    /// `needs_semantic_fallback` gated the semantic backend and cost 80% of its
    /// value; `satisfies` gated the prepared snapshot and discarded 65 of 93
    /// correct ones; and a *hard* version of this filter answers 0 of 93 when
    /// the hint is wrong, because the answer never enters the candidate set.
    /// The soft version costs one question in the same case.
    ///
    /// A hint matching nothing produces empty rankings, which contribute
    /// nothing and leave the unfiltered result exactly as it was. That is the
    /// degradation path, and it is why a model guessing badly is survivable.
    fn apply_hints(&self, plan: &RetrievalPlan, rankings: &mut Vec<Vec<SearchHit>>) {
        let subject = plan
            .subject_hint
            .as_deref()
            .map(crate::core::normalize_token)
            .filter(|s| !s.is_empty());
        let predicate = plan
            .predicate_hint
            .as_deref()
            .map(crate::core::normalize_token)
            .filter(|s| !s.is_empty());
        if subject.is_none() && predicate.is_none() {
            return;
        }

        let canonical = self.canonical.read();
        let matches = |hit: &SearchHit| -> bool {
            let Some(doc) = canonical.get(&hit.id) else {
                // Overlay hits are not in the canonical index. A fact learned
                // seconds ago is the one thing most likely to be what the model
                // is asking about, so an unresolvable id is boosted rather than
                // dropped — the failure mode of the alternative is silently
                // demoting the freshest record in memory.
                return true;
            };
            if let Some(subject) = &subject
                && &doc.subject_form != subject
            {
                return false;
            }
            if let Some(predicate) = &predicate
                && &crate::core::normalize_token(doc.predicate.as_str()) != predicate
            {
                return false;
            }
            true
        };

        let boosted: Vec<Vec<SearchHit>> = rankings
            .iter()
            .map(|ranking| ranking.iter().filter(|h| matches(h)).cloned().collect())
            .filter(|ranking: &Vec<SearchHit>| !ranking.is_empty())
            .collect();
        rankings.extend(boosted);
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
        let mut rankings = self.run_lexical(plan, now);
        let semantic = self.semantic_ranking(plan, budget, now).await;
        if !semantic.is_empty() {
            // Twice, which is how you weight a ranking in RRF: each appearance
            // contributes its own 1/(60 + rank). Measured at 79/93 in the top
            // five against 73 for semantics alone and 58 for lexical alone.
            rankings.push(semantic.clone());
            rankings.push(semantic);
        }
        self.apply_hints(plan, &mut rankings);
        let mut candidates = reciprocal_rank_fusion(&rankings);
        // Suppression runs after the fusion rather than before it: the semantic
        // backend returns ids of its own choosing, and one of them may sit in a
        // window the user corrected seconds ago. Filtering only the lexical
        // side would let the superseded fact back in by the side door.
        self.drop_suppressed(&mut candidates);

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
        if let Some(cached) = self.cache.read().get(&key)
            && cached.is_fresh(request.now)
        {
            let mut snapshot = cached.clone();
            snapshot.source_turn_id = plan.turn_id;
            snapshot.eligible_from_turn = TurnId(plan.turn_id.0 + 1);
            return Ok(snapshot);
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
        self.retrieve_scoped(query, turn_id, budget, Vec::new(), None, None)
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
        assert!(
            snapshot
                .facts
                .iter()
                .any(|f| f.statement.contains("quiet restaurants"))
        );
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
        assert!(
            snapshot
                .facts
                .iter()
                .any(|f| f.memory_id.as_str() == "mem_session")
        );
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
        assert!(
            snapshot
                .facts
                .iter()
                .any(|f| f.memory_id.as_str() == "mem_diet")
        );
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
    async fn the_interactive_budget_bounds_the_semantic_call_rather_than_forbidding_it() {
        let interactive = RetrievalBudget::interactive();
        let speculative = RetrievalBudget::speculative();

        // It used to be zero. Zero meant a *local* backend — an in-process
        // vector scan costing well under a millisecond — was never asked, and
        // measured against a perfect oracle this path answered exactly as many
        // questions as BM25 alone.
        assert!(
            interactive.semantic_ms > 0,
            "the tool path must at least ask; a backend that cannot answer in \
             time will time out on its own"
        );
        // But it stays a fraction of the speculative budget: nothing is waiting
        // on speculation, and the model is waiting on this.
        assert!(
            interactive.semantic_ms * 4 <= speculative.semantic_ms,
            "the interactive deadline ({}ms) has drifted close to the \
             speculative one ({}ms) — the point of the split is that only one \
             of them has somebody waiting",
            interactive.semantic_ms,
            speculative.semantic_ms,
        );
    }

    #[tokio::test]
    async fn a_zero_interactive_deadline_restores_the_old_behaviour() {
        let config = RetrievalConfig {
            immediate_semantic_timeout_ms: 0,
            ..RetrievalConfig::default()
        };
        assert_eq!(RetrievalBudget::interactive_with(&config).semantic_ms, 0);
    }

    /// The gate is gone, and this is what replaces the argument for it.
    ///
    /// It used to consult the backend only when lexical search came back thin,
    /// which fired on 13 of 93 paraphrased questions — and rescued all 13. The
    /// 80 it declined were declined because BM25 had returned something
    /// confident, which is the failure mode, not the safe case.
    /// The property the whole design rests on: a wrong hint must not be able to
    /// remove the answer.
    ///
    /// Measured, a *hard* version of this filter answers 0 of 93 questions when
    /// the hint is wrong — the answer is absent from the candidate set every
    /// single time — while the soft version costs one. If this test ever fails,
    /// the filter has become a gate and the feature is worse than not having it.
    #[tokio::test]
    async fn a_wrong_hint_reorders_the_results_without_removing_the_answer() {
        let (retriever, canonical, _overlay) = retriever();
        canonical
            .write()
            .upsert(IndexedMemory::from_canonical(&record(
                "mem_coffee",
                MemoryKind::Preference,
                "user",
                "The user drinks cortados.",
            )));

        let unhinted = retriever
            .retrieve_scoped(
                "cortados",
                TurnId(1),
                RetrievalBudget::interactive(),
                Vec::new(),
                None,
                None,
            )
            .await
            .expect("retrieval");
        assert!(
            !unhinted.facts.is_empty(),
            "the baseline must find the record for this test to mean anything"
        );

        let misdirected = retriever
            .retrieve_scoped(
                "cortados",
                TurnId(1),
                RetrievalBudget::interactive(),
                Vec::new(),
                Some("somebody-else-entirely".into()),
                Some("an-attribute-that-does-not-exist".into()),
            )
            .await
            .expect("retrieval");
        assert_eq!(
            misdirected.facts.len(),
            unhinted.facts.len(),
            "a hint matching nothing must leave the unfiltered result standing, \
             not empty it"
        );
    }

    /// And a right hint has to actually do something, or the field is theatre.
    #[tokio::test]
    async fn a_matching_hint_promotes_its_record_above_a_rival() {
        let (retriever, canonical, _overlay) = retriever();
        // Two records the query matches equally well, distinguished only by
        // whose fact it is.
        canonical
            .write()
            .upsert(IndexedMemory::from_canonical(&record(
                "mem_rival",
                MemoryKind::Preference,
                "rhea",
                "Rhea drinks cortados.",
            )));
        canonical
            .write()
            .upsert(IndexedMemory::from_canonical(&record(
                "mem_target",
                MemoryKind::Preference,
                "user",
                "The user drinks cortados.",
            )));

        let hinted = retriever
            .retrieve_scoped(
                "cortados",
                TurnId(1),
                RetrievalBudget::interactive(),
                Vec::new(),
                Some("user".into()),
                None,
            )
            .await
            .expect("retrieval");
        let first = hinted.facts.first().expect("a result");
        assert_eq!(
            first.memory_id.as_str(),
            "mem_target",
            "the record whose subject matches the hint should rank first; got {:?}",
            hinted
                .facts
                .iter()
                .map(|m| m.memory_id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn the_semantic_backend_is_consulted_even_when_lexical_search_is_confident() {
        let (retriever, canonical, overlay) = retriever();
        drop(retriever);
        let retriever = LocalMemoryRetriever::new(canonical, overlay, RetrievalConfig::default())
            .with_semantic_fallback(Arc::new(StubSemantic {
                ids: vec![MemoryId::new("mem_quiet")],
            }));

        // "pescatarian" is a confident lexical hit on mem_diet — precisely the
        // case the old gate declined to escalate.
        let snapshot = retriever
            .retrieve_immediate("pescatarian", TurnId(4), RetrievalBudget::speculative())
            .await
            .unwrap();
        assert!(
            snapshot
                .facts
                .iter()
                .any(|f| f.memory_id.as_str() == "mem_quiet"),
            "a confident lexical hit suppressed the semantic ranking entirely; \
             got {:?}",
            snapshot
                .facts
                .iter()
                .map(|f| f.memory_id.as_str())
                .collect::<Vec<_>>()
        );
    }
}
