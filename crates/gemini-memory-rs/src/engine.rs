//! The memory engine facade.
//!
//! [`MemoryEngine`] owns everything that outlives a conversation — the
//! repository, the compiled index, the event log, the extractor seams — and
//! [`MemorySession`] owns everything that does not: the candidate ledger, the
//! overlay, the turn counter, and the prepared snapshot the current turn is
//! being answered from.
//!
//! The split is the architecture in miniature. A session may end abruptly at
//! any moment; nothing it holds is authoritative until reconciliation writes it
//! through the engine.

use chrono::Utc;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;

use crate::bm25::{IndexedMemory, MemoryIndex};
use crate::core::{
    MemoryError, MemoryEvent, MemoryEventLog, MemoryRuntimeConfig, MemoryStatus, MutationIntent,
    SessionEventWriter, SessionId, TurnId, UserId,
};
use crate::ingestion::{
    BoundedObservationExtractor, CadenceTracker, InMemorySessionLedger, LedgerOutcome,
    MemoryObservationExtractor, ObservationExtractionContext, RuleBasedObservationExtractor,
    ScheduledWork, SessionLedger, SessionMemoryOverlay,
};
use crate::okf::{MemoryRepository, OkfRepository};
use crate::reconcile::{consolidate, MemoryCommitter, ReconciliationReport};
use crate::retrieval::{
    context_for, DeterministicPlanExtractor, DeterministicPlanner, IndexHandle, KnownEntities,
    LocalMemoryRetriever, MemoryRetriever, PreparedMemorySnapshot, RetrievalBudget,
    RetrievalPlanExtractor, RetrievalRequest,
};
use crate::transcript::GenerationGuard;

/// Everything that outlives a conversation.
pub struct MemoryEngine {
    user: UserId,
    config: MemoryRuntimeConfig,
    repository: Arc<dyn MemoryRepository>,
    canonical: Arc<IndexHandle>,
    planner: Arc<RwLock<Arc<DeterministicPlanner>>>,
    events: Arc<dyn MemoryEventLog>,
    plan_extractor: Arc<RwLock<Arc<dyn RetrievalPlanExtractor>>>,
    observation_extractor: Arc<dyn MemoryObservationExtractor>,
}

impl MemoryEngine {
    /// An engine backed entirely by in-process storage.
    ///
    /// Suitable for tests, single-node deployments and local development; swap
    /// the repository and event log for durable ones in production.
    pub fn in_memory(user: UserId) -> Self {
        Self::new(
            user,
            Arc::new(OkfRepository::in_memory()),
            Arc::new(crate::core::InMemoryEventLog::new()),
            MemoryRuntimeConfig::default(),
        )
    }

    /// An engine over the given repository and event log.
    pub fn new(
        user: UserId,
        repository: Arc<dyn MemoryRepository>,
        events: Arc<dyn MemoryEventLog>,
        config: MemoryRuntimeConfig,
    ) -> Self {
        let planner = Arc::new(DeterministicPlanner::new());
        Self {
            user,
            config,
            repository,
            canonical: Arc::new(IndexHandle::new()),
            planner: Arc::new(RwLock::new(planner.clone())),
            events,
            plan_extractor: Arc::new(RwLock::new(Arc::new(DeterministicPlanExtractor::new(
                planner,
            )))),
            observation_extractor: Arc::new(RuleBasedObservationExtractor::new()),
        }
    }

    /// Use a model-backed retrieval-plan extractor.
    ///
    /// The extractor is wrapped in a deadline that falls back to the rule-based
    /// plan, so an unavailable model degrades retrieval quality rather than
    /// stalling the pipeline.
    pub fn with_plan_extractor(self, extractor: Arc<dyn RetrievalPlanExtractor>) -> Self {
        *self.plan_extractor.write() = Arc::new(crate::retrieval::BoundedPlanExtractor::new(
            extractor,
            Duration::from_millis(500),
        ));
        self
    }

