//! The session candidate ledger.
//!
//! Observations accumulate here before anything becomes canonical. The ledger
//! is what makes a crash survivable, what turns three mentions of the same
//! thing into one candidate with three pieces of evidence, and what
//! consolidation reads at the end of a session.

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

use crate::core::{
    admit_observation, aggregate_confidence, AdmissionVerdict, DiscardReason, Explicitness,
    FactFingerprint, IngestionConfig, MemoryError, MemoryKind, MemoryObservation, MemoryValue,
    MutationIntent, ObservationId, ProposedPersistence, SessionId, TemporalScope, TurnId,
};

/// Where a candidate stands within the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCandidateStatus {
    /// Seen once; not yet trusted enough to answer with.
    Observed,
    /// Usable in this conversation.
    ActiveSessionFact,
    /// Will be offered to post-session reconciliation.
    StagedForReconciliation,
    /// Contradicted by something the user said later in the session.
    Suppressed,
    /// Refused by policy.
    Rejected,
    /// An explicit command awaiting durable commit.
    PendingExplicitCommit,
}

impl SessionCandidateStatus {
    /// Whether a candidate in this state may be retrieved during the session.
    pub fn is_usable(self) -> bool {
        matches!(
            self,
            Self::ActiveSessionFact | Self::StagedForReconciliation | Self::PendingExplicitCommit
        )
    }
}

/// One piece of evidence behind a candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationEvidence {
    /// Which observation.
    pub observation_id: ObservationId,
    /// Which turn it came from.
    pub turn_id: TurnId,
    /// How directly it was stated.
    pub explicitness: Explicitness,
    /// Extractor confidence.
    pub confidence: f32,
    /// The utterance behind it.
    pub utterance: String,
    /// When it was observed.
    pub observed_at: DateTime<Utc>,
}

/// A proposed memory accumulating evidence within a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCandidate {
    /// Deduplication key.
    pub fingerprint: FactFingerprint,
    /// Subject of the fact.
    pub subject: crate::core::EntityRef,
    /// Canonical predicate.
    pub predicate: crate::core::CanonicalPredicate,
    /// Value side.
    pub value: MemoryValue,
    /// Natural-language rendering.
    pub canonical_statement: String,
    /// Proposed kind.
    pub kind: MemoryKind,
    /// Expected persistence.
    pub temporal_scope: TemporalScope,
    /// Supporting evidence.
    pub evidence: Vec<ObservationEvidence>,
    /// Distinct turns that produced evidence.
    pub distinct_turns: usize,
    /// First turn the candidate appeared on.
    pub first_seen_turn: TurnId,
    /// Most recent turn the candidate appeared on.
    pub last_seen_turn: TurnId,
    /// Aggregated confidence.
    pub confidence: f32,
    /// Strongest explicitness across the evidence.
    pub explicitness: Explicitness,
    /// Retention policy assigned at admission.
    pub proposed_persistence: ProposedPersistence,
    /// Lifecycle state.
    pub status: SessionCandidateStatus,
    /// Explicit command carried by the evidence, if any.
    pub mutation_intent: Option<MutationIntent>,
    /// When the fact stops holding, for episodic candidates.
    pub expected_expiry: Option<DateTime<Utc>>,
}

impl SessionCandidate {
    fn from_observation(
        observation: &MemoryObservation,
        persistence: ProposedPersistence,
        now: DateTime<Utc>,
    ) -> Self {
        let evidence = ObservationEvidence {
            observation_id: observation.observation_id.clone(),
            turn_id: observation.turn_id,
            explicitness: observation.explicitness,
            confidence: observation.confidence,
            utterance: observation.transcript_evidence.utterance.clone(),
            observed_at: now,
        };
        let status = status_for(persistence, observation);
        Self {
            fingerprint: observation.fingerprint(),
            subject: observation.subject.clone(),
            predicate: observation.predicate.clone(),
            value: observation.value.clone(),
            canonical_statement: observation.canonical_statement.clone(),
            kind: observation.kind,
            temporal_scope: observation.temporal_scope,
            evidence: vec![evidence],
            distinct_turns: 1,
            first_seen_turn: observation.turn_id,
            last_seen_turn: observation.turn_id,
            confidence: observation.confidence,
            explicitness: observation.explicitness,
            proposed_persistence: persistence,
            status,
            mutation_intent: observation.mutation_intent,
            expected_expiry: observation.expected_expiry,
        }
    }

