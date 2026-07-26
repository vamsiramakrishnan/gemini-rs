//! The memory domain model — the vocabulary shared by every stage of the
//! pipeline (capture → staging → retrieval → reconciliation → promotion).
//!
//! Types here are deliberately serialization-stable: they are the schema of the
//! OKF front matter, the event log, and the structured-output contracts handed
//! to out-of-band extraction models.

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

use super::ids::{EntityId, MemoryId, ObservationId, SessionId, TurnId, UserId};

/// What sort of thing a memory records.
///
/// Kind drives retrieval scoping, default persistence and promotion policy —
/// an `Episodic` memory expires, an `Identity` memory does not.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// A stable fact about who the user is.
    Identity,
    /// A durable like, dislike or standing choice.
    Preference,
    /// A person the user is connected to.
    Relationship,
    /// A preference held by someone the user is connected to.
    RelationshipPreference,
    /// A recurring behaviour.
    Routine,
    /// Something the user has undertaken to do.
    Commitment,
    /// A piece of ongoing work.
    Project,
    /// A time-bounded event, condition or situation.
    Episodic,
    /// How the user prefers to be spoken to.
    CommunicationStyle,
    /// A place-scoped preference.
    LocationPreference,
    /// An inferred pattern awaiting reinforcement.
    StagedPattern,
}

impl MemoryKind {
    /// Whether this kind is inherently time-bounded.
    pub fn is_episodic(self) -> bool {
        matches!(self, Self::Episodic | Self::Commitment)
    }

    /// The retrieval scopes a plan may name to reach this kind.
    pub fn scope_label(self) -> &'static str {
        match self {
            Self::Identity => "profile",
            Self::Preference | Self::LocationPreference => "preferences",
            Self::Relationship | Self::RelationshipPreference => "relationships",
            Self::Routine => "routines",
            Self::Commitment => "commitments",
            Self::Project => "projects",
            Self::Episodic => "episodes",
            Self::CommunicationStyle => "communication",
            Self::StagedPattern => "staged",
        }
    }
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = serde_json::to_value(self).map_err(|_| fmt::Error)?;
        f.write_str(raw.as_str().unwrap_or("unknown"))
    }
}

/// How directly the user stated the thing being remembered.
///
/// Explicitness is the primary authority ordering in the engine: an explicit
/// command outranks an explicit statement, which outranks any inference,
/// regardless of how often the inference recurs.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Explicitness {
    /// A single weak inference from indirect evidence.
    WeakInference,
    /// A strong implication the user did not state outright.
    StrongImplication,
    /// The user stated the fact directly.
    ExplicitStatement,
    /// The user issued a memory command ("remember that…", "forget…").
    ExplicitCommand,
}

impl Explicitness {
    /// The maximum aggregated confidence evidence at this level may reach.
    ///
    /// Repetition of weak evidence must not manufacture certainty, so each
    /// level carries a hard ceiling (§18.4).
    pub fn confidence_ceiling(self, distinct_evidence: u32) -> f32 {
        match self {
            Self::ExplicitCommand => 1.0,
            Self::ExplicitStatement => 0.95,
            Self::StrongImplication => {
                if distinct_evidence > 1 {
                    0.85
                } else {
                    0.70
                }
            }
            Self::WeakInference => {
                if distinct_evidence > 1 {
                    0.75
                } else {
                    0.55
                }
            }
        }
    }

    /// Whether evidence at this level may be committed without reinforcement.
    pub fn is_explicit(self) -> bool {
        matches!(self, Self::ExplicitStatement | Self::ExplicitCommand)
    }
}

/// Lifecycle state of a canonical memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    /// Retrievable and current.
    #[default]
    Active,
    /// Held pending reinforcement; not retrievable as a durable fact.
    Staged,
    /// Replaced by a newer contradicting record.
    Superseded,
    /// Past its validity window.
    Expired,
    /// Removed at the user's request; retained only as a tombstone.
    Deleted,
}

impl MemoryStatus {
    /// Whether records in this state participate in normal retrieval.
    pub fn is_retrievable(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// How long a fact is expected to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TemporalScope {
    /// Expected to hold until explicitly changed.
    #[default]
    Persistent,
    /// A recent event worth recalling for days.
    RecentHistory,
    /// A transient state measured in hours.
    Momentary,
    /// Tied to a specific future time.
    Scheduled,
}

/// Privacy classification, gating automatic promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityClass {
    /// Ordinary personal context.
    #[default]
    Normal,
    /// Health, religion, politics, sexuality and similar categories.
    Sensitive,
    /// Never stored durably by this engine.
    Restricted,
}

