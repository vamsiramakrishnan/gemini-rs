//! An inventory of what a user's memory contains, for the model to read.
//!
//! # Why this exists
//!
//! [`recall_context`](crate::runtime::tools) lets the model narrow a search by
//! `about` and `attribute`. Those are only useful if it can name the values,
//! and it cannot guess them: the corpus files a haircut under `barber` and a
//! coffee order under `beverage_preference`, and a model asked to invent those
//! from an utterance produces something reasonable and wrong.
//!
//! Measured over 93 questions (`tests/memory_map_probe.rs`), asking
//! `gemini-2.5-flash-lite` to fill the filter fields:
//!
//! | condition | `about` | `attribute` | `about` + `attribute` |
//! |---|---|---|---|
//! | with no map | 49% | 2% | 2% |
//! | **with this map** | 67% | **69%** | **48%** |
//!
//! `attribute` goes from 2% to 69%. That is the difference between a filter
//! that costs more than it earns and one that pays: with the filter applied as
//! a *soft* ranking, break-even is 8% accuracy, so 2% is a net loss and 48% is
//! worth about five questions on top-5.
//!
//! # It is a fixed cost
//!
//! The map is bounded by the user's vocabulary rather than by how much they
//! have accumulated — people acquire more facts about the same handful of
//! people and properties, not endlessly more kinds of thing:
//!
//! | records | subjects | predicates | map tokens |
//! |---|---|---|---|
//! | 250 | 38 | 16 | 242 |
//! | 1,000 | 42 | 16 | 262 |
//! | 16,000 | 42 | 16 | **282** |
//!
//! Flat from a thousand records up.
//!
//! # Where to put it
//!
//! In the **system instruction**, not the tool description. Live sessions fix
//! tool declarations at connect time and the corpus grows while the session
//! runs, so a map in the schema goes stale and cannot be refreshed. Instructions
//! can be updated mid-session. It is also set once and cached rather than
//! resent per call, which is what makes 282 tokens the whole price.

use std::collections::BTreeMap;

use crate::core::{CanonicalMemory, MemoryStatus};

/// How many values of each field the map names before summarising the rest.
///
/// Chosen so the map stays inside a few hundred tokens even for a user whose
/// vocabulary is unusually wide. Values are listed most-frequent first, so a
/// truncated tail is the long tail.
pub const DEFAULT_LIMIT: usize = 40;

/// Render the inventory a model needs to write filters.
///
/// Only active records are counted — a model should not narrow a search to a
/// value that exists solely on superseded facts. Counts are included because
/// they tell the model which values are worth filtering by and which are
/// one-offs, and they cost almost nothing.
///
/// Returns an empty string when there is nothing to describe, so it can be
/// concatenated into an instruction unconditionally.
pub fn memory_map(records: &[CanonicalMemory]) -> String {
    memory_map_with_limit(records, DEFAULT_LIMIT)
}

/// [`memory_map`] with an explicit cap on values named per field.
pub fn memory_map_with_limit(records: &[CanonicalMemory], limit: usize) -> String {
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();
    if active.is_empty() {
        return String::new();
    }

    let mut subjects: BTreeMap<&str, usize> = BTreeMap::new();
    let mut predicates: BTreeMap<&str, usize> = BTreeMap::new();
    for memory in &active {
        *subjects
            .entry(memory.retrieval.subject.as_str())
            .or_default() += 1;
        *predicates.entry(memory.predicate.as_str()).or_default() += 1;
    }
    assemble(subjects, predicates, limit)
}

/// The same map, built from the live BM25 index rather than a record list.
///
/// This is the form the runtime uses. The canonical index is already resident,
/// so the map costs one pass over memory rather than a repository read — which
/// matters because it has to be rebuilt whenever the corpus changes, and the
/// corpus changes mid-conversation.
pub fn memory_map_from_index(index: &crate::bm25::MemoryIndex, limit: usize) -> String {
    let now = chrono::Utc::now();
    let mut subjects: BTreeMap<&str, usize> = BTreeMap::new();
    let mut predicates: BTreeMap<&str, usize> = BTreeMap::new();
    for doc in index.documents() {
        // `is_retrievable` rather than a status check, because it also drops
        // records that have expired — exactly as unavailable as superseded
        // ones, and exactly as wrong to advertise as filterable values.
        if !doc.is_retrievable(now) {
            continue;
        }
        *subjects.entry(doc.subject_form.as_str()).or_default() += 1;
        *predicates.entry(doc.predicate.as_str()).or_default() += 1;
    }
    assemble(subjects, predicates, limit)
}

/// Render the two vocabularies into the block the model reads.
fn assemble(
    subjects: BTreeMap<&str, usize>,
    predicates: BTreeMap<&str, usize>,
    limit: usize,
) -> String {
    if subjects.is_empty() && predicates.is_empty() {
        return String::new();
    }
    let mut map = String::from(
        "The values that exist in this user's memory. When you call \
         recall_context, `about` and `attribute` must come from these lists — \
         omit either if none fits.\n",
    );
    // Subjects are listed in their *normalised* form ("rhea", not "Rhea"),
    // because that is the form the hint matcher compares against. Showing the
    // display name would read better and would silently fail to match wherever
    // the two differ — a subject displayed as "Rhea Kapoor" is stored as
    // "rhea", and a model echoing the pretty version would narrow to nothing.
    map.push_str(&render("about", subjects, limit));
    map.push_str(&render("attribute", predicates, limit));
    map
}

