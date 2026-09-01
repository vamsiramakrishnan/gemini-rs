//! What consolidation proposes and what resolution decides.
//!
//! The split matters: a model may generate proposals, but only the resolver —
//! deterministic code — turns one into a mutation, and only the committer
//! writes it. A proposal is a request, never an instruction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::{
    CanonicalMemory, CanonicalPredicate, DiscardReason, EntityRef, EvidenceCounters, Explicitness,
    FactFingerprint, MemoryId, MemoryKind, MemorySource, MemoryStatus, MemoryValue, MutationIntent,
    PrivacyMetadata, ProposedPersistence, RetrievalMetadata, SensitivityClass, SessionId,
    TemporalMetadata, TemporalScope, TurnId, UserId, normalize_token,
};

/// A memory consolidation is asking to be allowed to write.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposedMemory {
    /// Deduplication key.
    pub fingerprint: FactFingerprint,
    /// Subject of the fact.
    pub subject: EntityRef,
    /// Canonical predicate.
    pub predicate: CanonicalPredicate,
    /// Value side.
    pub value: MemoryValue,
    /// Natural-language rendering.
    pub statement: String,
    /// Why the engine believes it.
    pub evidence_summary: String,
    /// Proposed kind.
    pub kind: MemoryKind,
    /// Expected persistence class.
    pub temporal_scope: TemporalScope,
    /// Strongest explicitness behind it.
    pub explicitness: Explicitness,
    /// Aggregated confidence.
    pub confidence: f32,
    /// Evidence counters.
    pub evidence: EvidenceCounters,
    /// Retention proposal.
    pub persistence: ProposedPersistence,
    /// Expiry for episodic proposals.
    pub expected_expiry: Option<DateTime<Utc>>,
    /// Explicit command behind it, if any.
    pub mutation_intent: Option<MutationIntent>,
    /// Privacy classification.
    pub sensitivity: SensitivityClass,
    /// Context qualifier distinguishing coexisting facts.
    pub qualifier: Option<String>,
    /// Session the evidence came from.
    pub session_id: SessionId,
    /// Last turn the evidence came from.
    pub turn_id: TurnId,
    /// Search tags derived from the evidence.
    pub tags: Vec<String>,
}

impl ProposedMemory {
    /// Materialize the proposal as a canonical record owned by `owner`.
    pub fn into_canonical(
        self,
        owner: &UserId,
        id: MemoryId,
        status: MemoryStatus,
        now: DateTime<Utc>,
    ) -> CanonicalMemory {
        let mut temporal = TemporalMetadata::created_at(now);
        temporal.expires_at =
            crate::core::resolve_expiry(self.kind, self.temporal_scope, self.expected_expiry, now);

        CanonicalMemory {
            id,
            owner: owner.clone(),
            kind: self.kind,
            predicate: self.predicate.clone(),
            status,
            confidence: self.confidence,
            subject: self.subject.clone(),
            value: self.value,
            statement: self.statement,
            evidence_summary: self.evidence_summary,
            source: MemorySource::from_explicitness(
                self.explicitness,
                self.session_id,
                self.turn_id,
            ),
            temporal,
            retrieval: RetrievalMetadata {
                subject: normalize_token(&self.subject.display),
                tags: self.tags,
                aliases: Vec::new(),
                entities: self.subject.surface_forms(),
                location: None,
            },
            evidence: self.evidence,
            privacy: PrivacyMetadata {
                deletable: true,
                exportable: true,
                sensitivity: self.sensitivity,
            },
            temporal_scope: self.temporal_scope,
            supersedes: Vec::new(),
            superseded_by: None,
            qualifier: self.qualifier,
        }
    }
}

/// How to identify records the user asked to remove.
#[derive(Debug, Clone, PartialEq)]
pub enum MemorySelector {
    /// A specific record.
    ById(MemoryId),
    /// Everything asserted about a subject and predicate.
    BySubjectPredicate(String),
    /// Everything whose statement mentions a topic.
    ByTopic(String),
}

impl MemorySelector {
    /// Whether a record is targeted by this selector.
    pub fn matches(&self, memory: &CanonicalMemory) -> bool {
        match self {
            Self::ById(id) => &memory.id == id,
            Self::BySubjectPredicate(prefix) => {
                memory.fingerprint().subject_predicate() == prefix.as_str()
            }
            Self::ByTopic(topic) => {
                // Word-sequence matching, not substring: deletion is
                // irreversible, topics are often a single short word, and
                // `contains` would let "forget art" delete a memory about a
                // shopping cart.
                let needle = normalize_token(topic);
                !needle.is_empty()
                    && (contains_word_sequence(&normalize_token(&memory.statement), &needle)
                        || memory
                            .retrieval
                            .tags
                            .iter()
                            .any(|t| normalize_token(t) == needle))
            }
        }
    }
}

/// Whether `needle` occurs in `haystack` as a whole word sequence.
///
/// Both arguments must already be normalized.
fn contains_word_sequence(haystack: &str, needle: &str) -> bool {
    let hay: Vec<&str> = haystack.split_whitespace().collect();
    let ned: Vec<&str> = needle.split_whitespace().collect();
    if ned.is_empty() || ned.len() > hay.len() {
        return false;
    }
    hay.windows(ned.len()).any(|w| w == ned.as_slice())
}