/// Who actually said the thing an observation was drawn from.
///
/// Only [`SpeakerAttribution::User`] speech may become memory. Bystander and
/// assistant-originated content is discarded at ingestion, not filtered later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerAttribution {
    /// The enrolled user, speaking to B.
    User,
    /// Someone else within microphone range.
    Bystander,
    /// B's own output, echoed back through a transcript.
    Assistant,
    /// Attribution could not be established.
    #[default]
    Unknown,
}

impl SpeakerAttribution {
    /// Whether observations from this speaker may be stored at all.
    pub fn may_be_stored(self) -> bool {
        matches!(self, Self::User)
    }
}

/// What the extractor proposes should happen to a candidate over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposedPersistence {
    /// Long-lived semantic memory.
    Durable,
    /// Time-bounded episodic memory.
    Episodic,
    /// Useful for this conversation only.
    SessionOnly,
    /// Insufficient evidence; hold for reinforcement.
    Staged,
    /// Should not be retained.
    Discard,
}

/// The value side of a memory triple.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum MemoryValue {
    /// Free text — the common case.
    Text(String),
    /// A boolean assertion.
    Bool(bool),
    /// A numeric quantity.
    Number(f64),
    /// An unordered set of values.
    List(Vec<String>),
}

impl MemoryValue {
    /// A normalized, lowercase rendering used for fingerprinting.
    pub fn normalized(&self) -> String {
        match self {
            Self::Text(t) => normalize_token(t),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => format!("{n}"),
            Self::List(items) => {
                let mut normalized: Vec<String> =
                    items.iter().map(|i| normalize_token(i)).collect();
                normalized.sort();
                normalized.join(",")
            }
        }
    }

    /// A human-readable rendering for statements and search text.
    pub fn display(&self) -> String {
        match self {
            Self::Text(t) => t.clone(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => format!("{n}"),
            Self::List(items) => items.join(", "),
        }
    }
}

impl From<&str> for MemoryValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

/// A canonicalized predicate name such as `dietary_identity`.
///
/// Predicates are normalized on construction so `"Dietary Identity"` and
/// `"dietary_identity"` fingerprint identically.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct CanonicalPredicate(String);

impl CanonicalPredicate {
    /// Normalize and wrap a predicate name.
    pub fn new(raw: impl AsRef<str>) -> Self {
        let normalized: String = raw
            .as_ref()
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let collapsed = normalized
            .split('_')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("_");
        Self(collapsed)
    }

    /// Borrow the normalized predicate.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CanonicalPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A reference to the subject of a memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EntityRef {
    /// Stable identifier, normalized from the display name when unknown.
    pub id: EntityId,
    /// How the user refers to this entity.
    pub display: String,
    /// Alternative names ("wife", "Rhea", "my partner").
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl EntityRef {
    /// The user themselves — the subject of most memories.
    pub fn user() -> Self {
        Self {
            id: EntityId::new("user"),
            display: "user".to_string(),
            aliases: vec!["I".into(), "me".into(), "my".into()],
        }
    }

    /// A named third party, with the id derived from the normalized name.
    pub fn named(display: impl Into<String>) -> Self {
        let display = display.into();
        Self {
            id: EntityId::new(normalize_token(&display)),
            display,
            aliases: Vec::new(),
        }
    }

    /// Add an alias, returning the modified reference.
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    /// Every surface form this entity can be matched by, normalized.
    pub fn surface_forms(&self) -> Vec<String> {
        let mut forms = vec![normalize_token(&self.display)];
        forms.extend(self.aliases.iter().map(|a| normalize_token(a)));
        forms.retain(|f| !f.is_empty());
        forms.sort();
        forms.dedup();
        forms
    }
}

/// A deduplication key for "the same fact stated twice".
///
/// A fingerprint is a *hint*, not an identity: reconciliation may still decide
/// two differently-fingerprinted candidates are semantically equivalent, or
/// that two identically-fingerprinted ones differ by context.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactFingerprint(String);

impl FactFingerprint {
    /// Build a fingerprint from the normalized triple plus temporal scope.
    pub fn new(
        subject: &EntityRef,
        predicate: &CanonicalPredicate,
        value: &MemoryValue,
        scope: TemporalScope,
    ) -> Self {
        let scope_part = match scope {
            TemporalScope::Persistent => "",
            TemporalScope::RecentHistory => "|recent",
            TemporalScope::Momentary => "|momentary",
            TemporalScope::Scheduled => "|scheduled",
        };
        Self(format!(
            "{}|{}|{}{}",
            normalize_token(&subject.display),
            predicate.as_str(),
            value.normalized(),
            scope_part
        ))
    }

