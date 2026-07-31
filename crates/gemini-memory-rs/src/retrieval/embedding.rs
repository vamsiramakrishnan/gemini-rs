//! The text a record should be embedded as.
//!
//! [`SemanticFallback`](super::SemanticFallback) is a trait: the engine ranks
//! and fuses, and the caller supplies the vector search. That leaves one
//! decision entirely to the implementor — *what text goes into the embedder* —
//! and it turns out to matter more than any other choice in the semantic layer,
//! including the model and the number of dimensions.
//!
//! So the answer lives here rather than in a caller's head.
//!
//! # What was measured
//!
//! Six candidate texts, over 1,199 records and 93 questions phrased as people
//! actually speak rather than as the corpus is written
//! (`tests/semantic_fusion_probe.rs`):
//!
//! | what was embedded | top-1 |
//! |---|---|
//! | the statement alone | 41/93 |
//! | statement + hand-written aliases and tags | 57/93 |
//! | statement + the predicate line | 53/93 |
//! | **statement + the whole frontmatter as prose** | **66/93** |
//! | statement + six LLM-written questions | 31/93 |
//! | all of the above together | 52/93 |
//!
//! The winner is [`embedding_text`], and the thing to notice is that it costs
//! nothing: no model call, no second pass at ingestion, no author. It is the
//! fields the record already carries, written as a few lines of prose.
//!
//! It also beats the hand-written aliases — which were composed by someone who
//! had seen the question set — by nine questions.
//!
//! # Why it works
//!
//! A statement gives the *value* and only implies the *attribute*. "The user's
//! usual coffee order is a cortado" answers a question about coffee orders
//! without ever containing the words "coffee order" in the way a question asks
//! them. Naming the attribute outright is worth 12 of the 25 points (41 → 53);
//! the subject, entities and temporal scope are worth the other 13 (53 → 66).
//!
//! Every one of those fields carries retrievable signal the statement had left
//! implicit. It is not that structured boilerplate flatters the geometry.
//!
//! # Why not to enrich further
//!
//! Two separate experiments say adding model-written text makes this worse, and
//! they agree on the reason. LLM-written *questions* score 31/93, below the bare
//! statement; LLM-written short alias *terms* score worse than leaving the alias
//! field empty (`tests/alias_terms_probe.rs`).
//!
//! The shape was never the variable. Both were applied to every record, and
//! uniform enrichment is what fails: a synonym added to one record is a rare,
//! discriminating term, while the same synonym added to all of them has no IDF
//! left and discriminates nothing — having lengthened a length-normalised field
//! everywhere on the way. Selective enrichment may well pay; nothing here can
//! target it, and ingestion is the wrong place to try.

use crate::core::CanonicalMemory;

/// The text to embed for a record.
///
/// The statement, followed by the record's own frontmatter written as prose.
/// This is the measured-best input to a dense retriever for this corpus — see
/// the module docs for the comparison it won.
///
/// Use this in a [`SemanticFallback`](super::SemanticFallback) implementation
/// so that what is indexed matches what was measured:
///
/// ```
/// # use gemini_memory_rs::core::{CanonicalMemory, MemoryKind, UserId};
/// # use gemini_memory_rs::retrieval::embedding_text;
/// # fn example(records: &[CanonicalMemory]) {
/// for record in records {
///     let text = embedding_text(record);
///     // embed(&text) and store the vector against record.id
/// }
/// # }
/// ```
pub fn embedding_text(memory: &CanonicalMemory) -> String {
    format!("{}\n{}", memory.statement, frontmatter_prose(memory))
}

/// The frontmatter alone, as the prose [`embedding_text`] appends.
///
/// Separated because a caller embedding something other than the statement —
/// a summary, a window of turns — still wants the structured lines, and
/// because it is what the ablation in the module docs isolates.
pub fn frontmatter_prose(memory: &CanonicalMemory) -> String {
    let mut lines = vec![
        format!("About: {}", memory.subject.display),
        predicate_line(memory),
    ];
    if !memory.retrieval.entities.is_empty() {
        lines.push(format!(
            "Mentions: {}",
            memory.retrieval.entities.join(", ")
        ));
    }
    if let Some(location) = &memory.retrieval.location {
        lines.push(format!("Place: {location}"));
    }
    if let Some(qualifier) = &memory.qualifier {
        lines.push(format!("When: {qualifier}"));
    }
    lines.push(format!("Holds: {:?}", memory.temporal_scope));
    lines.join("\n")
}

/// The line naming the attribute this record is about.
///
/// The single highest-value line in the whole rendering: adding it alone moves
/// top-1 from 41 to 53 of 93. A question asks by the attribute; a statement
/// only implies it.
pub fn predicate_line(memory: &CanonicalMemory) -> String {
    format!(
        "Kind: {:?} {}",
        memory.kind,
        memory.predicate.as_str().replace('_', " ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::core::{
        CanonicalPredicate, EntityRef, Explicitness, MemoryId, MemoryKind, MemorySource,
        MemoryValue, RetrievalMetadata, SessionId, TemporalMetadata, TemporalScope, TurnId, UserId,
    };

    fn record() -> CanonicalMemory {
        CanonicalMemory {
            id: MemoryId::new("mem_coffee"),
            owner: UserId::new("usr_test"),
            kind: MemoryKind::Preference,
            predicate: CanonicalPredicate::new("beverage_preference"),
            status: crate::core::MemoryStatus::Active,
            confidence: 0.9,
            subject: EntityRef::named("user"),
            value: MemoryValue::Text("cortado".into()),
            statement: "The user's usual coffee order is a cortado.".into(),
            evidence_summary: "stated".into(),
            source: MemorySource::from_explicitness(
                Explicitness::ExplicitStatement,
                SessionId::new("ses_1"),
                TurnId(1),
            ),
            temporal: TemporalMetadata::created_at(Utc::now()),
            retrieval: RetrievalMetadata {
                subject: "user".into(),
                entities: vec!["cortado".into()],
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
    fn the_attribute_is_named_even_though_the_statement_only_implies_it() {
        let memory = record();
        let text = embedding_text(&memory);
        assert!(
            text.contains("beverage preference"),
            "the predicate must appear in readable words, not as a symbol: {text}"
        );
        assert!(
            text.contains("The user's usual coffee order is a cortado."),
            "the statement must survive: {text}"
        );
    }

    #[test]
    fn underscores_become_spaces_so_the_embedder_sees_words() {
        let memory = record();
        assert!(!predicate_line(&memory).contains('_'));
    }

    #[test]
    fn absent_fields_leave_no_empty_lines() {
        let memory = record();
        let prose = frontmatter_prose(&memory);
        assert!(
            !prose.lines().any(str::is_empty),
            "a missing location or qualifier should omit its line entirely: {prose:?}"
        );
        assert!(!prose.contains("Place:"), "no location was set");
        assert!(!prose.contains("When:"), "no qualifier was set");
    }
}
