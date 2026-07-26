//! The evaluation corpus and cases.
//!
//! A small, hand-written corpus that states what the engine is *supposed* to
//! do. The cases are the specification the thresholds in [`super::harness`] are
//! judged against; changing one should be a deliberate decision about product
//! behaviour, not a way to make a run go green.

use chrono::{Duration, Utc};

use crate::core::{
    CanonicalMemory, CanonicalPredicate, EntityRef, EvidenceCounters, Explicitness, MemoryId,
    MemoryKind, MemorySource, MemoryStatus, MemoryValue, PrivacyMetadata, RetrievalMetadata,
    SessionId, SpeakerAttribution, TemporalMetadata, TemporalScope, TurnId, UserId,
};

/// One retrieval expectation.
#[derive(Debug, Clone)]
pub struct RetrievalCase {
    /// Case name, for failure messages.
    pub name: &'static str,
    /// What the user said.
    pub query: &'static str,
    /// Whether memory should be consulted at all.
    pub expects_memory: bool,
    /// Records that would be reasonable to return.
    pub relevant: &'static [&'static str],
    /// Records that must never be returned.
    pub forbidden: &'static [&'static str],
}

/// What should happen to one utterance at ingestion.
#[derive(Debug, Clone)]
pub struct IngestionCase {
    /// Case name.
    pub name: &'static str,
    /// What was said.
    pub utterance: &'static str,
    /// Who said it.
    pub speaker: SpeakerAttribution,
    /// Whether a candidate should be created at all.
    pub stores: bool,
    /// The kind expected, when one is stored.
    pub kind: Option<MemoryKind>,
    /// The explicitness expected, when one is stored.
    pub explicitness: Option<Explicitness>,
}

/// The evaluation user.
pub fn eval_user() -> UserId {
    UserId::new("usr_eval")
}

fn record(
    id: &str,
    kind: MemoryKind,
    predicate: &str,
    subject: EntityRef,
    statement: &str,
    tags: &[&str],
    aliases: &[&str],
) -> CanonicalMemory {
    let now = Utc::now();
    let subject_form = crate::core::normalize_token(&subject.display);
    let entities = subject.surface_forms();
    CanonicalMemory {
        id: MemoryId::new(id),
        owner: eval_user(),
        kind,
        predicate: CanonicalPredicate::new(predicate),
        status: MemoryStatus::Active,
        confidence: 0.92,
        subject,
        value: MemoryValue::Text(statement.to_string()),
        statement: statement.to_string(),
        evidence_summary: "Explicitly stated by the user.".into(),
        source: MemorySource::from_explicitness(
            Explicitness::ExplicitStatement,
            SessionId::new("ses_eval"),
            TurnId(1),
        ),
        temporal: TemporalMetadata::created_at(now),
        retrieval: RetrievalMetadata {
            subject: subject_form,
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
            entities,
            location: None,
        },
        evidence: EvidenceCounters {
            count: 2,
            distinct_sessions: 2,
            distinct_days: 2,
        },
        privacy: PrivacyMetadata::default(),
        temporal_scope: TemporalScope::Persistent,
        supersedes: Vec::new(),
        superseded_by: None,
        qualifier: None,
    }
}