    /// Use a model-backed observation extractor, under the configured deadline.
    pub fn with_observation_extractor(
        mut self,
        extractor: Arc<dyn MemoryObservationExtractor>,
    ) -> Self {
        self.observation_extractor = Arc::new(BoundedObservationExtractor::new(
            extractor,
            Duration::from_millis(self.config.ingestion.extraction_soft_timeout_ms),
        ));
        self
    }

    /// The user whose memory this engine serves.
    pub fn user(&self) -> &UserId {
        &self.user
    }

    /// The canonical repository.
    pub fn repository(&self) -> &Arc<dyn MemoryRepository> {
        &self.repository
    }

    /// The event log.
    pub fn events(&self) -> &Arc<dyn MemoryEventLog> {
        &self.events
    }

    /// Runtime configuration.
    pub fn config(&self) -> &MemoryRuntimeConfig {
        &self.config
    }

    /// Compile the retrieval index from the canonical corpus.
    ///
    /// The index is derived and disposable; this rebuilds it from scratch, which
    /// is also the recovery path after any index corruption or schema change.
    pub async fn compile_index(&self) -> Result<u64, MemoryError> {
        let records = self.repository.all(&self.user).await?;
        let index = MemoryIndex::build(
            records
                .iter()
                .filter(|m| m.status == MemoryStatus::Active)
                .map(IndexedMemory::from_canonical),
        );

        let known = KnownEntities::from_index(&index);
        let planner = Arc::new(DeterministicPlanner::with_entities(known));
        *self.planner.write() = planner.clone();
        // Keep the default extractor in step with the refreshed entity table.
        // A model-backed extractor installed by the caller is left alone.
        self.canonical.replace(index);

        let revision = self.canonical.revision();
        let writer = SessionEventWriter::new(
            self.events.clone(),
            self.user.clone(),
            SessionId::new("index"),
        );
        let _ = writer
            .append(None, MemoryEvent::IndexRevisionPublished { revision })
            .await;
        Ok(revision)
    }

    /// Begin a logical conversation.
    pub fn begin_session(&self, session_id: SessionId) -> MemorySession {
        let overlay_handle = Arc::new(IndexHandle::new());
        let retriever = Arc::new(LocalMemoryRetriever::new(
            self.canonical.clone(),
            overlay_handle.clone(),
            self.config.retrieval.clone(),
        ));

        MemorySession {
            user: self.user.clone(),
            session_id: session_id.clone(),
            config: self.config.clone(),
            repository: self.repository.clone(),
            ledger: Arc::new(InMemorySessionLedger::new(
                session_id.clone(),
                self.config.ingestion.clone(),
            )),
            overlay: RwLock::new(SessionMemoryOverlay::new()),
            overlay_handle,
            retriever,
            planner: self.planner.clone(),
            plan_extractor: self.plan_extractor.clone(),
            observation_extractor: self.observation_extractor.clone(),
            generation: GenerationGuard::new(),
            prepared: RwLock::new(PreparedMemorySnapshot::empty(TurnId::ZERO)),
            active: RwLock::new(PreparedMemorySnapshot::empty(TurnId::ZERO)),
            cadence: RwLock::new(CadenceTracker::new(&self.config, Utc::now())),
            events: SessionEventWriter::new(self.events.clone(), self.user.clone(), session_id),
            canonical: self.canonical.clone(),
        }
    }

    /// Run a promotion sweep over staged patterns.
    pub async fn promote_patterns(&self) -> Result<usize, MemoryError> {
        let records = self.repository.all(&self.user).await?;
        let outcomes =
            crate::reconcile::sweep(&records, &self.config.pattern_promotion, Utc::now());
        let promoted = outcomes
            .iter()
            .filter(|o| matches!(o, crate::reconcile::PromotionOutcome::Promote(_)))
            .count();
        if outcomes.is_empty() {
            return Ok(0);
        }
        crate::reconcile::commit_promotions(
            &self.repository,
            &self.user,
            outcomes,
            &format!("promotion-{}", Utc::now().timestamp()),
        )
        .await?;
        self.compile_index().await?;
        Ok(promoted)
    }
}

