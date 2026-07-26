//! Committing resolved mutations to canonical memory.
//!
//! One transaction per session. A contradiction resolution writes both the new
//! active record and the retired one; committing only half would leave the
//! corpus asserting two incompatible facts, so the unit of commit is the whole
//! reconciliation, not the individual record.

use chrono::Utc;
use std::sync::Arc;

use super::consolidate::ConsolidationOutput;
use super::proposal::{MemorySelector, ResolutionKind, ResolvedMutation};
use super::resolver::Resolver;
use crate::core::{
    stable_hash, CommitReceipt, MemoryError, MemoryEvent, MemoryStatus, SessionEventWriter, UserId,
};
use crate::okf::{MemoryRepository, MemoryTransaction, ReconciliationSelector};

/// Turns proposals into durable memory.
pub struct MemoryCommitter {
    repository: Arc<dyn MemoryRepository>,
    events: Option<SessionEventWriter>,
}

/// What one reconciliation did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconciliationReport {
    /// Count of each resolution kind, for metrics.
    pub creates: usize,
    /// Records reinforced.
    pub reinforces: usize,
    /// Records refined.
    pub refines: usize,
    /// Records superseded.
    pub supersedes: usize,
    /// Records staged.
    pub stages: usize,
    /// Records deleted.
    pub deletes: usize,
    /// Proposals refused.
    pub discards: usize,
    /// Repository revision after the commit.
    pub revision: u64,
}

impl ReconciliationReport {
    fn record(&mut self, kind: ResolutionKind) {
        match kind {
            ResolutionKind::Create => self.creates += 1,
            ResolutionKind::Reinforce => self.reinforces += 1,
            ResolutionKind::Refine => self.refines += 1,
            ResolutionKind::Supersede => self.supersedes += 1,
            ResolutionKind::Coexist => self.creates += 1,
            ResolutionKind::Stage => self.stages += 1,
            ResolutionKind::Delete => self.deletes += 1,
            ResolutionKind::Discard => self.discards += 1,
        }
    }

    /// Whether anything at all changed.
    pub fn is_empty(&self) -> bool {
        self.creates + self.reinforces + self.refines + self.supersedes + self.stages + self.deletes
            == 0
    }
}

impl MemoryCommitter {
    /// A committer writing to `repository`.
    pub fn new(repository: Arc<dyn MemoryRepository>) -> Self {
        Self {
            repository,
            events: None,
        }
    }

    /// Emit audit events alongside every commit.
    pub fn with_events(mut self, events: SessionEventWriter) -> Self {
        self.events = Some(events);
        self
    }

    /// Resolve and commit a session's consolidation output.
    pub async fn reconcile(
        &self,
        owner: &UserId,
        output: ConsolidationOutput,
        idempotency_key: &str,
    ) -> Result<ReconciliationReport, MemoryError> {
        let now = Utc::now();
        let resolver = Resolver::new(owner.clone());
        let mut report = ReconciliationReport::default();
        let mut transaction = MemoryTransaction::new(owner.clone(), idempotency_key.to_string());

        for proposal in output.proposals {
            // The candidate window is the subject-and-predicate neighbourhood,
            // not the whole corpus: reconciliation compares a proposal against
            // what could plausibly be the same fact, and nothing else.
            let window = self
                .repository
                .find_candidates(
                    owner,
                    &ReconciliationSelector::by_subject_predicate(
                        proposal.fingerprint.subject_predicate().to_string(),
                    ),
                )
                .await?;

            let resolved = resolver.resolve(proposal, &window, now);
            report.record(resolved.kind);
            self.emit(&resolved).await;
            transaction = apply(transaction, resolved);
        }

        for selector in &output.deletions {
            let targets = self.resolve_deletion(owner, selector).await?;
            for id in targets {
                report.deletes += 1;
                if let Some(events) = &self.events {
                    let _ = events
                        .append(
                            None,
                            MemoryEvent::MemoryDeleted {
                                memory_id: id.clone(),
                            },
                        )
                        .await;
                }
                transaction = transaction.delete(id);
            }
        }

        if transaction.is_empty() {
            return Ok(report);
        }

        let receipt = self.repository.commit(transaction).await?;
        report.revision = receipt.revision;
        Ok(report)
    }

    /// Commit a single resolved mutation, for explicit in-session commands.
    pub async fn commit_one(
        &self,
        owner: &UserId,
        resolved: ResolvedMutation,
    ) -> Result<CommitReceipt, MemoryError> {
        let key = stable_hash(&format!("{}|{:?}", resolved.fingerprint, resolved.kind));
        self.emit(&resolved).await;
        let transaction = apply(MemoryTransaction::new(owner.clone(), key), resolved);
        self.repository.commit(transaction).await
    }

    /// Expand a deletion selector into concrete record ids.
    async fn resolve_deletion(
        &self,
        owner: &UserId,
        selector: &MemorySelector,
    ) -> Result<Vec<crate::core::MemoryId>, MemoryError> {
        // Deletion reaches every status, not only active records: a user who
        // asks to forget something means the superseded copies too.
        let all = self.repository.all(owner).await?;
        Ok(all
            .iter()
            .filter(|m| selector.matches(m))
            .map(|m| m.id.clone())
            .collect())
    }

