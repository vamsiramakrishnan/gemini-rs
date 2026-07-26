//! The session overlay — facts the user stated moments ago, usable now.
//!
//! Waiting for post-session reconciliation before a fact becomes retrievable
//! would mean B forgets what it was told thirty seconds ago, which is the exact
//! failure the whole system exists to avoid. The overlay closes that gap: it is
//! searched alongside canonical memory, ranked above it, and presented hedged
//! because it has not yet been reconciled.

use chrono::{DateTime, Utc};

use super::ledger::{SessionCandidate, SessionCandidateStatus};
use crate::bm25::{IndexedMemory, MemoryIndex};
use crate::core::{
    stable_hash, CanonicalMemory, EvidenceCounters, MemoryId, MemorySource, MemoryStatus,
    MutationIntent, PrivacyMetadata, RetrievalMetadata, SensitivityClass, SessionId,
    TemporalMetadata, UserId,
};

/// The searchable projection of the session ledger.
#[derive(Debug, Default)]
pub struct SessionMemoryOverlay {
    revision: u64,
    index: MemoryIndex,
}

impl SessionMemoryOverlay {
    /// An empty overlay.
    pub fn new() -> Self {
        Self::default()
    }

    /// Overlay revision, bumped on every rebuild that changed something.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The searchable index.
    pub fn index(&self) -> &MemoryIndex {
        &self.index
    }

    /// How many facts the overlay holds.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the overlay is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Rebuild from the ledger's usable candidates.
    ///
    /// Rebuilding wholesale rather than patching is what makes suppression
    /// work: a candidate contradicted this turn simply stops being produced,
    /// and cannot linger in the index asserting the old value.
    pub fn rebuild(
        &mut self,
        owner: &UserId,
        session_id: &SessionId,
        candidates: &[SessionCandidate],
        now: DateTime<Utc>,
    ) -> usize {
        let mut index = MemoryIndex::new();
        for candidate in candidates {
            if !candidate.status.is_usable() {
                continue;
            }
            if candidate.status == SessionCandidateStatus::Suppressed {
                continue;
            }
            // "forget that", "what do you remember" and friends are commands
            // *about* memory. They belong in the ledger so consolidation can
            // act on them, but they are not facts about the user and must
            // never be handed back as recalled context.
            if matches!(
                candidate.mutation_intent,
                Some(MutationIntent::List)
                    | Some(MutationIntent::Forget)
                    | Some(MutationIntent::Delete)
            ) {
                continue;
            }
            let provisional = provisional_memory(candidate, owner, session_id, now);
            index.upsert(IndexedMemory::from_canonical(&provisional).as_session_overlay());
        }
        let changed = index.len() != self.index.len();
        self.index = index;
        if changed {
            self.revision += 1;
        } else {
            // Content may have changed even when the count did not; the
            // revision is a cache key, so err toward invalidating.
            self.revision += 1;
        }
        self.index.len()
    }

    /// Drop every overlay fact, e.g. when a logical session ends.
    pub fn clear(&mut self) {
        self.index = MemoryIndex::new();
        self.revision += 1;
    }
}

/// A stable synthetic id for an uncommitted session fact.
///
/// Derived from the fingerprint so the same fact keeps the same id across
/// rebuilds, and prefixed so it can never be mistaken for a canonical record.
pub fn provisional_memory_id(candidate: &SessionCandidate) -> MemoryId {
    MemoryId::new(format!(
        "session_{}",
        stable_hash(candidate.fingerprint.as_str())
    ))
}

/// Project a session candidate as a provisional memory record.
pub fn provisional_memory(
    candidate: &SessionCandidate,
    owner: &UserId,
    session_id: &SessionId,
    now: DateTime<Utc>,
) -> CanonicalMemory {
    let mut temporal = TemporalMetadata::created_at(now);
    temporal.expires_at = candidate.expected_expiry;

    CanonicalMemory {
        id: provisional_memory_id(candidate),
        owner: owner.clone(),
        kind: candidate.kind,
        predicate: candidate.predicate.clone(),
        status: MemoryStatus::Staged,
        confidence: candidate.confidence,
        subject: candidate.subject.clone(),
        value: candidate.value.clone(),
        statement: candidate.canonical_statement.clone(),
        evidence_summary: format!(
            "Observed {} time(s) in the current session.",
            candidate.evidence.len()
        ),
        source: MemorySource::from_explicitness(
            candidate.explicitness,
            session_id.clone(),
            candidate.last_seen_turn,
        ),
        temporal,
        retrieval: RetrievalMetadata {
            subject: crate::core::normalize_token(&candidate.subject.display),
            tags: derive_tags(candidate),
            aliases: Vec::new(),
            entities: candidate.subject.surface_forms(),
            location: None,
        },
        evidence: EvidenceCounters {
            count: candidate.evidence.len() as u32,
            distinct_sessions: 1,
            distinct_days: candidate.distinct_days().max(1),
        },
        privacy: PrivacyMetadata {
            deletable: true,
            exportable: true,
            sensitivity: SensitivityClass::Normal,
        },
        temporal_scope: candidate.temporal_scope,
        supersedes: Vec::new(),
        superseded_by: None,
        qualifier: None,
    }
}