/// Everything scoped to one logical conversation.
pub struct MemorySession {
    user: UserId,
    session_id: SessionId,
    config: MemoryRuntimeConfig,
    repository: Arc<dyn MemoryRepository>,
    ledger: Arc<InMemorySessionLedger>,
    overlay: RwLock<SessionMemoryOverlay>,
    overlay_handle: Arc<IndexHandle>,
    retriever: Arc<LocalMemoryRetriever>,
    planner: Arc<RwLock<Arc<DeterministicPlanner>>>,
    plan_extractor: Arc<RwLock<Arc<dyn RetrievalPlanExtractor>>>,
    observation_extractor: Arc<dyn MemoryObservationExtractor>,
    generation: GenerationGuard,
    prepared: RwLock<PreparedMemorySnapshot>,
    active: RwLock<PreparedMemorySnapshot>,
    cadence: RwLock<CadenceTracker>,
    events: SessionEventWriter,
    canonical: Arc<IndexHandle>,
}

impl MemorySession {
    /// The logical conversation this session represents.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// The candidate ledger.
    pub fn ledger(&self) -> &Arc<InMemorySessionLedger> {
        &self.ledger
    }

    /// The retriever serving this session.
    pub fn retriever(&self) -> &Arc<LocalMemoryRetriever> {
        &self.retriever
    }

    /// The runtime configuration this session was opened with.
    pub fn config(&self) -> &MemoryRuntimeConfig {
        &self.config
    }

    /// Freeze the prepared snapshot for a turn that is starting.
    ///
    /// Everything the model is answered with for this turn comes from the
    /// snapshot taken here, so a retrieval landing mid-response cannot change
    /// what B appears to remember halfway through a sentence.
    pub fn begin_turn(&self, turn_id: TurnId) -> u64 {
        let prepared = self.prepared.read().clone();
        *self.active.write() = prepared;
        self.cadence.write().touch(Utc::now());
        let _ = turn_id;
        self.generation.advance()
    }

    /// The snapshot the current turn is being answered from.
    pub fn active_snapshot(&self) -> PreparedMemorySnapshot {
        self.active.read().clone()
    }

    /// The most recently prepared snapshot, whether or not a turn is using it.
    pub fn prepared_snapshot(&self) -> PreparedMemorySnapshot {
        self.prepared.read().clone()
    }

    /// The generation guard, for cancelling stale speculative work.
    pub fn generation(&self) -> &GenerationGuard {
        &self.generation
    }

    /// Prepare context speculatively from a transcript.
    ///
    /// Publishes the resulting snapshot only if the conversation has not moved
    /// on since the work started.
    pub async fn prepare(
        &self,
        turn_id: TurnId,
        transcript: &str,
    ) -> Result<PreparedMemorySnapshot, MemoryError> {
        let generation = self.generation.current();
        let now = Utc::now();
        let planner = self.planner.read().clone();
        let context = context_for(&planner, transcript, turn_id, generation, now);
        let extractor = self.plan_extractor.read().clone();
        let plan = extractor.extract(context).await?;

        let snapshot = self
            .retriever
            .prepare(RetrievalRequest { plan, now })
            .await?;

        if self.generation.is_current(generation) {
            *self.prepared.write() = snapshot.clone();
        }
        Ok(snapshot)
    }

    /// Serve a `recall_context` tool call.
    ///
    /// Reads the frozen snapshot when it covers the query, and falls back to a
    /// live lexical search when speculation missed. The fallback is bounded and
    /// never reaches the network.
    pub async fn recall(&self, query: &str, turn_id: TurnId) -> serde_json::Value {
        let now = Utc::now();
        let active = self.active_snapshot();
        if active.satisfies(query, now) {
            return active.to_tool_payload();
        }
        match self
            .retriever
            .retrieve_immediate(query, turn_id, RetrievalBudget::interactive())
            .await
        {
            Ok(snapshot) => snapshot.to_tool_payload(),
            // Memory failure degrades to "nothing found"; it never fails a turn.
            Err(_) => PreparedMemorySnapshot::empty(turn_id).to_tool_payload(),
        }
    }