    async fn emit(&self, resolved: &ResolvedMutation) {
        let Some(events) = &self.events else { return };
        let event = match resolved.kind {
            ResolutionKind::Discard => MemoryEvent::MutationRejected {
                fingerprint: resolved.fingerprint.clone(),
                reason: resolved
                    .discard_reason
                    .unwrap_or(crate::core::DiscardReason::InsufficientEvidence),
            },
            kind => match resolved.writes.first() {
                Some(memory) => MemoryEvent::MutationCommitted {
                    memory_id: memory.id.clone(),
                    kind: kind.label().to_string(),
                },
                None => MemoryEvent::MutationProposed {
                    fingerprint: resolved.fingerprint.clone(),
                    kind: kind.label().to_string(),
                },
            },
        };
        let _ = events.append(None, event).await;

        if resolved.kind == ResolutionKind::Supersede || resolved.kind == ResolutionKind::Refine {
            if let (Some(new), Some(old)) = (resolved.writes.first(), resolved.writes.get(1)) {
                let _ = events
                    .append(
                        None,
                        MemoryEvent::MemorySuperseded {
                            old: old.id.clone(),
                            new: new.id.clone(),
                        },
                    )
                    .await;
            }
        }
    }
}

fn apply(mut transaction: MemoryTransaction, resolved: ResolvedMutation) -> MemoryTransaction {
    for memory in resolved.writes {
        transaction = transaction.put(memory);
    }
    for id in resolved.deletes {
        transaction = transaction.delete(id);
    }
    transaction
}

