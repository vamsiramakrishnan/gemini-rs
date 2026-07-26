//! The lexical surface of a memory: which text is indexed, in which field, and
//! how much each field is worth.
//!
//! Fields are weighted rather than concatenated because *where* a term matched
//! carries most of the signal. "Rhea" appearing as the subject of a record is a
//! far stronger indication of relevance than "Rhea" appearing in the middle of
//! a sentence.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::{
    normalize_token, CanonicalMemory, CanonicalPredicate, MemoryId, MemoryKind, MemoryStatus,
    TemporalScope,
};

/// The indexed fields, in weight order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// The memory's subject surface form.
    Subject,
    /// Other entities the memory mentions.
    Entities,
    /// Paraphrases the fact may be asked for by.
    Aliases,
    /// The canonical predicate.
    Predicate,
    /// Topical tags.
    Tags,
    /// A place the fact is scoped to.
    Location,
    /// The natural-language statement.
    Statement,
}

impl Field {
    /// Every field, in a stable order.
    pub const ALL: [Field; 7] = [
        Field::Subject,
        Field::Entities,
        Field::Aliases,
        Field::Predicate,
        Field::Tags,
        Field::Location,
        Field::Statement,
    ];

    /// Index into per-field arrays.
    pub fn slot(self) -> usize {
        match self {
            Field::Subject => 0,
            Field::Entities => 1,
            Field::Aliases => 2,
            Field::Predicate => 3,
            Field::Tags => 4,
            Field::Location => 5,
            Field::Statement => 6,
        }
    }

    /// The field's contribution multiplier (§13.3).
    pub fn weight(self) -> f32 {
        match self {
            Field::Subject => 3.0,
            Field::Entities => 3.0,
            Field::Aliases => 2.5,
            Field::Predicate => 2.2,
            Field::Tags => 2.0,
            Field::Location => 1.5,
            Field::Statement => 1.0,
        }
    }

    /// A short label for search explanations.
    pub fn label(self) -> &'static str {
        match self {
            Field::Subject => "subject",
            Field::Entities => "entities",
            Field::Aliases => "aliases",
            Field::Predicate => "predicate",
            Field::Tags => "tags",
            Field::Location => "location",
            Field::Statement => "statement",
        }
    }
}

/// Where an indexed memory came from.
///
/// Overlay facts are things the user said moments ago and the engine has not
/// yet committed. They are retrievable immediately and ranked above canonical
/// memory, but presented more cautiously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOrigin {
    /// Reconciled and durable.
    Canonical,
    /// Learned in the current session and not yet committed.
    SessionOverlay,
}

/// A memory reduced to what the index needs.
#[derive(Debug, Clone)]
pub struct IndexedMemory {
    /// Record identity.
    pub id: MemoryId,
    /// Memory kind, for scope filtering.
    pub kind: MemoryKind,
    /// Lifecycle state.
    pub status: MemoryStatus,
    /// Canonical predicate, for per-predicate diversity limits.
    pub predicate: CanonicalPredicate,
    /// Aggregated confidence.
    pub confidence: f32,
    /// Whether the record rests on something the user said outright.
    pub explicit: bool,
    /// Where the record came from.
    pub origin: MemoryOrigin,
    /// Expected persistence.
    pub temporal_scope: TemporalScope,
    /// When the fact started holding.
    pub valid_from: DateTime<Utc>,
    /// When it stops being retrievable, if ever.
    pub expires_at: Option<DateTime<Utc>>,
    /// The sentence handed to the model.
    pub statement: String,
    /// Normalized subject surface form, for exact-entity boosting.
    pub subject_form: String,
    /// Normalized entity surface forms, for exact-entity boosting.
    pub entity_forms: Vec<String>,
    /// Tokenized field contents.
    pub fields: [Vec<String>; 7],
}

impl IndexedMemory {
    /// Project a canonical record into the index.
    pub fn from_canonical(memory: &CanonicalMemory) -> Self {
        let mut fields: [Vec<String>; 7] = Default::default();
        fields[Field::Subject.slot()] = tokenize(&memory.retrieval.subject);
        fields[Field::Entities.slot()] = memory
            .retrieval
            .entities
            .iter()
            .flat_map(|e| tokenize(e))
            .collect();
        fields[Field::Aliases.slot()] = memory
            .retrieval
            .aliases
            .iter()
            .chain(memory.subject.aliases.iter())
            .flat_map(|a| tokenize(a))
            .collect();
        fields[Field::Predicate.slot()] = tokenize(memory.predicate.as_str());
        fields[Field::Tags.slot()] = memory
            .retrieval
            .tags
            .iter()
            .flat_map(|t| tokenize(t))
            .collect();
        fields[Field::Location.slot()] = memory
            .retrieval
            .location
            .as_deref()
            .map(tokenize)
            .unwrap_or_default();
        fields[Field::Statement.slot()] = tokenize(&memory.statement);

        let mut entity_forms: Vec<String> = memory
            .retrieval
            .entities
            .iter()
            .map(|e| normalize_token(e))
            .collect();
        entity_forms.extend(memory.subject.surface_forms());
        entity_forms.retain(|f| !f.is_empty());
        entity_forms.sort();
        entity_forms.dedup();

        Self {
            id: memory.id.clone(),
            kind: memory.kind,
            status: memory.status,
            predicate: memory.predicate.clone(),
            confidence: memory.confidence,
            explicit: memory.source.is_explicit(),
            origin: MemoryOrigin::Canonical,
            temporal_scope: memory.temporal_scope,
            valid_from: memory.temporal.valid_from,
            expires_at: memory.temporal.expires_at,
            statement: memory.statement.clone(),
            subject_form: normalize_token(&memory.subject.display),
            entity_forms,
            fields,
        }
    }