    /// Borrow the fingerprint string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The subject alone, used to widen the reconciliation candidate window.
    pub fn subject(&self) -> &str {
        self.0.split('|').next().unwrap_or(&self.0)
    }

    /// The subject-and-predicate prefix, used to find contradiction candidates.
    pub fn subject_predicate(&self) -> &str {
        let mut parts = self.0.match_indices('|');
        match (parts.next(), parts.next()) {
            (Some(_), Some((second, _))) => &self.0[..second],
            _ => &self.0,
        }
    }
}

impl fmt::Display for FactFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a memory came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemorySource {
    /// Provenance class, e.g. `explicit_user_statement`.
    pub source_type: String,
    /// The logical session the evidence was gathered in.
    pub session_id: Option<SessionId>,
    /// The turn the evidence was gathered on.
    pub turn_id: Option<TurnId>,
}

impl MemorySource {
    /// Build a source record from an explicitness level and location.
    pub fn from_explicitness(
        explicitness: Explicitness,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Self {
        let source_type = match explicitness {
            Explicitness::ExplicitCommand => "explicit_user_command",
            Explicitness::ExplicitStatement => "explicit_user_statement",
            Explicitness::StrongImplication => "strong_implication",
            Explicitness::WeakInference => "weak_inference",
        };
        Self {
            source_type: source_type.to_string(),
            session_id: Some(session_id),
            turn_id: Some(turn_id),
        }
    }

    /// Whether this source represents something the user said outright.
    pub fn is_explicit(&self) -> bool {
        self.source_type.starts_with("explicit_")
    }
}

/// Validity and freshness metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalMetadata {
    /// When the record was first written.
    pub created_at: DateTime<Utc>,
    /// When the record was last modified.
    pub updated_at: DateTime<Utc>,
    /// When evidence last confirmed the record.
    pub last_confirmed_at: DateTime<Utc>,
    /// When the fact started holding.
    pub valid_from: DateTime<Utc>,
    /// When the fact stopped holding, if superseded.
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    /// When an episodic record should stop being retrieved.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

impl TemporalMetadata {
    /// Fresh metadata for a record created now.
    pub fn created_at(now: DateTime<Utc>) -> Self {
        Self {
            created_at: now,
            updated_at: now,
            last_confirmed_at: now,
            valid_from: now,
            valid_to: None,
            expires_at: None,
        }
    }

    /// Apply an expiry `ttl` from `now`.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.expires_at = Some(self.valid_from + ttl);
        self
    }

    /// Whether the record has passed its expiry at `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|e| e <= now) || self.valid_to.is_some_and(|v| v <= now)
    }

    /// Whole days since the record was last confirmed.
    pub fn days_since_confirmed(&self, now: DateTime<Utc>) -> i64 {
        (now - self.last_confirmed_at).num_days().max(0)
    }
}

/// The fields a memory is matched on during lexical retrieval.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RetrievalMetadata {
    /// Normalized subject surface form.
    pub subject: String,
    /// Topical tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Paraphrases the fact may be asked for by.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Other entities the fact mentions.
    #[serde(default)]
    pub entities: Vec<String>,
    /// Place the fact is scoped to, if any.
    #[serde(default)]
    pub location: Option<String>,
}

/// How much evidence stands behind a memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EvidenceCounters {
    /// Total supporting observations.
    pub count: u32,
    /// Logical sessions that produced supporting evidence.
    pub distinct_sessions: u32,
    /// Calendar days that produced supporting evidence.
    pub distinct_days: u32,
}

impl EvidenceCounters {
    /// A single first observation.
    pub fn first() -> Self {
        Self {
            count: 1,
            distinct_sessions: 1,
            distinct_days: 1,
        }
    }
}

/// User-facing data rights for a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyMetadata {
    /// Whether the user may delete this record.
    pub deletable: bool,
    /// Whether the record is included in data export.
    pub exportable: bool,
    /// Category gating automatic promotion.
    pub sensitivity: SensitivityClass,
}

