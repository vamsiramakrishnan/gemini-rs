//! The append-only memory event log — the recovery and audit backbone.
//!
//! Everything durable that happens to memory is an event first and a mutation
//! second. A crash mid-session loses the in-process ledger but not the events,
//! so the session overlay can be rebuilt by replay.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::domain::{stable_hash, FactFingerprint, MemoryObservation, MutationIntent};
use super::error::MemoryError;
use super::ids::{EventId, MemoryId, SessionId, TurnId, UserId};
use super::policy::DiscardReason;

/// A durable fact about something that happened to memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MemoryEvent {
    /// A finalized user utterance was accepted as evidence.
    FinalTranscriptRecorded {
        /// The finalized text.
        text: String,
    },
    /// An observation was extracted from a finalized utterance.
    ObservationExtracted {
        /// The observation.
        observation: Box<MemoryObservation>,
    },
    /// An observation was refused by policy.
    ObservationRejected {
        /// Fingerprint of the refused candidate, for auditing.
        fingerprint: FactFingerprint,
        /// Why it was refused.
        reason: DiscardReason,
    },
    /// Two observations were recognised as the same candidate.
    SessionCandidateMerged {
        /// The surviving fingerprint.
        fingerprint: FactFingerprint,
        /// Evidence count after the merge.
        evidence_count: u32,
    },
    /// The in-session overlay changed.
    SessionOverlayUpdated {
        /// Overlay revision after the change.
        revision: u64,
    },
    /// The user issued an explicit memory command.
    ExplicitMutationRequested {
        /// What they asked for.
        intent: MutationIntent,
        /// The statement they gave, verbatim.
        statement: String,
    },
    /// A long session reached a checkpoint.
    SessionCheckpointed {
        /// Turns completed at checkpoint time.
        turns: u64,
    },
    /// A logical session was sealed and accepts no further writes.
    SessionSealed {
        /// Candidates carried into consolidation.
        candidate_count: usize,
    },
    /// Consolidation proposed a mutation.
    MutationProposed {
        /// Fingerprint of the proposal.
        fingerprint: FactFingerprint,
        /// The resolution kind, as a stable label.
        kind: String,
    },
    /// A mutation was committed to canonical memory.
    MutationCommitted {
        /// The affected record.
        memory_id: MemoryId,
        /// The resolution kind, as a stable label.
        kind: String,
    },
    /// A proposal was refused at commit time.
    MutationRejected {
        /// Fingerprint of the refused proposal.
        fingerprint: FactFingerprint,
        /// Why it was refused.
        reason: DiscardReason,
    },
    /// An active record was replaced.
    MemorySuperseded {
        /// The record that was replaced.
        old: MemoryId,
        /// The record that replaced it.
        new: MemoryId,
    },
    /// A record was deleted at the user's request.
    MemoryDeleted {
        /// The removed record.
        memory_id: MemoryId,
    },
    /// A new retrieval index revision was published.
    IndexRevisionPublished {
        /// The revision that is now serving.
        revision: u64,
    },
}

impl MemoryEvent {
    /// A stable label for metrics and log filtering.
    pub fn label(&self) -> &'static str {
        match self {
            Self::FinalTranscriptRecorded { .. } => "final_transcript_recorded",
            Self::ObservationExtracted { .. } => "observation_extracted",
            Self::ObservationRejected { .. } => "observation_rejected",
            Self::SessionCandidateMerged { .. } => "session_candidate_merged",
            Self::SessionOverlayUpdated { .. } => "session_overlay_updated",
            Self::ExplicitMutationRequested { .. } => "explicit_mutation_requested",
            Self::SessionCheckpointed { .. } => "session_checkpointed",
            Self::SessionSealed { .. } => "session_sealed",
            Self::MutationProposed { .. } => "mutation_proposed",
            Self::MutationCommitted { .. } => "mutation_committed",
            Self::MutationRejected { .. } => "mutation_rejected",
            Self::MemorySuperseded { .. } => "memory_superseded",
            Self::MemoryDeleted { .. } => "memory_deleted",
            Self::IndexRevisionPublished { .. } => "index_revision_published",
        }
    }
}

/// The current event schema version. Bumped whenever [`MemoryEvent`] changes
/// shape in a way replay must account for.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// An event plus the addressing metadata every consumer needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEventEnvelope {
    /// Identifier for this envelope.
    pub event_id: EventId,
    /// When it occurred.
    pub occurred_at: DateTime<Utc>,
    /// Whose memory it concerns.
    pub user_id: UserId,
    /// The logical session it belongs to.
    pub logical_session_id: SessionId,
    /// The turn it belongs to, when applicable.
    pub turn_id: Option<TurnId>,
    /// Deduplication key for at-least-once delivery.
    pub idempotency_key: String,
    /// Schema version of the payload.
    pub schema_version: u32,
    /// The event itself.
    pub payload: MemoryEvent,
}

impl MemoryEventEnvelope {
    /// Wrap an event for a user and session.
    ///
    /// The idempotency key is derived from the addressing tuple and the payload
    /// so a retried append is recognised as a duplicate rather than duplicated.
    pub fn new(
        user_id: UserId,
        logical_session_id: SessionId,
        turn_id: Option<TurnId>,
        payload: MemoryEvent,
        now: DateTime<Utc>,
    ) -> Self {
        let payload_repr = serde_json::to_string(&payload).unwrap_or_default();
        let idempotency_key = stable_hash(&format!(
            "{user_id}|{logical_session_id}|{}|{}",
            turn_id.map(|t| t.0).unwrap_or_default(),
            payload_repr
        ));
        Self {
            event_id: EventId::generate(),
            occurred_at: now,
            user_id,
            logical_session_id,
            turn_id,
            idempotency_key,
            schema_version: EVENT_SCHEMA_VERSION,
            payload,
        }
    }
}