    /// Serve a `recall_context` tool call restricted to a scope.
    ///
    /// An unrestricted recall may be answered from the frozen snapshot; a
    /// scoped one always runs a live local search, because the snapshot was
    /// prepared without knowing the restriction.
    pub async fn recall_scoped(
        &self,
        query: &str,
        turn_id: TurnId,
        scope: crate::runtime::tools::RecallScope,
    ) -> serde_json::Value {
        let kinds = scope.kinds();
        if kinds.is_empty() {
            return self.recall(query, turn_id).await;
        }
        match self
            .retriever
            .retrieve_scoped(query, turn_id, RetrievalBudget::interactive(), kinds)
            .await
        {
            Ok(snapshot) => snapshot.to_tool_payload(),
            Err(_) => PreparedMemorySnapshot::empty(turn_id).to_tool_payload(),
        }
    }

    /// Record a finalized user turn: durable evidence, then extraction.
    ///
    /// The transcript event is appended before extraction runs, so a crash
    /// between the two loses an extraction that can be retried rather than the
    /// evidence itself.
    pub async fn observe_final_transcript(
        &self,
        turn_id: TurnId,
        transcript: &str,
    ) -> Result<Vec<LedgerOutcome>, MemoryError> {
        self.events
            .append(
                Some(turn_id),
                MemoryEvent::FinalTranscriptRecorded {
                    text: transcript.to_string(),
                },
            )
            .await?;

        let observations = match self
            .observation_extractor
            .extract(ObservationExtractionContext::user_turn(
                transcript,
                self.session_id.clone(),
                turn_id,
                Utc::now(),
            ))
            .await
        {
            Ok(observations) => observations,
            Err(error) => {
                // Degrade — a failed extraction must never fail the turn — but
                // record it. Silently returning nothing makes a broken
                // extractor indistinguishable from a quiet conversation.
                let _ = self
                    .events
                    .append(
                        Some(turn_id),
                        MemoryEvent::ExtractionFailed {
                            stage: "observation".to_string(),
                            reason: error.to_string(),
                        },
                    )
                    .await;
                Vec::new()
            }
        };

        let mut outcomes = Vec::new();
        for observation in observations {
            let fingerprint = observation.fingerprint();
            if let Some(intent) = observation.mutation_intent {
                self.events
                    .append(
                        Some(turn_id),
                        MemoryEvent::ExplicitMutationRequested {
                            intent,
                            statement: observation.canonical_statement.clone(),
                        },
                    )
                    .await?;
            }
            let outcome = self.ledger.append_observation(observation).await?;
            if let LedgerOutcome::Rejected(reason) = &outcome {
                // The real fingerprint, so the audit trail names the fact that
                // was refused rather than a placeholder.
                let _ = self
                    .events
                    .append(
                        Some(turn_id),
                        MemoryEvent::ObservationRejected {
                            fingerprint: fingerprint.clone(),
                            reason: *reason,
                        },
                    )
                    .await;
            }
            outcomes.push(outcome);
        }

        self.refresh_overlay().await;
        Ok(outcomes)
    }

    /// Complete a turn and run whatever the cadence says is now due.
    pub async fn on_turn_complete(
        &self,
        turn_id: TurnId,
    ) -> Result<Vec<ScheduledWork>, MemoryError> {
        let due = self.cadence.write().on_turn_complete(Utc::now());
        for work in &due {
            match work {
                ScheduledWork::MicroReconcile => {
                    self.ledger.micro_reconcile();
                    self.refresh_overlay().await;
                }
                ScheduledWork::Checkpoint => {
                    self.ledger.micro_reconcile();
                    self.refresh_overlay().await;
                    let turns = self.cadence.read().total_turns();
                    self.events
                        .append(turn_id.into(), MemoryEvent::SessionCheckpointed { turns })
                        .await?;
                }
                ScheduledWork::SealSession => {}
            }
        }
        Ok(due)
    }