/// Tags for a provisional record: the predicate's parts plus the value's terms.
fn derive_tags(candidate: &SessionCandidate) -> Vec<String> {
    let mut tags: Vec<String> = candidate
        .predicate
        .as_str()
        .split('_')
        .map(str::to_string)
        .collect();
    tags.extend(crate::bm25::tokenize(&candidate.value.display()));
    for term in &candidate.search_terms {
        tags.extend(crate::bm25::tokenize(term));
    }
    tags.retain(|t| !t.is_empty());
    tags.sort();
    tags.dedup();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::{MemoryOrigin, Query};
    use crate::core::{
        CanonicalPredicate, EntityRef, Explicitness, IngestionConfig, MemoryKind,
        MemoryObservation, MemoryValue, ObservationId, ProposedPersistence, SpeakerAttribution,
        TemporalScope, TranscriptEvidence, TurnId,
    };
    use crate::ingestion::ledger::{InMemorySessionLedger, SessionLedger};

    fn observation(predicate: &str, value: &str, turn: u64) -> MemoryObservation {
        MemoryObservation {
            observation_id: ObservationId::generate(),
            session_id: SessionId::new("ses_1"),
            turn_id: TurnId(turn),
            subject: EntityRef::user(),
            predicate: CanonicalPredicate::new(predicate),
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
            mutation_intent: None,
            search_terms: Vec::new(),
        }
    }

    async fn overlay_from(observations: Vec<MemoryObservation>) -> SessionMemoryOverlay {
        let ledger =
            InMemorySessionLedger::new(SessionId::new("ses_1"), IngestionConfig::default());
        for obs in observations {
            ledger.append_observation(obs).await.unwrap();
        }
        ledger.micro_reconcile();

        let mut overlay = SessionMemoryOverlay::new();
        overlay.rebuild(
            &UserId::new("usr_1"),
            &SessionId::new("ses_1"),
            &ledger.usable_candidates(),
            Utc::now(),
        );
        overlay
    }

    #[tokio::test]
    async fn a_fact_stated_this_turn_is_searchable_immediately() {
        let overlay = overlay_from(vec![observation("dietary_identity", "pescatarian", 1)]).await;
        let hits = overlay
            .index()
            .search(&Query::new("pescatarian"), Utc::now());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].origin, MemoryOrigin::SessionOverlay);
    }

    #[tokio::test]
    async fn a_suppressed_candidate_disappears_from_the_overlay() {
        let overlay = overlay_from(vec![
            observation("dietary_identity", "vegetarian", 1),
            observation("dietary_identity", "pescatarian", 5),
        ])
        .await;

        assert_eq!(overlay.len(), 1);
        assert!(overlay
            .index()
            .search(&Query::new("vegetarian"), Utc::now())
            .is_empty());
        assert!(!overlay
            .index()
            .search(&Query::new("pescatarian"), Utc::now())
            .is_empty());
    }

    #[tokio::test]
    async fn provisional_ids_are_stable_across_rebuilds() {
        let first = overlay_from(vec![observation("dietary_identity", "pescatarian", 1)]).await;
        let second = overlay_from(vec![observation("dietary_identity", "pescatarian", 1)]).await;

        let id_of = |o: &SessionMemoryOverlay| {
            o.index().search(&Query::new("pescatarian"), Utc::now())[0]
                .id
                .clone()
        };
        assert_eq!(id_of(&first), id_of(&second));
        assert!(id_of(&first).as_str().starts_with("session_"));
    }

    #[tokio::test]
    async fn rebuilding_bumps_the_revision_so_caches_invalidate() {
        let mut overlay = SessionMemoryOverlay::new();
        let before = overlay.revision();
        overlay.rebuild(
            &UserId::new("usr_1"),
            &SessionId::new("ses_1"),
            &[],
            Utc::now(),
        );
        assert!(overlay.revision() > before);
    }

    #[tokio::test]
    async fn clearing_empties_the_overlay() {
        let mut overlay =
            overlay_from(vec![observation("dietary_identity", "pescatarian", 1)]).await;
        overlay.clear();
        assert!(overlay.is_empty());
    }

    #[tokio::test]
    async fn a_provisional_record_is_staged_not_active() {
        let ledger =
            InMemorySessionLedger::new(SessionId::new("ses_1"), IngestionConfig::default());
        ledger
            .append_observation(observation("dietary_identity", "pescatarian", 1))
            .await
            .unwrap();
        let candidate = &ledger.usable_candidates()[0];
        let provisional = provisional_memory(
            candidate,
            &UserId::new("usr_1"),
            &SessionId::new("ses_1"),
            Utc::now(),
        );
        assert_eq!(provisional.status, MemoryStatus::Staged);
        assert!(provisional
            .retrieval
            .tags
            .contains(&"pescatarian".to_string()));
    }
}