/// The evaluation corpus.
pub fn corpus() -> Vec<CanonicalMemory> {
    let rhea = EntityRef::named("Rhea")
        .with_alias("my wife")
        .with_alias("wife");

    let mut episode = record(
        "mem_bandra",
        MemoryKind::Episodic,
        "outing_outcome",
        EntityRef::user(),
        "Dinner at a noisy restaurant in Bandra went badly.",
        &["restaurant", "bandra", "noise", "dinner"],
        &["the noisy dinner"],
    );
    episode.temporal_scope = TemporalScope::RecentHistory;
    episode.temporal.valid_from = Utc::now() - Duration::days(3);
    episode.temporal.expires_at = Some(Utc::now() + Duration::days(4));

    let mut superseded = record(
        "mem_old_vegetarian",
        MemoryKind::Preference,
        "dietary_identity",
        EntityRef::user(),
        "The user is vegetarian.",
        &["diet", "food", "vegetarian"],
        &["does not eat meat"],
    );
    superseded.status = MemoryStatus::Superseded;
    superseded.superseded_by = Some(MemoryId::new("mem_diet"));

    vec![
        record(
            "mem_diet",
            MemoryKind::Preference,
            "dietary_identity",
            EntityRef::user(),
            "The user is pescatarian.",
            &["diet", "food", "pescatarian"],
            &["does not eat meat", "eats fish"],
        ),
        record(
            "mem_rhea",
            MemoryKind::Relationship,
            "spouse",
            rhea.clone(),
            "Rhea is the user's wife.",
            &["family", "wife", "spouse"],
            &["married to Rhea"],
        ),
        record(
            "mem_rhea_quiet",
            MemoryKind::RelationshipPreference,
            "venue_preference",
            rhea,
            "Rhea prefers quiet restaurants.",
            &["restaurant", "quiet", "noise", "venue"],
            &["dislikes loud places"],
        ),
        record(
            "mem_music",
            MemoryKind::Preference,
            "venue_preference",
            EntityRef::user(),
            "The user enjoys live music venues with friends.",
            &["music", "venue", "friends"],
            &["likes gigs"],
        ),
        record(
            "mem_gym",
            MemoryKind::Routine,
            "exercise_routine",
            EntityRef::user(),
            "The user goes to the gym before work.",
            &["gym", "exercise", "morning", "routine"],
            &["works out in the morning"],
        ),
        record(
            "mem_coffee",
            MemoryKind::Preference,
            "beverage_preference",
            EntityRef::user(),
            "The user drinks flat white coffee.",
            &["coffee", "beverage", "flat white"],
            &["usual order"],
        ),
        episode,
        superseded,
    ]
}

/// Retrieval cases (§37.1).
pub fn retrieval_cases() -> Vec<RetrievalCase> {
    vec![
        RetrievalCase {
            name: "generic world knowledge skips memory",
            query: "what is the capital of France",
            expects_memory: false,
            relevant: &[],
            forbidden: &["mem_diet", "mem_rhea", "mem_coffee"],
        },
        RetrievalCase {
            name: "visual question skips memory",
            query: "what does this label say",
            expects_memory: false,
            relevant: &[],
            forbidden: &["mem_diet"],
        },
        RetrievalCase {
            name: "explicit recall of diet",
            query: "what do you remember about my diet and food preferences",
            expects_memory: true,
            relevant: &["mem_diet"],
            forbidden: &["mem_old_vegetarian"],
        },
        RetrievalCase {
            name: "recommendation for a relationship",
            query: "find a quiet restaurant for my wife",
            expects_memory: true,
            relevant: &["mem_rhea_quiet", "mem_rhea", "mem_bandra"],
            forbidden: &["mem_old_vegetarian"],
        },
        RetrievalCase {
            name: "prior event reference",
            query: "how did that noisy dinner in Bandra go last week",
            expects_memory: true,
            relevant: &["mem_bandra", "mem_rhea_quiet"],
            forbidden: &["mem_old_vegetarian", "mem_coffee"],
        },
        RetrievalCase {
            name: "routine recall",
            query: "remind me about my gym routine",
            expects_memory: true,
            relevant: &["mem_gym"],
            forbidden: &["mem_coffee", "mem_old_vegetarian"],
        },
        RetrievalCase {
            name: "beverage preference",
            query: "do you remember what coffee I like",
            expects_memory: true,
            relevant: &["mem_coffee"],
            forbidden: &["mem_old_vegetarian"],
        },
        RetrievalCase {
            name: "superseded facts never resurface",
            query: "do you remember whether I eat vegetarian food",
            expects_memory: true,
            relevant: &["mem_diet"],
            forbidden: &["mem_old_vegetarian"],
        },
    ]
}