    /// Whether the session has been idle long enough to seal.
    pub fn is_idle(&self) -> bool {
        self.cadence.read().is_idle(Utc::now())
    }

    /// Seal the session and reconcile its evidence into canonical memory.
    ///
    /// Idempotent by session id: reconciling twice commits once.
    pub async fn finish(&self) -> Result<ReconciliationReport, MemoryError> {
        self.ledger.micro_reconcile();
        let sealed = self.ledger.seal().await?;
        self.cadence.write().seal();
        self.events
            .append(
                None,
                MemoryEvent::SessionSealed {
                    candidate_count: sealed.candidates.len(),
                },
            )
            .await?;

        let output = consolidate(&sealed);
        let committer =
            MemoryCommitter::new(self.repository.clone()).with_events(self.events.clone());
        let report = committer
            .reconcile(&self.user, output, self.session_id.as_str())
            .await?;

        if !report.is_empty() {
            self.recompile_canonical().await?;
        }
        self.overlay.write().clear();
        self.overlay_handle.replace(MemoryIndex::new());
        self.retriever.invalidate_cache();
        Ok(report)
    }

    /// Apply an explicit memory command from the user.
    ///
    /// Explicit intent takes effect in the conversation immediately and commits
    /// durably afterwards. The durable event is appended before the overlay is
    /// touched, so the engine never tells a user their correction was recorded
    /// when it was not.
    pub async fn apply_explicit_command(
        &self,
        intent: MutationIntent,
        statement: &str,
        turn_id: TurnId,
    ) -> Result<serde_json::Value, MemoryError> {
        self.events
            .append(
                Some(turn_id),
                MemoryEvent::ExplicitMutationRequested {
                    intent,
                    statement: statement.to_string(),
                },
            )
            .await?;

        if intent == MutationIntent::List {
            return Ok(serde_json::json!({
                "status": "accepted",
                "operation": "list",
                "facts": self.known_statements(),
            }));
        }

        let observation = explicit_observation(
            intent,
            statement,
            self.session_id.clone(),
            turn_id,
            Utc::now(),
        );
        let outcome = self.ledger.append_observation(observation).await?;
        self.refresh_overlay().await;

        let accepted = !matches!(outcome, LedgerOutcome::Rejected(_));
        Ok(serde_json::json!({
            "status": if accepted { "accepted" } else { "refused" },
            "operation": operation_label(intent),
            "effective_in_session": accepted,
            "durable_commit": "pending",
        }))
    }

    /// Every statement currently retrievable, canonical and provisional.
    pub fn known_statements(&self) -> Vec<String> {
        let mut statements: Vec<String> = self
            .canonical
            .read()
            .documents()
            .map(|d| d.statement.clone())
            .collect();
        statements.extend(
            self.ledger
                .usable_candidates()
                .iter()
                .map(|c| c.canonical_statement.clone()),
        );
        statements.sort();
        statements.dedup();
        statements
    }

    /// Everything the user has explicitly asked to be remembered this session.
    pub fn pending_explicit_commands(&self) -> Vec<MutationIntent> {
        self.ledger
            .usable_candidates()
            .iter()
            .filter_map(|c| c.mutation_intent)
            .collect()
    }

    async fn refresh_overlay(&self) {
        let candidates = self.ledger.usable_candidates();

        // Anything the user stated outright this session hides the durable
        // record it contradicts for the rest of the conversation. Reconciliation
        // will make that permanent; until then the overlay is the truth.
        let suppressed: std::collections::HashSet<String> = candidates
            .iter()
            .filter(|c| {
                c.explicitness.is_explicit() && c.mutation_intent != Some(MutationIntent::List)
            })
            .map(|c| c.subject_predicate().to_string())
            .collect();
        self.retriever.suppress_windows(suppressed);

        let revision = {
            let mut overlay = self.overlay.write();
            overlay.rebuild(&self.user, &self.session_id, &candidates, Utc::now());
            self.overlay_handle.replace(clone_index(overlay.index()));
            overlay.revision()
        };
        self.retriever.invalidate_cache();
        let _ = self
            .events
            .append(None, MemoryEvent::SessionOverlayUpdated { revision })
            .await;
    }