    fn absorb(&mut self, observation: &MemoryObservation, now: DateTime<Utc>) {
        if self
            .evidence
            .iter()
            .any(|e| e.observation_id == observation.observation_id)
        {
            return;
        }
        let new_turn = observation.turn_id != self.last_seen_turn;
        self.evidence.push(ObservationEvidence {
            observation_id: observation.observation_id.clone(),
            turn_id: observation.turn_id,
            explicitness: observation.explicitness,
            confidence: observation.confidence,
            utterance: observation.transcript_evidence.utterance.clone(),
            observed_at: now,
        });
        if new_turn {
            self.distinct_turns += 1;
        }
        self.last_seen_turn = observation.turn_id;
        self.explicitness = self.explicitness.max(observation.explicitness);
        self.confidence = aggregate_confidence(
            &self
                .evidence
                .iter()
                .map(|e| (e.confidence, e.explicitness))
                .collect::<Vec<_>>(),
        );
        if observation.mutation_intent.is_some() {
            self.mutation_intent = observation.mutation_intent;
        }
        if self.status == SessionCandidateStatus::Observed && self.explicitness.is_explicit() {
            self.status = SessionCandidateStatus::ActiveSessionFact;
        }
    }

    /// Distinct calendar days the evidence spans.
    pub fn distinct_days(&self) -> u32 {
        self.evidence
            .iter()
            .map(|e| (e.observed_at.year(), e.observed_at.ordinal()))
            .collect::<HashSet<_>>()
            .len() as u32
    }

    /// The `subject|predicate` window used to find contradictions.
    pub fn subject_predicate(&self) -> &str {
        self.fingerprint.subject_predicate()
    }
}

fn status_for(
    persistence: ProposedPersistence,
    observation: &MemoryObservation,
) -> SessionCandidateStatus {
    if observation.mutation_intent.is_some() {
        return SessionCandidateStatus::PendingExplicitCommit;
    }
    match persistence {
        ProposedPersistence::Durable | ProposedPersistence::Episodic => {
            SessionCandidateStatus::ActiveSessionFact
        }
        ProposedPersistence::SessionOnly => SessionCandidateStatus::ActiveSessionFact,
        ProposedPersistence::Staged => SessionCandidateStatus::Observed,
        ProposedPersistence::Discard => SessionCandidateStatus::Rejected,
    }
}

/// What happened when an observation was offered to the ledger.
#[derive(Debug, Clone, PartialEq)]
pub enum LedgerOutcome {
    /// A new candidate was created.
    Created(FactFingerprint),
    /// Evidence was added to an existing candidate.
    Reinforced {
        /// The candidate reinforced.
        fingerprint: FactFingerprint,
        /// Evidence count after the merge.
        evidence_count: usize,
    },
    /// The observation was refused by policy.
    Rejected(DiscardReason),
}

/// A ledger sealed against further writes.
#[derive(Debug, Clone)]
pub struct SealedSessionLedger {
    /// The session it belongs to.
    pub session_id: SessionId,
    /// Candidates carried into consolidation.
    pub candidates: Vec<SessionCandidate>,
    /// When it was sealed.
    pub sealed_at: DateTime<Utc>,
}

/// A snapshot of the ledger mid-session.
#[derive(Debug, Clone)]
pub struct SessionLedgerSnapshot {
    /// The session it belongs to.
    pub session_id: SessionId,
    /// Every candidate, in fingerprint order.
    pub candidates: Vec<SessionCandidate>,
    /// Ledger revision.
    pub revision: u64,
}

/// Accumulates observations for a logical session.
#[async_trait]
pub trait SessionLedger: Send + Sync {
    /// Offer an observation to the ledger.
    async fn append_observation(
        &self,
        observation: MemoryObservation,
    ) -> Result<LedgerOutcome, MemoryError>;