/// Ingestion cases (§37.2).
pub fn ingestion_cases() -> Vec<IngestionCase> {
    vec![
        IngestionCase {
            name: "explicit preference",
            utterance: "I am pescatarian",
            speaker: SpeakerAttribution::User,
            stores: true,
            kind: Some(MemoryKind::Identity),
            explicitness: Some(Explicitness::ExplicitStatement),
        },
        IngestionCase {
            name: "explicit memory command",
            utterance: "please remember that I am allergic to shellfish",
            speaker: SpeakerAttribution::User,
            stores: true,
            kind: None,
            explicitness: Some(Explicitness::ExplicitCommand),
        },
        IngestionCase {
            name: "time-bounded plan is episodic",
            utterance: "I am meeting Kushal for dinner tonight",
            speaker: SpeakerAttribution::User,
            stores: true,
            kind: Some(MemoryKind::Episodic),
            explicitness: Some(Explicitness::ExplicitStatement),
        },
        IngestionCase {
            name: "routine statement",
            utterance: "I always go to the gym before work",
            speaker: SpeakerAttribution::User,
            stores: true,
            kind: Some(MemoryKind::Routine),
            explicitness: Some(Explicitness::ExplicitStatement),
        },
        IngestionCase {
            name: "small talk stores nothing",
            utterance: "the weather is lovely today",
            speaker: SpeakerAttribution::User,
            stores: false,
            kind: None,
            explicitness: None,
        },
        IngestionCase {
            name: "a question is not a statement",
            utterance: "what i am asking is whether the place is open",
            speaker: SpeakerAttribution::User,
            stores: false,
            kind: None,
            explicitness: None,
        },
        IngestionCase {
            name: "bystander speech is refused",
            utterance: "I am vegetarian",
            speaker: SpeakerAttribution::Bystander,
            stores: false,
            kind: None,
            explicitness: None,
        },
        IngestionCase {
            name: "assistant speech is refused",
            utterance: "I am a helpful assistant",
            speaker: SpeakerAttribution::Assistant,
            stores: false,
            kind: None,
            explicitness: None,
        },
        IngestionCase {
            name: "unattributed speech is refused",
            utterance: "I prefer window seats",
            speaker: SpeakerAttribution::Unknown,
            stores: false,
            kind: None,
            explicitness: None,
        },
        IngestionCase {
            name: "forget command is recognised",
            utterance: "forget that I like sushi",
            speaker: SpeakerAttribution::User,
            stores: true,
            kind: None,
            explicitness: Some(Explicitness::ExplicitCommand),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corpus_is_internally_consistent() {
        let corpus = corpus();
        let ids: Vec<String> = corpus.iter().map(|m| m.id.to_string()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate record ids");
        assert!(corpus.iter().all(|m| m.owner == eval_user()));
        assert!(
            corpus.iter().any(|m| m.status == MemoryStatus::Superseded),
            "the corpus must include a superseded record to test against"
        );
    }

    #[test]
    fn every_case_names_records_that_exist() {
        let ids: Vec<String> = corpus().iter().map(|m| m.id.to_string()).collect();
        for case in retrieval_cases() {
            for id in case.relevant.iter().chain(case.forbidden.iter()) {
                assert!(
                    ids.contains(&(*id).to_string()),
                    "case `{}` names unknown record `{id}`",
                    case.name
                );
            }
        }
    }

    #[test]
    fn the_case_set_covers_both_skip_and_recall() {
        let cases = retrieval_cases();
        assert!(cases.iter().any(|c| !c.expects_memory));
        assert!(cases.iter().any(|c| c.expects_memory));
        assert!(ingestion_cases().iter().any(|c| !c.stores));
        assert!(ingestion_cases().iter().any(|c| c.stores));
    }
}