/// What resolution decided to do with a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionKind {
    /// No equivalent record existed.
    Create,
    /// An equivalent active record existed; strengthen it.
    Reinforce,
    /// New evidence is more precise but compatible.
    Refine,
    /// Newer explicit evidence contradicts the active record.
    Supersede,
    /// The statements differ by context and both hold.
    Coexist,
    /// Evidence is insufficient; hold for reinforcement.
    Stage,
    /// The user asked for removal.
    Delete,
    /// Refused.
    Discard,
}

impl ResolutionKind {
    /// A stable label for events and metrics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Reinforce => "reinforce",
            Self::Refine => "refine",
            Self::Supersede => "supersede",
            Self::Coexist => "coexist",
            Self::Stage => "stage",
            Self::Delete => "delete",
            Self::Discard => "discard",
        }
    }

    /// Whether this outcome writes anything durable.
    pub fn is_write(self) -> bool {
        !matches!(self, Self::Discard)
    }
}

/// A resolved mutation, ready to be committed.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMutation {
    /// What was decided.
    pub kind: ResolutionKind,
    /// The proposal's fingerprint, for auditing.
    pub fingerprint: FactFingerprint,
    /// Records to write, in order.
    pub writes: Vec<CanonicalMemory>,
    /// Records to remove.
    pub deletes: Vec<MemoryId>,
    /// Why, when the outcome was a refusal.
    pub discard_reason: Option<DiscardReason>,
}

impl ResolvedMutation {
    /// A refusal.
    pub fn discard(fingerprint: FactFingerprint, reason: DiscardReason) -> Self {
        Self {
            kind: ResolutionKind::Discard,
            fingerprint,
            writes: Vec::new(),
            deletes: Vec::new(),
            discard_reason: Some(reason),
        }
    }

    /// A single-record write.
    pub fn write(
        kind: ResolutionKind,
        fingerprint: FactFingerprint,
        memory: CanonicalMemory,
    ) -> Self {
        Self {
            kind,
            fingerprint,
            writes: vec![memory],
            deletes: Vec::new(),
            discard_reason: None,
        }
    }

    /// Whether anything will actually be written or removed.
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.deletes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: &str, statement: &str, tags: &[&str]) -> CanonicalMemory {
        let now = Utc::now();
        CanonicalMemory {
            id: MemoryId::new(id),
            owner: UserId::new("usr_1"),
            kind: MemoryKind::Preference,
            predicate: CanonicalPredicate::new("dietary_identity"),
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
            temporal: TemporalMetadata::created_at(now),
            retrieval: RetrievalMetadata {
                subject: "user".into(),
                tags: tags.iter().map(|t| (*t).to_string()).collect(),
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

    #[test]
    fn selectors_target_what_they_name_and_nothing_else() {
        let record = memory("mem_a", "The user is pescatarian.", &["diet"]);

        assert!(MemorySelector::ById(MemoryId::new("mem_a")).matches(&record));
        assert!(!MemorySelector::ById(MemoryId::new("mem_b")).matches(&record));

        assert!(
            MemorySelector::BySubjectPredicate("user|dietary_identity".into()).matches(&record)
        );
        assert!(!MemorySelector::BySubjectPredicate("user|coffee_order".into()).matches(&record));

        assert!(MemorySelector::ByTopic("pescatarian".into()).matches(&record));
        assert!(MemorySelector::ByTopic("diet".into()).matches(&record));
        assert!(!MemorySelector::ByTopic("cycling".into()).matches(&record));
    }

    #[test]
    fn a_topic_matches_whole_words_only() {
        // "forget art" must not delete a memory about a shopping cart.
        let cart = memory("mem_cart", "The user left items in the cart.", &[]);
        assert!(!MemorySelector::ByTopic("art".into()).matches(&cart));
        assert!(MemorySelector::ByTopic("cart".into()).matches(&cart));

        // Multi-word topics still match as a sequence.
        let dinner = memory("mem_d", "The user enjoyed the quiet dinner in Bandra.", &[]);
        assert!(MemorySelector::ByTopic("quiet dinner".into()).matches(&dinner));
        assert!(!MemorySelector::ByTopic("dinner quiet".into()).matches(&dinner));
    }

    #[test]
    fn an_empty_topic_selector_matches_nothing() {
        let record = memory("mem_a", "The user is pescatarian.", &[]);
        assert!(!MemorySelector::ByTopic("   ".into()).matches(&record));
    }

    #[test]
    fn every_resolution_but_discard_writes_something() {
        for kind in [
            ResolutionKind::Create,
            ResolutionKind::Reinforce,
            ResolutionKind::Refine,
            ResolutionKind::Supersede,
            ResolutionKind::Coexist,
            ResolutionKind::Stage,
            ResolutionKind::Delete,
        ] {
            assert!(kind.is_write(), "{} should write", kind.label());
        }
        assert!(!ResolutionKind::Discard.is_write());
    }
}