    /// Read the current candidate set.
    async fn snapshot(&self) -> Result<SessionLedgerSnapshot, MemoryError>;

    /// Close the ledger and hand its candidates to consolidation.
    async fn seal(&self) -> Result<SealedSessionLedger, MemoryError>;
}

/// The in-process ledger.
#[derive(Debug)]
pub struct InMemorySessionLedger {
    session_id: SessionId,
    config: IngestionConfig,
    inner: RwLock<LedgerState>,
}

#[derive(Debug, Default)]
struct LedgerState {
    candidates: BTreeMap<FactFingerprint, SessionCandidate>,
    revision: u64,
    sealed: bool,
}

impl InMemorySessionLedger {
    /// A ledger for one session.
    pub fn new(session_id: SessionId, config: IngestionConfig) -> Self {
        Self {
            session_id,
            config,
            inner: RwLock::new(LedgerState::default()),
        }
    }

    /// The session this ledger belongs to.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// The ledger revision, bumped on every accepted observation.
    pub fn revision(&self) -> u64 {
        self.inner.read().revision
    }

    /// How many candidates are held.
    pub fn len(&self) -> usize {
        self.inner.read().candidates.len()
    }

    /// Whether the ledger holds no candidates.
    pub fn is_empty(&self) -> bool {
        self.inner.read().candidates.is_empty()
    }

    /// Candidates usable for retrieval right now.
    pub fn usable_candidates(&self) -> Vec<SessionCandidate> {
        self.inner
            .read()
            .candidates
            .values()
            .filter(|c| c.status.is_usable())
            .cloned()
            .collect()
    }

    /// Merge duplicates, resolve in-session contradictions, and re-evaluate
    /// statuses (§18).
    ///
    /// Runs on a cadence during the session. Deliberately local: it compares
    /// candidates against each other, never against the durable corpus.
    pub fn micro_reconcile(&self) -> MicroReconciliationReport {
        let mut state = self.inner.write();
        let mut report = MicroReconciliationReport::default();

        // Within one `subject|predicate` window, the newest explicit statement
        // wins and older competing values are suppressed. A user who says
        // "actually, pescatarian" has not left two beliefs behind.
        let mut by_window: BTreeMap<String, Vec<FactFingerprint>> = BTreeMap::new();
        for (fingerprint, candidate) in state.candidates.iter() {
            if candidate.status == SessionCandidateStatus::Rejected {
                continue;
            }
            by_window
                .entry(candidate.subject_predicate().to_string())
                .or_default()
                .push(fingerprint.clone());
        }

        for (_, fingerprints) in by_window {
            if fingerprints.len() < 2 {
                continue;
            }
            let winner = fingerprints
                .iter()
                .max_by(|a, b| {
                    let ca = &state.candidates[*a];
                    let cb = &state.candidates[*b];
                    ca.explicitness
                        .cmp(&cb.explicitness)
                        .then_with(|| ca.last_seen_turn.cmp(&cb.last_seen_turn))
                })
                .cloned()
                .expect("non-empty window");

            for fingerprint in fingerprints {
                if fingerprint == winner {
                    continue;
                }
                if let Some(candidate) = state.candidates.get_mut(&fingerprint) {
                    if candidate.status != SessionCandidateStatus::Suppressed {
                        candidate.status = SessionCandidateStatus::Suppressed;
                        report.suppressed += 1;
                    }
                }
            }
        }

        // Promote observations that have earned in-session trust.
        for candidate in state.candidates.values_mut() {
            if candidate.status == SessionCandidateStatus::Observed
                && (candidate.distinct_turns >= 2
                    || candidate.confidence >= self.config.minimum_observation_confidence * 2.0)
            {
                candidate.status = SessionCandidateStatus::StagedForReconciliation;
                report.staged += 1;
            }
        }

        report.candidates = state.candidates.len();
        state.revision += 1;
        report
    }
}