    async fn recompile_canonical(&self) -> Result<(), MemoryError> {
        let records = self.repository.all(&self.user).await?;
        self.canonical.replace(MemoryIndex::build(
            records
                .iter()
                .filter(|m| m.status == MemoryStatus::Active)
                .map(IndexedMemory::from_canonical),
        ));
        let planner = Arc::new(DeterministicPlanner::with_entities(
            KnownEntities::from_index(&self.canonical.read()),
        ));
        *self.planner.write() = planner;
        Ok(())
    }
}

/// Build the observation behind an explicit memory command.
///
/// Explicit commands are the one path where the engine trusts a statement
/// wholesale: the user said it about themselves, on purpose, to be remembered.
fn explicit_observation(
    intent: MutationIntent,
    statement: &str,
    session_id: SessionId,
    turn_id: TurnId,
    now: chrono::DateTime<Utc>,
) -> crate::core::MemoryObservation {
    use crate::core::{
        CanonicalPredicate, EntityRef, Explicitness, MemoryKind, MemoryValue, ObservationId,
        ProposedPersistence, SensitivityClass, SpeakerAttribution, TemporalScope,
        TranscriptEvidence,
    };

    let predicate = match intent {
        MutationIntent::Forget | MutationIntent::Delete => "memory_removal",
        MutationIntent::List => "memory_listing",
        _ => "preference",
    };

    crate::core::MemoryObservation {
        observation_id: ObservationId::generate(),
        session_id,
        turn_id,
        subject: EntityRef::user(),
        predicate: CanonicalPredicate::new(predicate),
        value: MemoryValue::Text(statement.to_string()),
        canonical_statement: statement.to_string(),
        kind: MemoryKind::Preference,
        explicitness: Explicitness::ExplicitCommand,
        confidence: 1.0,
        persistence: ProposedPersistence::Durable,
        temporal_scope: TemporalScope::Persistent,
        valid_from: Some(now),
        expected_expiry: None,
        transcript_evidence: TranscriptEvidence::new(statement),
        speaker_attribution: SpeakerAttribution::User,
        sensitivity: SensitivityClass::Normal,
        mutation_intent: Some(intent),
    }
}

fn operation_label(intent: MutationIntent) -> &'static str {
    match intent {
        MutationIntent::Remember => "remember",
        MutationIntent::Correct => "correct",
        MutationIntent::Forget => "forget",
        MutationIntent::Delete => "delete",
        MutationIntent::List => "list",
    }
}