    /// Mark this document as an uncommitted session fact.
    pub fn as_session_overlay(mut self) -> Self {
        self.origin = MemoryOrigin::SessionOverlay;
        self
    }

    /// Token count in a field.
    pub fn field_len(&self, field: Field) -> usize {
        self.fields[field.slot()].len()
    }

    /// Whether the document may be returned at `now`.
    pub fn is_retrievable(&self, now: DateTime<Utc>) -> bool {
        let status_ok = match self.origin {
            // Overlay facts are staged by definition; they are still usable.
            MemoryOrigin::SessionOverlay => self.status != MemoryStatus::Deleted,
            MemoryOrigin::Canonical => self.status == MemoryStatus::Active,
        };
        status_ok && self.expires_at.is_none_or(|e| e > now)
    }
}

/// Split text into normalized, indexable terms.
///
/// Deliberately simple: lowercase, split on anything non-alphanumeric, and drop
/// stop words. A personal memory corpus is small enough that stemming buys
/// little and costs recall precision on names.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .filter(|t| !is_stop_word(t))
        .collect()
}

/// Words carrying no retrieval signal in a personal-memory corpus.
fn is_stop_word(token: &str) -> bool {
    const STOP: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "do", "does", "for", "from", "had",
        "has", "have", "in", "is", "it", "its", "of", "on", "or", "that", "the", "to", "was",
        "were", "will", "with",
    ];
    STOP.contains(&token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        EntityRef, EvidenceCounters, Explicitness, MemorySource, MemoryValue, PrivacyMetadata,
        RetrievalMetadata, SessionId, TemporalMetadata, TurnId, UserId,
    };

    fn canonical() -> CanonicalMemory {
        CanonicalMemory {
            id: MemoryId::new("mem_1"),
            owner: UserId::new("usr_1"),
            kind: MemoryKind::RelationshipPreference,
            predicate: CanonicalPredicate::new("venue_preference"),
            status: MemoryStatus::Active,
            confidence: 0.9,
            subject: EntityRef::named("Rhea").with_alias("my wife"),
            value: MemoryValue::Text("quiet restaurants".into()),
            statement: "Rhea prefers quiet restaurants.".into(),
            evidence_summary: "stated".into(),
            source: MemorySource::from_explicitness(
                Explicitness::ExplicitStatement,
                SessionId::new("ses_1"),
                TurnId(3),
            ),
            temporal: TemporalMetadata::created_at(Utc::now()),
            retrieval: RetrievalMetadata {
                subject: "rhea".into(),
                tags: vec!["restaurant".into(), "noise".into()],
                aliases: vec!["wife".into()],
                entities: vec!["Rhea".into()],
                location: Some("Bandra".into()),
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
    fn tokenizer_normalizes_and_drops_stop_words() {
        assert_eq!(
            tokenize("The user is a Pescatarian!"),
            vec!["user", "pescatarian"]
        );
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn every_field_is_populated_from_the_record() {
        let doc = IndexedMemory::from_canonical(&canonical());
        assert_eq!(doc.fields[Field::Subject.slot()], vec!["rhea"]);
        assert_eq!(doc.fields[Field::Tags.slot()], vec!["restaurant", "noise"]);
        assert!(doc.fields[Field::Aliases.slot()].contains(&"wife".to_string()));
        assert_eq!(doc.fields[Field::Location.slot()], vec!["bandra"]);
        assert!(doc.fields[Field::Statement.slot()].contains(&"quiet".to_string()));
        assert!(doc.entity_forms.contains(&"rhea".to_string()));
        assert!(doc.entity_forms.contains(&"my wife".to_string()));
    }

    #[test]
    fn subject_and_entity_fields_outweigh_the_statement() {
        assert!(Field::Subject.weight() > Field::Statement.weight());
        assert!(Field::Aliases.weight() > Field::Tags.weight());
    }

    #[test]
    fn expired_and_superseded_documents_are_not_retrievable() {
        let now = Utc::now();
        let mut doc = IndexedMemory::from_canonical(&canonical());
        assert!(doc.is_retrievable(now));

        doc.expires_at = Some(now - chrono::Duration::hours(1));
        assert!(!doc.is_retrievable(now));

        let mut superseded = IndexedMemory::from_canonical(&canonical());
        superseded.status = MemoryStatus::Superseded;
        assert!(!superseded.is_retrievable(now));
    }

    #[test]
    fn overlay_facts_stay_retrievable_while_staged() {
        let mut doc = IndexedMemory::from_canonical(&canonical()).as_session_overlay();
        doc.status = MemoryStatus::Staged;
        assert!(doc.is_retrievable(Utc::now()));
    }
}