/// What one micro-reconciliation pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MicroReconciliationReport {
    /// Candidates suppressed by a later contradiction.
    pub suppressed: usize,
    /// Candidates promoted to staging.
    pub staged: usize,
    /// Total candidates after the pass.
    pub candidates: usize,
}

#[async_trait]
impl SessionLedger for InMemorySessionLedger {
    async fn append_observation(
        &self,
        observation: MemoryObservation,
    ) -> Result<LedgerOutcome, MemoryError> {
        if self.inner.read().sealed {
            return Err(MemoryError::PolicyRefused(
                "session ledger is sealed".to_string(),
            ));
        }

        let persistence = match admit_observation(&observation, &self.config) {
            AdmissionVerdict::Accept(persistence) => persistence,
            AdmissionVerdict::Reject(reason) => return Ok(LedgerOutcome::Rejected(reason)),
        };

        let now = Utc::now();
        let fingerprint = observation.fingerprint();
        let mut state = self.inner.write();
        state.revision += 1;

        match state.candidates.get_mut(&fingerprint) {
            Some(existing) => {
                existing.absorb(&observation, now);
                Ok(LedgerOutcome::Reinforced {
                    fingerprint,
                    evidence_count: existing.evidence.len(),
                })
            }
            None => {
                state.candidates.insert(
                    fingerprint.clone(),
                    SessionCandidate::from_observation(&observation, persistence, now),
                );
                Ok(LedgerOutcome::Created(fingerprint))
            }
        }
    }

    async fn snapshot(&self) -> Result<SessionLedgerSnapshot, MemoryError> {
        let state = self.inner.read();
        Ok(SessionLedgerSnapshot {
            session_id: self.session_id.clone(),
            candidates: state.candidates.values().cloned().collect(),
            revision: state.revision,
        })
    }