/// A durable append-only sink for memory events.
///
/// The live session worker awaits *acceptance* here — not extraction, not
/// reconciliation. That is the only point where the engine may tell a user
/// their correction was durably recorded.
#[async_trait]
pub trait MemoryEventLog: Send + Sync {
    /// Append an envelope, returning once it is durable.
    async fn append(&self, envelope: MemoryEventEnvelope) -> Result<(), MemoryError>;

    /// Read back every event for a logical session, in append order.
    async fn replay_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<MemoryEventEnvelope>, MemoryError>;
}

/// An in-process event log, used by tests and single-node deployments.
///
/// Deduplicates on `idempotency_key`, so replaying an append is a no-op.
#[derive(Debug, Default)]
pub struct InMemoryEventLog {
    entries: parking_lot::RwLock<Vec<MemoryEventEnvelope>>,
    seen: parking_lot::RwLock<std::collections::HashSet<String>>,
}

impl InMemoryEventLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event appended so far, in order.
    pub fn entries(&self) -> Vec<MemoryEventEnvelope> {
        self.entries.read().clone()
    }

    /// How many events have been appended.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Count of appended events carrying a given label.
    pub fn count_label(&self, label: &str) -> usize {
        self.entries
            .read()
            .iter()
            .filter(|e| e.payload.label() == label)
            .count()
    }
}

#[async_trait]
impl MemoryEventLog for InMemoryEventLog {
    async fn append(&self, envelope: MemoryEventEnvelope) -> Result<(), MemoryError> {
        let mut seen = self.seen.write();
        if !seen.insert(envelope.idempotency_key.clone()) {
            return Ok(());
        }
        drop(seen);
        self.entries.write().push(envelope);
        Ok(())
    }

    async fn replay_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<MemoryEventEnvelope>, MemoryError> {
        Ok(self
            .entries
            .read()
            .iter()
            .filter(|e| &e.logical_session_id == session_id)
            .cloned()
            .collect())
    }
}

/// A convenience wrapper that stamps the user and session onto every append.
#[derive(Clone)]
pub struct SessionEventWriter {
    log: Arc<dyn MemoryEventLog>,
    user_id: UserId,
    session_id: SessionId,
}

impl SessionEventWriter {
    /// Bind a log to one user and logical session.
    pub fn new(log: Arc<dyn MemoryEventLog>, user_id: UserId, session_id: SessionId) -> Self {
        Self {
            log,
            user_id,
            session_id,
        }
    }

    /// Append an event for a specific turn.
    pub async fn append(
        &self,
        turn_id: Option<TurnId>,
        payload: MemoryEvent,
    ) -> Result<(), MemoryError> {
        let envelope = MemoryEventEnvelope::new(
            self.user_id.clone(),
            self.session_id.clone(),
            turn_id,
            payload,
            Utc::now(),
        );
        self.log.append(envelope).await
    }

    /// The bound user.
    pub fn user_id(&self) -> &UserId {
        &self.user_id
    }

    /// The bound logical session.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

/// The result of applying one memory transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitReceipt {
    /// The repository revision after the commit.
    pub revision: u64,
    /// Records created or updated.
    pub written: Vec<MemoryId>,
    /// Records removed.
    pub deleted: Vec<MemoryId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(payload: MemoryEvent) -> MemoryEventEnvelope {
        MemoryEventEnvelope::new(
            UserId::new("usr_1"),
            SessionId::new("ses_1"),
            Some(TurnId(1)),
            payload,
            Utc::now(),
        )
    }

    #[tokio::test]
    async fn appends_are_idempotent_on_replay() {
        let log = InMemoryEventLog::new();
        let e = envelope(MemoryEvent::FinalTranscriptRecorded {
            text: "hello".into(),
        });
        log.append(e.clone()).await.unwrap();
        log.append(e).await.unwrap();
        assert_eq!(log.len(), 1);
    }

    #[tokio::test]
    async fn distinct_payloads_are_distinct_events() {
        let log = InMemoryEventLog::new();
        log.append(envelope(MemoryEvent::FinalTranscriptRecorded {
            text: "a".into(),
        }))
        .await
        .unwrap();
        log.append(envelope(MemoryEvent::FinalTranscriptRecorded {
            text: "b".into(),
        }))
        .await
        .unwrap();
        assert_eq!(log.len(), 2);
    }

    #[tokio::test]
    async fn replay_is_scoped_to_one_session() {
        let log = InMemoryEventLog::new();
        log.append(envelope(MemoryEvent::SessionSealed { candidate_count: 1 }))
            .await
            .unwrap();
        log.append(MemoryEventEnvelope::new(
            UserId::new("usr_1"),
            SessionId::new("ses_other"),
            None,
            MemoryEvent::SessionSealed { candidate_count: 2 },
            Utc::now(),
        ))
        .await
        .unwrap();

        let replayed = log.replay_session(&SessionId::new("ses_1")).await.unwrap();
        assert_eq!(replayed.len(), 1);
    }
}