/// Copy an index so the overlay and the retriever's handle stay independent.
///
/// The overlay owns the authoritative projection of the ledger; the handle is
/// what the retriever reads without ever blocking on a rebuild.
fn clone_index(source: &MemoryIndex) -> MemoryIndex {
    MemoryIndex::build(source.documents().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::InMemoryEventLog;

    fn engine() -> MemoryEngine {
        MemoryEngine::in_memory(UserId::new("usr_1"))
    }

    #[tokio::test]
    async fn a_fact_stated_this_session_is_recalled_in_a_later_turn() {
        let engine = engine();
        let session = engine.begin_session(SessionId::new("ses_1"));

        session.begin_turn(TurnId(1));
        session
            .observe_final_transcript(TurnId(1), "I am pescatarian")
            .await
            .unwrap();
        session.on_turn_complete(TurnId(1)).await.unwrap();

        session.begin_turn(TurnId(2));
        let snapshot = session
            .prepare(
                TurnId(2),
                "what do you remember about my dietary preferences",
            )
            .await
            .unwrap();
        assert!(
            snapshot
                .facts
                .iter()
                .any(|f| f.statement.contains("pescatarian")),
            "prepared: {:?}",
            snapshot.facts
        );
    }

    #[tokio::test]
    async fn a_fact_survives_into_the_next_session_after_reconciliation() {
        let engine = engine();

        let first = engine.begin_session(SessionId::new("ses_1"));
        first.begin_turn(TurnId(1));
        first
            .observe_final_transcript(TurnId(1), "I am pescatarian")
            .await
            .unwrap();
        let report = first.finish().await.unwrap();
        assert_eq!(report.creates, 1);

        engine.compile_index().await.unwrap();

        let second = engine.begin_session(SessionId::new("ses_2"));
        second.begin_turn(TurnId(1));
        let snapshot = second
            .prepare(
                TurnId(1),
                "what do you remember about my dietary preferences",
            )
            .await
            .unwrap();
        assert!(snapshot
            .facts
            .iter()
            .any(|f| f.statement.contains("pescatarian")));
    }

    #[tokio::test]
    async fn a_correction_stated_mid_session_takes_effect_on_the_next_turn() {
        let engine = engine();
        let session = engine.begin_session(SessionId::new("ses_1"));

        session.begin_turn(TurnId(1));
        session
            .observe_final_transcript(TurnId(1), "I am vegetarian")
            .await
            .unwrap();
        session.on_turn_complete(TurnId(1)).await.unwrap();

        session.begin_turn(TurnId(2));
        session
            .observe_final_transcript(TurnId(2), "actually I am pescatarian")
            .await
            .unwrap();
        session.on_turn_complete(TurnId(2)).await.unwrap();

        session.begin_turn(TurnId(3));
        let payload = session
            .recall("diet vegetarian pescatarian", TurnId(3))
            .await;
        let rendered = payload.to_string();
        assert!(rendered.contains("pescatarian"), "got {rendered}");
    }

    #[tokio::test]
    async fn a_correction_hides_the_durable_fact_it_contradicts_immediately() {
        let engine = engine();

        // Monday: the user is vegetarian, and it is reconciled and indexed.
        let monday = engine.begin_session(SessionId::new("ses_monday"));
        monday.begin_turn(TurnId(1));
        monday
            .observe_final_transcript(TurnId(1), "I am vegetarian")
            .await
            .unwrap();
        monday.finish().await.unwrap();
        engine.compile_index().await.unwrap();

        // Thursday: the user corrects it. The old fact must stop being
        // retrieved now, not after the session reconciles.
        let thursday = engine.begin_session(SessionId::new("ses_thursday"));
        thursday.begin_turn(TurnId(1));
        thursday
            .observe_final_transcript(TurnId(1), "actually I am pescatarian")
            .await
            .unwrap();
        thursday.on_turn_complete(TurnId(1)).await.unwrap();

        thursday.begin_turn(TurnId(2));
        let snapshot = thursday
            .prepare(
                TurnId(2),
                "what do you remember about my dietary preferences",
            )
            .await
            .unwrap();

        let statements: Vec<&str> = snapshot
            .facts
            .iter()
            .map(|f| f.statement.as_str())
            .collect();
        assert!(
            statements.iter().any(|s| s.contains("pescatarian")),
            "the correction was not recalled: {statements:?}"
        );
        assert!(
            !statements.iter().any(|s| s.contains("vegetarian")),
            "the corrected-away fact was still recalled: {statements:?}"
        );
    }

    #[tokio::test]
    async fn asking_what_is_remembered_does_not_itself_become_a_memory() {
        let engine = engine();
        let session = engine.begin_session(SessionId::new("ses_1"));
        session.begin_turn(TurnId(1));
        session
            .observe_final_transcript(TurnId(1), "I am pescatarian")
            .await
            .unwrap();
        session.begin_turn(TurnId(2));
        session
            .observe_final_transcript(TurnId(2), "what do you remember about me")
            .await
            .unwrap();

        let snapshot = session
            .prepare(
                TurnId(3),
                "what do you remember about my dietary preferences",
            )
            .await
            .unwrap();
        assert!(
            !snapshot
                .facts
                .iter()
                .any(|f| f.statement.contains("asked what is remembered")),
            "a question about memory was recalled as a memory: {:?}",
            snapshot.facts
        );
    }

    #[tokio::test]
    async fn a_generic_question_gets_no_memory_and_no_search() {
        let engine = engine();
        let session = engine.begin_session(SessionId::new("ses_1"));
        session.begin_turn(TurnId(1));
        let snapshot = session
            .prepare(TurnId(1), "what is the capital of France")
            .await
            .unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(
            session.recall("capital of France", TurnId(1)).await["status"],
            "not_found"
        );
    }

    #[tokio::test]
    async fn the_active_snapshot_does_not_change_mid_turn() {
        let engine = engine();
        let session = engine.begin_session(SessionId::new("ses_1"));

        session.begin_turn(TurnId(1));
        session
            .observe_final_transcript(TurnId(1), "I am pescatarian")
            .await
            .unwrap();
        session.begin_turn(TurnId(2));
        session.prepare(TurnId(2), "what do I eat").await.unwrap();

        let during_turn = session.active_snapshot();
        // A newer preparation lands while turn 2 is still in flight.
        session
            .observe_final_transcript(TurnId(3), "I am allergic to nuts")
            .await
            .unwrap();
        session
            .prepare(TurnId(3), "what am I allergic to")
            .await
            .unwrap();

        assert_eq!(
            session.active_snapshot().snapshot_id,
            during_turn.snapshot_id,
            "the in-flight turn keeps its frozen snapshot"
        );
    }

    #[tokio::test]
    async fn bystander_speech_never_reaches_the_ledger() {
        let engine = engine();
        let session = engine.begin_session(SessionId::new("ses_1"));
        session.begin_turn(TurnId(1));
        session
            .observe_final_transcript(TurnId(1), "the weather is nice today")
            .await
            .unwrap();
        assert!(session.ledger().is_empty());
    }

    #[tokio::test]
    async fn reconciling_the_same_session_twice_writes_once() {
        let engine = engine();
        let session = engine.begin_session(SessionId::new("ses_1"));
        session.begin_turn(TurnId(1));
        session
            .observe_final_transcript(TurnId(1), "I am pescatarian")
            .await
            .unwrap();

        session.finish().await.unwrap();
        session.finish().await.unwrap();

        let stored = engine.repository().all(engine.user()).await.unwrap();
        assert_eq!(stored.len(), 1);
    }

    #[tokio::test]
    async fn the_event_log_records_the_turn_before_the_extraction() {
        let log = Arc::new(InMemoryEventLog::new());
        let engine = MemoryEngine::new(
            UserId::new("usr_1"),
            Arc::new(OkfRepository::in_memory()),
            log.clone(),
            MemoryRuntimeConfig::default(),
        );
        let session = engine.begin_session(SessionId::new("ses_1"));
        session
            .observe_final_transcript(TurnId(1), "I am pescatarian")
            .await
            .unwrap();

        let entries = log.entries();
        assert_eq!(entries[0].payload.label(), "final_transcript_recorded");
    }

    #[tokio::test]
    async fn a_staged_pattern_is_promoted_once_it_spans_sessions_and_days() {
        let engine = engine();

        // Two sessions, each stating the routine outright.
        for (idx, session_id) in ["ses_1", "ses_2"].iter().enumerate() {
            let session = engine.begin_session(SessionId::new(*session_id));
            session.begin_turn(TurnId(1));
            session
                .observe_final_transcript(TurnId(1), "I always go to the gym before work")
                .await
                .unwrap();
            session.finish().await.unwrap();
            let _ = idx;
        }

        let stored = engine.repository().all(engine.user()).await.unwrap();
        let routine = stored
            .iter()
            .find(|m| m.predicate.as_str() == "exercise_routine")
            .expect("the routine was recorded");
        assert_eq!(routine.evidence.distinct_sessions, 2);
    }
}