    async fn seal(&self) -> Result<SealedSessionLedger, MemoryError> {
        let mut state = self.inner.write();
        state.sealed = true;
        Ok(SealedSessionLedger {
            session_id: self.session_id.clone(),
            candidates: state
                .candidates
                .values()
                .filter(|c| {
                    !matches!(
                        c.status,
                        SessionCandidateStatus::Rejected | SessionCandidateStatus::Suppressed
                    )
                })
                .cloned()
                .collect(),
            sealed_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CanonicalPredicate, EntityRef, SensitivityClass, SpeakerAttribution, TranscriptEvidence,
    };

    fn observation(
        predicate: &str,
        value: &str,
        turn: u64,
        explicitness: Explicitness,
    ) -> MemoryObservation {
        MemoryObservation {
            observation_id: ObservationId::generate(),
            session_id: SessionId::new("ses_1"),
            turn_id: TurnId(turn),
            subject: EntityRef::user(),
            predicate: CanonicalPredicate::new(predicate),
            value: MemoryValue::Text(value.to_string()),
            canonical_statement: format!("The user is {value}."),
            kind: MemoryKind::Preference,
            explicitness,
            confidence: 0.9,
            persistence: ProposedPersistence::Durable,
            temporal_scope: TemporalScope::Persistent,
            valid_from: None,
            expected_expiry: None,
            transcript_evidence: TranscriptEvidence::new(format!("I am {value}")),
            speaker_attribution: SpeakerAttribution::User,
            sensitivity: SensitivityClass::Normal,
            mutation_intent: None,
        }
    }

    fn ledger() -> InMemorySessionLedger {
        InMemorySessionLedger::new(SessionId::new("ses_1"), IngestionConfig::default())
    }

    #[tokio::test]
    async fn the_same_fact_stated_twice_becomes_one_candidate_with_two_evidences() {
        let ledger = ledger();
        let first = ledger
            .append_observation(observation(
                "dietary_identity",
                "pescatarian",
                1,
                Explicitness::ExplicitStatement,
            ))
            .await
            .unwrap();
        assert!(matches!(first, LedgerOutcome::Created(_)));

        let second = ledger
            .append_observation(observation(
                "dietary_identity",
                "pescatarian",
                4,
                Explicitness::ExplicitStatement,
            ))
            .await
            .unwrap();
        assert!(matches!(
            second,
            LedgerOutcome::Reinforced {
                evidence_count: 2,
                ..
            }
        ));
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.usable_candidates()[0].distinct_turns, 2);
    }

    #[tokio::test]
    async fn refused_observations_never_enter_the_ledger() {
        let ledger = ledger();
        let mut bystander = observation(
            "dietary_identity",
            "vegan",
            1,
            Explicitness::ExplicitStatement,
        );
        bystander.speaker_attribution = SpeakerAttribution::Bystander;

        let outcome = ledger.append_observation(bystander).await.unwrap();
        assert_eq!(
            outcome,
            LedgerOutcome::Rejected(DiscardReason::SpeakerNotUser)
        );
        assert!(ledger.is_empty());
    }

    #[tokio::test]
    async fn a_later_contradiction_suppresses_the_earlier_candidate() {
        let ledger = ledger();
        ledger
            .append_observation(observation(
                "dietary_identity",
                "vegetarian",
                1,
                Explicitness::ExplicitStatement,
            ))
            .await
            .unwrap();
        ledger
            .append_observation(observation(
                "dietary_identity",
                "pescatarian",
                5,
                Explicitness::ExplicitStatement,
            ))
            .await
            .unwrap();

        let report = ledger.micro_reconcile();
        assert_eq!(report.suppressed, 1);

        let usable = ledger.usable_candidates();
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].canonical_statement, "The user is pescatarian.");
    }

    #[tokio::test]
    async fn an_explicit_correction_beats_a_more_recent_inference() {
        let ledger = ledger();
        ledger
            .append_observation(observation(
                "dietary_identity",
                "pescatarian",
                1,
                Explicitness::ExplicitStatement,
            ))
            .await
            .unwrap();
        // A later *inference* must not displace what the user actually said.
        ledger
            .append_observation(observation(
                "dietary_identity",
                "vegan",
                6,
                Explicitness::WeakInference,
            ))
            .await
            .unwrap();

        ledger.micro_reconcile();
        let usable = ledger.usable_candidates();
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].canonical_statement, "The user is pescatarian.");
    }

    #[tokio::test]
    async fn repeated_inference_earns_staging_but_not_more() {
        let ledger = ledger();
        for turn in 1..=2 {
            ledger
                .append_observation(observation(
                    "exercise_routine",
                    "morning gym",
                    turn,
                    Explicitness::StrongImplication,
                ))
                .await
                .unwrap();
        }
        let report = ledger.micro_reconcile();
        assert_eq!(report.staged, 1);
        assert_eq!(
            ledger.usable_candidates()[0].status,
            SessionCandidateStatus::StagedForReconciliation
        );
    }

    #[tokio::test]
    async fn sealing_drops_suppressed_and_rejected_candidates_and_stops_writes() {
        let ledger = ledger();
        ledger
            .append_observation(observation(
                "dietary_identity",
                "vegetarian",
                1,
                Explicitness::ExplicitStatement,
            ))
            .await
            .unwrap();
        ledger
            .append_observation(observation(
                "dietary_identity",
                "pescatarian",
                5,
                Explicitness::ExplicitStatement,
            ))
            .await
            .unwrap();
        ledger.micro_reconcile();

        let sealed = ledger.seal().await.unwrap();
        assert_eq!(sealed.candidates.len(), 1);

        let err = ledger
            .append_observation(observation(
                "dietary_identity",
                "vegan",
                9,
                Explicitness::ExplicitStatement,
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::PolicyRefused(_)));
    }

    #[tokio::test]
    async fn confidence_aggregates_rather_than_accumulates() {
        let ledger = ledger();
        for turn in 1..=4 {
            ledger
                .append_observation(observation(
                    "beverage_preference",
                    "flat white",
                    turn,
                    Explicitness::ExplicitStatement,
                ))
                .await
                .unwrap();
        }
        let candidate = &ledger.usable_candidates()[0];
        assert!(candidate.confidence <= 0.95);
        assert!(candidate.confidence >= 0.9);
    }
}