/// Apply a promotion sweep's outcomes to the repository.
pub async fn commit_promotions(
    repository: &Arc<dyn MemoryRepository>,
    owner: &UserId,
    outcomes: Vec<super::promotion::PromotionOutcome>,
    idempotency_key: &str,
) -> Result<CommitReceipt, MemoryError> {
    use super::promotion::PromotionOutcome;

    let mut transaction = MemoryTransaction::new(owner.clone(), idempotency_key.to_string());
    for outcome in outcomes {
        match outcome {
            PromotionOutcome::Promote(memory) => transaction = transaction.put(*memory),
            PromotionOutcome::Expire(memory) => {
                let mut expired = *memory;
                expired.status = MemoryStatus::Expired;
                expired.temporal.updated_at = Utc::now();
                transaction = transaction.put(expired);
            }
            PromotionOutcome::Hold { .. } => {}
        }
    }
    repository.commit(transaction).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        Explicitness, InMemoryEventLog, IngestionConfig, MemoryEventLog, MemoryKind,
        MemoryObservation, MemoryValue, ObservationId, ProposedPersistence, SensitivityClass,
        SessionId, SpeakerAttribution, TemporalScope, TranscriptEvidence, TurnId,
    };
    use crate::ingestion::{InMemorySessionLedger, SessionLedger};
    use crate::okf::OkfRepository;
    use crate::reconcile::consolidate::consolidate;

    fn observation(
        predicate: &str,
        value: &str,
        turn: u64,
        intent: Option<crate::core::MutationIntent>,
    ) -> MemoryObservation {
        MemoryObservation {
            observation_id: ObservationId::generate(),
            session_id: SessionId::new("ses_1"),
            turn_id: TurnId(turn),
            subject: crate::core::EntityRef::user(),
            predicate: crate::core::CanonicalPredicate::new(predicate),
            value: MemoryValue::Text(value.to_string()),
            canonical_statement: format!("The user is {value}."),
            kind: MemoryKind::Preference,
            explicitness: Explicitness::ExplicitStatement,
            confidence: 0.9,
            persistence: ProposedPersistence::Durable,
            temporal_scope: TemporalScope::Persistent,
            valid_from: None,
            expected_expiry: None,
            transcript_evidence: TranscriptEvidence::new(format!("I am {value}")),
            speaker_attribution: SpeakerAttribution::User,
            sensitivity: SensitivityClass::Normal,
            mutation_intent: intent,
        }
    }

    async fn run_session(
        committer: &MemoryCommitter,
        owner: &UserId,
        session: &str,
        observations: Vec<MemoryObservation>,
    ) -> ReconciliationReport {
        let ledger =
            InMemorySessionLedger::new(SessionId::new(session), IngestionConfig::default());
        for obs in observations {
            ledger.append_observation(obs).await.unwrap();
        }
        ledger.micro_reconcile();
        let sealed = ledger.seal().await.unwrap();
        committer
            .reconcile(owner, consolidate(&sealed), session)
            .await
            .unwrap()
    }

    fn setup() -> (Arc<dyn MemoryRepository>, MemoryCommitter, UserId) {
        let repository: Arc<dyn MemoryRepository> = Arc::new(OkfRepository::in_memory());
        let committer = MemoryCommitter::new(repository.clone());
        (repository, committer, UserId::new("usr_1"))
    }

    #[tokio::test]
    async fn a_first_session_creates_the_record() {
        let (repository, committer, owner) = setup();
        let report = run_session(
            &committer,
            &owner,
            "ses_1",
            vec![observation("dietary_identity", "pescatarian", 1, None)],
        )
        .await;

        assert_eq!(report.creates, 1);
        let stored = repository.all(&owner).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].statement, "The user is pescatarian.");
    }

    #[tokio::test]
    async fn the_same_fact_in_a_later_session_reinforces_rather_than_duplicating() {
        let (repository, committer, owner) = setup();
        run_session(
            &committer,
            &owner,
            "ses_1",
            vec![observation("dietary_identity", "pescatarian", 1, None)],
        )
        .await;
        let report = run_session(
            &committer,
            &owner,
            "ses_2",
            vec![observation("dietary_identity", "pescatarian", 1, None)],
        )
        .await;

        assert_eq!(report.reinforces, 1);
        assert_eq!(report.creates, 0);
        let stored = repository.all(&owner).await.unwrap();
        assert_eq!(stored.len(), 1, "no duplicate record");
        assert!(stored[0].evidence.count >= 2);
        assert_eq!(stored[0].evidence.distinct_sessions, 2);
    }

    #[tokio::test]
    async fn a_correction_in_a_later_session_supersedes_the_old_record() {
        let (repository, committer, owner) = setup();
        run_session(
            &committer,
            &owner,
            "ses_1",
            vec![observation("dietary_identity", "vegetarian", 1, None)],
        )
        .await;
        let report = run_session(
            &committer,
            &owner,
            "ses_2",
            vec![observation("dietary_identity", "pescatarian", 1, None)],
        )
        .await;

        assert_eq!(report.supersedes, 1);
        let stored = repository.all(&owner).await.unwrap();
        let active: Vec<_> = stored
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].statement, "The user is pescatarian.");

        let retired: Vec<_> = stored
            .iter()
            .filter(|m| m.status == MemoryStatus::Superseded)
            .collect();
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].superseded_by.as_ref(), Some(&active[0].id));
    }

    #[tokio::test]
    async fn a_forget_command_removes_the_record_and_its_superseded_copies() {
        let (repository, committer, owner) = setup();
        run_session(
            &committer,
            &owner,
            "ses_1",
            vec![observation("dietary_identity", "vegetarian", 1, None)],
        )
        .await;
        run_session(
            &committer,
            &owner,
            "ses_2",
            vec![observation("dietary_identity", "pescatarian", 1, None)],
        )
        .await;
        assert_eq!(repository.all(&owner).await.unwrap().len(), 2);

        let report = run_session(
            &committer,
            &owner,
            "ses_3",
            vec![observation(
                "memory_removal",
                "pescatarian",
                1,
                Some(crate::core::MutationIntent::Forget),
            )],
        )
        .await;

        assert_eq!(report.deletes, 1, "the superseded copy mentions vegetarian");
        let remaining = repository.all(&owner).await.unwrap();
        assert!(
            remaining
                .iter()
                .all(|m| !m.statement.contains("pescatarian")),
            "deleted content is gone: {remaining:?}"
        );
    }

    #[tokio::test]
    async fn a_retried_reconciliation_does_not_double_write() {
        let (repository, committer, owner) = setup();
        let ledger =
            InMemorySessionLedger::new(SessionId::new("ses_1"), IngestionConfig::default());
        ledger
            .append_observation(observation("dietary_identity", "pescatarian", 1, None))
            .await
            .unwrap();
        let sealed = ledger.seal().await.unwrap();

        committer
            .reconcile(&owner, consolidate(&sealed), "same-key")
            .await
            .unwrap();
        committer
            .reconcile(&owner, consolidate(&sealed), "same-key")
            .await
            .unwrap();

        assert_eq!(repository.all(&owner).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_session_that_proposed_nothing_commits_nothing() {
        let (repository, committer, owner) = setup();
        let report = run_session(&committer, &owner, "ses_1", Vec::new()).await;
        assert!(report.is_empty());
        assert_eq!(repository.revision(&owner).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn commits_are_audited() {
        let log = Arc::new(InMemoryEventLog::new());
        let repository: Arc<dyn MemoryRepository> = Arc::new(OkfRepository::in_memory());
        let owner = UserId::new("usr_1");
        let committer = MemoryCommitter::new(repository).with_events(SessionEventWriter::new(
            log.clone() as Arc<dyn MemoryEventLog>,
            owner.clone(),
            SessionId::new("ses_1"),
        ));

        run_session(
            &committer,
            &owner,
            "ses_1",
            vec![observation("dietary_identity", "vegetarian", 1, None)],
        )
        .await;
        run_session(
            &committer,
            &owner,
            "ses_2",
            vec![observation("dietary_identity", "pescatarian", 1, None)],
        )
        .await;

        assert!(log.count_label("mutation_committed") >= 2);
        assert_eq!(log.count_label("memory_superseded"), 1);
    }
}