impl Default for PrivacyMetadata {
    fn default() -> Self {
        Self {
            deletable: true,
            exportable: true,
            sensitivity: SensitivityClass::Normal,
        }
    }
}

/// A reconciled, durable memory — the canonical unit of the OKF repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalMemory {
    /// Stable record identifier.
    pub id: MemoryId,
    /// Owning user namespace.
    pub owner: UserId,
    /// What sort of memory this is.
    pub kind: MemoryKind,
    /// Canonical predicate.
    pub predicate: CanonicalPredicate,
    /// Lifecycle state.
    pub status: MemoryStatus,
    /// Aggregated confidence in `[0, 1]`.
    pub confidence: f32,
    /// The subject of the memory.
    pub subject: EntityRef,
    /// The value side of the triple.
    pub value: MemoryValue,
    /// One-sentence natural-language rendering, shown to the model.
    pub statement: String,
    /// Why the engine believes this.
    pub evidence_summary: String,
    /// Provenance.
    pub source: MemorySource,
    /// Validity window.
    pub temporal: TemporalMetadata,
    /// Lexical retrieval surface.
    pub retrieval: RetrievalMetadata,
    /// Evidence counters.
    pub evidence: EvidenceCounters,
    /// Data rights.
    pub privacy: PrivacyMetadata,
    /// How long the fact is expected to hold.
    pub temporal_scope: TemporalScope,
    /// Records this one replaced.
    #[serde(default)]
    pub supersedes: Vec<MemoryId>,
    /// The record that replaced this one.
    #[serde(default)]
    pub superseded_by: Option<MemoryId>,
    /// Context qualifier distinguishing coexisting facts ("with family").
    #[serde(default)]
    pub qualifier: Option<String>,
}

impl CanonicalMemory {
    /// The fingerprint this record would produce.
    pub fn fingerprint(&self) -> FactFingerprint {
        FactFingerprint::new(
            &self.subject,
            &self.predicate,
            &self.value,
            self.temporal_scope,
        )
    }

    /// Whether the record may be returned to the model at `now`.
    pub fn is_retrievable(&self, now: DateTime<Utc>) -> bool {
        self.status.is_retrievable() && !self.temporal.is_expired(now)
    }
}

/// A structured interpretation of one specific user statement.
///
/// An observation is *evidence*, not a fact. It becomes a fact only after
/// consolidation and reconciliation accept it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryObservation {
    /// Identifier for this observation.
    pub observation_id: ObservationId,
    /// Logical session it was drawn from.
    pub session_id: SessionId,
    /// Turn it was drawn from.
    pub turn_id: TurnId,
    /// Who or what the statement is about.
    pub subject: EntityRef,
    /// Canonical predicate.
    pub predicate: CanonicalPredicate,
    /// Value side of the triple.
    pub value: MemoryValue,
    /// One-sentence natural-language rendering.
    pub canonical_statement: String,
    /// Proposed memory kind.
    pub kind: MemoryKind,
    /// How directly it was stated.
    pub explicitness: Explicitness,
    /// Extractor confidence in `[0, 1]`.
    pub confidence: f32,
    /// Proposed retention.
    pub persistence: ProposedPersistence,
    /// Expected temporal scope.
    pub temporal_scope: TemporalScope,
    /// When the fact started holding, if stated.
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    /// When the fact is expected to stop holding.
    #[serde(default)]
    pub expected_expiry: Option<DateTime<Utc>>,
    /// The transcript span this was drawn from.
    pub transcript_evidence: TranscriptEvidence,
    /// Who said it.
    pub speaker_attribution: SpeakerAttribution,
    /// Privacy classification.
    pub sensitivity: SensitivityClass,
    /// Whether this observation is a memory *command* rather than a statement.
    #[serde(default)]
    pub mutation_intent: Option<MutationIntent>,
}

impl MemoryObservation {
    /// The fingerprint this observation would produce.
    pub fn fingerprint(&self) -> FactFingerprint {
        FactFingerprint::new(
            &self.subject,
            &self.predicate,
            &self.value,
            self.temporal_scope,
        )
    }
}

/// An explicit user instruction about memory itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MutationIntent {
    /// "Remember that…"
    Remember,
    /// "Actually, it's…"
    Correct,
    /// "Forget…"
    Forget,
    /// "Delete everything about…"
    Delete,
    /// "What do you remember about me?"
    List,
}