/// One line: `label: value (count), value (count), and N more`.
fn render(label: &str, counts: BTreeMap<&str, usize>, limit: usize) -> String {
    let mut sorted: Vec<(&str, usize)> = counts.into_iter().collect();
    // Frequency first so truncation drops the long tail, then name so the
    // output is stable for a given corpus rather than varying run to run.
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let shown: Vec<String> = sorted
        .iter()
        .take(limit)
        .map(|(name, count)| format!("{name} ({count})"))
        .collect();
    let tail = sorted.len().saturating_sub(limit);
    let suffix = if tail > 0 {
        format!(", and {tail} more")
    } else {
        String::new()
    };
    format!("{label}: {}{suffix}\n", shown.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::core::{
        CanonicalPredicate, EntityRef, Explicitness, MemoryId, MemoryKind, MemorySource,
        MemoryValue, RetrievalMetadata, SessionId, TemporalMetadata, TemporalScope, TurnId, UserId,
    };

    fn record(subject: &str, predicate: &str, statement: &str) -> CanonicalMemory {
        CanonicalMemory {
            id: MemoryId::new(format!("mem_{predicate}_{subject}")),
            owner: UserId::new("usr_test"),
            kind: MemoryKind::Preference,
            predicate: CanonicalPredicate::new(predicate),
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
                ..Default::default()
            },
            temporal_scope: TemporalScope::Persistent,
            qualifier: None,
            evidence: Default::default(),
            privacy: Default::default(),
            supersedes: Vec::new(),
            superseded_by: None,
        }
    }

    #[test]
    fn it_names_the_values_a_model_would_otherwise_have_to_guess() {
        let records = vec![
            record("user", "barber", "The user's barber is Deepa."),
            record("user", "beverage_preference", "The user drinks cortados."),
            record("Rhea", "barber", "Rhea's barber is Sam."),
        ];
        let map = memory_map(&records);
        assert!(map.contains("user (2)"), "{map}");
        // Normalised, not the display form — see the note in the renderer.
        assert!(map.contains("rhea (1)"), "{map}");
        assert!(map.contains("barber (2)"), "{map}");
        assert!(map.contains("beverage_preference (1)"), "{map}");
    }

    #[test]
    fn superseded_records_do_not_advertise_values_that_no_longer_apply() {
        let mut stale = record("Priya", "barber", "Priya's barber was Sam.");
        stale.status = MemoryStatus::Superseded;
        let records = vec![
            record("user", "barber", "The user's barber is Deepa."),
            stale,
        ];
        let map = memory_map(&records);
        assert!(
            !map.contains("priya"),
            "a value only present on superseded records should not be offered: {map}"
        );
    }

    #[test]
    fn the_tail_is_summarised_rather_than_dropped_silently() {
        let records: Vec<CanonicalMemory> = (0..10)
            .map(|i| record("user", &format!("attribute_{i}"), "x"))
            .collect();
        let map = memory_map_with_limit(&records, 3);
        assert!(map.contains("and 7 more"), "{map}");
    }

    /// The runtime path: the map built from the live index must say the same
    /// thing as the map built from records, or the thing measured and the thing
    /// shipped are different maps.
    #[test]
    fn the_index_backed_map_agrees_with_the_record_backed_one() {
        let records = vec![
            record("user", "barber", "The user's barber is Deepa."),
            record("user", "beverage_preference", "The user drinks cortados."),
            record("Rhea", "barber", "Rhea's barber is Sam."),
        ];
        let index = crate::bm25::MemoryIndex::build(
            records
                .iter()
                .map(crate::bm25::IndexedMemory::from_canonical),
        );
        assert_eq!(
            memory_map_from_index(&index, DEFAULT_LIMIT),
            memory_map(&records),
            "the two builders must not drift; the experiments measured one and \
             the runtime serves the other"
        );
    }

    /// A superseded record is dropped by the index, so it cannot reach the map.
    #[test]
    fn the_index_backed_map_omits_records_that_are_not_retrievable() {
        let mut stale = record("Priya", "barber", "Priya's barber was Sam.");
        stale.status = MemoryStatus::Superseded;
        let records = [
            record("user", "barber", "The user's barber is Deepa."),
            stale,
        ];
        let index = crate::bm25::MemoryIndex::build(
            records
                .iter()
                .map(crate::bm25::IndexedMemory::from_canonical),
        );
        let map = memory_map_from_index(&index, DEFAULT_LIMIT);
        assert!(
            !map.contains("priya"),
            "a superseded record must not be offered as a filter value: {map}"
        );
        assert!(map.contains("user"), "{map}");
    }

    #[test]
    fn an_empty_index_renders_nothing_to_concatenate() {
        let index = crate::bm25::MemoryIndex::new();
        assert!(memory_map_from_index(&index, DEFAULT_LIMIT).is_empty());
    }

    #[test]
    fn an_empty_corpus_renders_nothing_to_concatenate() {
        assert!(memory_map(&[]).is_empty());
    }

    /// The property the token budget rests on: the map grows with the
    /// vocabulary, not with how much the user has accumulated.
    #[test]
    fn more_records_over_the_same_vocabulary_do_not_grow_the_map() {
        let few: Vec<CanonicalMemory> = (0..10)
            .map(|_| record("user", "barber", "The user's barber is Deepa."))
            .collect();
        let many: Vec<CanonicalMemory> = (0..10_000)
            .map(|_| record("user", "barber", "The user's barber is Deepa."))
            .collect();
        // Same values, so the same lines — only the counts differ in width.
        assert!(memory_map(&many).len() < memory_map(&few).len() + 16);
    }
}