/// The evidence span an observation was drawn from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TranscriptEvidence {
    /// The finalized user utterance.
    pub utterance: String,
    /// Stable hash of the utterance, for idempotency.
    pub utterance_hash: String,
}

impl TranscriptEvidence {
    /// Build evidence from a finalized utterance.
    pub fn new(utterance: impl Into<String>) -> Self {
        let utterance = utterance.into();
        let hash = stable_hash(&utterance);
        Self {
            utterance,
            utterance_hash: hash,
        }
    }
}

/// Lowercase a string and collapse everything non-alphanumeric to single spaces.
pub fn normalize_token(raw: &str) -> String {
    let lowered: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect();
    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A stable, dependency-free 64-bit content hash rendered as hex.
///
/// Used for idempotency keys and transcript fingerprints, never for security.
pub fn stable_hash(input: &str) -> String {
    // FNV-1a, 64-bit.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicates_normalize_to_snake_case() {
        assert_eq!(
            CanonicalPredicate::new("Dietary Identity").as_str(),
            "dietary_identity"
        );
        assert_eq!(
            CanonicalPredicate::new("  dietary--identity  ").as_str(),
            "dietary_identity"
        );
    }

    #[test]
    fn identical_facts_fingerprint_identically() {
        let subject = EntityRef::user();
        let predicate = CanonicalPredicate::new("dietary_identity");
        let a = FactFingerprint::new(
            &subject,
            &predicate,
            &MemoryValue::Text("Pescatarian".into()),
            TemporalScope::Persistent,
        );
        let b = FactFingerprint::new(
            &subject,
            &predicate,
            &MemoryValue::Text("pescatarian".into()),
            TemporalScope::Persistent,
        );
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "user|dietary_identity|pescatarian");
    }

    #[test]
    fn fingerprints_expose_their_subject() {
        let fp = FactFingerprint::new(
            &EntityRef::named("Rhea"),
            &CanonicalPredicate::new("venue_preference"),
            &MemoryValue::Text("quiet".into()),
            TemporalScope::Persistent,
        );
        assert_eq!(fp.subject(), "rhea");
    }

    #[test]
    fn fingerprints_expose_the_subject_predicate_prefix() {
        let fp = FactFingerprint::new(
            &EntityRef::user(),
            &CanonicalPredicate::new("dietary_identity"),
            &MemoryValue::Text("vegetarian".into()),
            TemporalScope::Persistent,
        );
        assert_eq!(fp.subject_predicate(), "user|dietary_identity");
    }

    #[test]
    fn temporal_scope_separates_fingerprints() {
        let persistent = FactFingerprint::new(
            &EntityRef::user(),
            &CanonicalPredicate::new("activity"),
            &MemoryValue::Text("travelling".into()),
            TemporalScope::Persistent,
        );
        let momentary = FactFingerprint::new(
            &EntityRef::user(),
            &CanonicalPredicate::new("activity"),
            &MemoryValue::Text("travelling".into()),
            TemporalScope::Momentary,
        );
        assert_ne!(persistent, momentary);
    }

    #[test]
    fn repetition_of_weak_evidence_cannot_manufacture_certainty() {
        assert_eq!(Explicitness::WeakInference.confidence_ceiling(9), 0.75);
        assert_eq!(Explicitness::ExplicitCommand.confidence_ceiling(1), 1.0);
    }

    #[test]
    fn only_user_speech_may_be_stored() {
        assert!(SpeakerAttribution::User.may_be_stored());
        assert!(!SpeakerAttribution::Bystander.may_be_stored());
        assert!(!SpeakerAttribution::Assistant.may_be_stored());
        assert!(!SpeakerAttribution::Unknown.may_be_stored());
    }

    #[test]
    fn expiry_is_evaluated_against_both_windows() {
        let now = Utc::now();
        let meta = TemporalMetadata::created_at(now).with_ttl(Duration::hours(6));
        assert!(!meta.is_expired(now));
        assert!(meta.is_expired(now + Duration::hours(7)));
    }

    #[test]
    fn entity_surface_forms_are_normalized_and_deduped() {
        let entity = EntityRef::named("Rhea")
            .with_alias("my wife")
            .with_alias("Rhea");
        assert_eq!(entity.surface_forms(), vec!["my wife", "rhea"]);
    }

    #[test]
    fn stable_hash_is_deterministic_and_distinguishing() {
        assert_eq!(stable_hash("hello"), stable_hash("hello"));
        assert_ne!(stable_hash("hello"), stable_hash("hellp"));
    }
}
