//! The retrieval plan — a transient, query-oriented reading of the transcript.
//!
//! A plan answers "which parts of existing memory might matter to this turn?"
//! It never answers "what should be stored?" — that is a separate extraction
//! with a separate schema, because conflating the two produces a model that
//! stores what it was asked to recall.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::core::{stable_hash, CanonicalPredicate, MemoryKind, PlanId, TurnId};

/// Caps enforced on every plan, whoever produced it (§12.2).
///
/// A model asked for search terms will happily return forty. These bounds are
/// applied after extraction so an over-eager plan degrades to a good one rather
/// than a slow one.
pub mod limits {
    /// Maximum entities.
    pub const ENTITIES: usize = 5;
    /// Maximum topics.
    pub const TOPICS: usize = 8;
    /// Maximum predicates.
    pub const PREDICATES: usize = 5;
    /// Maximum lexical queries.
    pub const LEXICAL_QUERIES: usize = 3;
    /// Maximum terms within a single lexical query.
    pub const QUERY_TERMS: usize = 16;
    /// Maximum memory scopes.
    pub const SCOPES: usize = 5;
}

/// What the user is trying to do with memory this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalIntent {
    /// No memory is needed — a generic factual or visual question.
    #[default]
    None,
    /// The user asked what B knows.
    ExplicitRecall,
    /// The user wants a suggestion informed by their preferences.
    PersonalRecommendation,
    /// The user referred to something that happened before.
    PriorEventReference,
    /// The user referred to a person in their life.
    RelationshipReference,
    /// The user is comparing options against their preferences.
    Comparison,
    /// Memory may help but the intent is unclear.
    Ambient,
}

impl RetrievalIntent {
    /// Whether this intent justifies searching memory at all.
    pub fn requires_memory(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// An entity the plan wants memories about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalEntity {
    /// The surface form as spoken ("my wife", "Rhea").
    pub surface: String,
    /// A canonical form when one is known.
    #[serde(default)]
    pub canonical: Option<String>,
}

impl RetrievalEntity {
    /// An entity known only by how the user said it.
    pub fn surface(surface: impl Into<String>) -> Self {
        Self {
            surface: surface.into(),
            canonical: None,
        }
    }

    /// An entity resolved to a canonical name.
    pub fn resolved(surface: impl Into<String>, canonical: impl Into<String>) -> Self {
        Self {
            surface: surface.into(),
            canonical: Some(canonical.into()),
        }
    }

    /// Every form this entity should be matched by.
    pub fn forms(&self) -> Vec<String> {
        let mut forms = vec![self.surface.clone()];
        if let Some(canonical) = &self.canonical {
            forms.push(canonical.clone());
        }
        forms
    }
}

/// A time window the plan is interested in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TemporalConstraint {
    /// Earliest instant of interest.
    #[serde(default)]
    pub after: Option<DateTime<Utc>>,
    /// Latest instant of interest.
    #[serde(default)]
    pub before: Option<DateTime<Utc>>,
    /// A human label such as `last_week`, kept for explanation.
    #[serde(default)]
    pub label: Option<String>,
}

/// A transient plan for one turn's retrieval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalPlan {
    /// Plan identity.
    pub plan_id: PlanId,
    /// The turn the plan was derived from.
    pub turn_id: TurnId,
    /// The generation the plan was derived at.
    pub generation: u64,
    /// Whether memory should be consulted at all.
    pub requires_memory: bool,
    /// Confidence in that judgement.
    pub confidence: f32,
    /// What the user appears to want.
    pub intent: RetrievalIntent,
    /// Entities of interest.
    pub entities: Vec<RetrievalEntity>,
    /// Topical terms.
    pub topics: Vec<String>,
    /// Canonical predicates of interest.
    pub predicates: Vec<CanonicalPredicate>,
    /// Independent lexical queries to run and fuse.
    pub lexical_queries: Vec<String>,
    /// Memory kinds worth searching.
    pub scopes: Vec<MemoryKind>,
    /// Time window, if the user named one.
    #[serde(default)]
    pub temporal: Option<TemporalConstraint>,
    /// Hash of the transcript the plan came from, for cache keying.
    pub source_transcript_hash: String,
}

impl RetrievalPlan {
    /// A plan that says "do not search".
    pub fn skip(turn_id: TurnId, generation: u64, transcript: &str) -> Self {
        Self {
            plan_id: PlanId::generate(),
            turn_id,
            generation,
            requires_memory: false,
            confidence: 1.0,
            intent: RetrievalIntent::None,
            entities: Vec::new(),
            topics: Vec::new(),
            predicates: Vec::new(),
            lexical_queries: Vec::new(),
            scopes: Vec::new(),
            temporal: None,
            source_transcript_hash: stable_hash(transcript),
        }
    }

    /// Apply the §12.2 caps and drop empties, returning the trimmed plan.
    ///
    /// Called on every plan regardless of origin, so a model that returns
    /// forty search terms produces a fast plan rather than a slow one.
    pub fn normalized(mut self) -> Self {
        self.entities.retain(|e| !e.surface.trim().is_empty());
        self.entities.truncate(limits::ENTITIES);

        self.topics = dedup_non_empty(self.topics);
        self.topics.truncate(limits::TOPICS);

        self.predicates.dedup();
        self.predicates.truncate(limits::PREDICATES);

        self.lexical_queries = dedup_non_empty(self.lexical_queries)
            .into_iter()
            .map(|q| {
                q.split_whitespace()
                    .take(limits::QUERY_TERMS)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        self.lexical_queries.truncate(limits::LEXICAL_QUERIES);

        self.scopes.dedup();
        self.scopes.truncate(limits::SCOPES);

        self.confidence = self.confidence.clamp(0.0, 1.0);

        // A plan with nothing to search for cannot require memory, whatever it
        // claims about itself.
        if self.lexical_queries.is_empty() && self.entities.is_empty() {
            self.requires_memory = false;
            self.intent = RetrievalIntent::None;
        }
        self
    }

    /// A cache key covering everything that changes the result.
    pub fn cache_key(&self) -> String {
        let mut parts = self.lexical_queries.clone();
        parts.extend(self.entities.iter().flat_map(|e| e.forms()));
        parts.extend(self.scopes.iter().map(|s| s.scope_label().to_string()));
        parts.sort();
        stable_hash(&parts.join("|"))
    }

    /// Every entity surface form the index should boost on.
    pub fn entity_forms(&self) -> Vec<String> {
        self.entities.iter().flat_map(|e| e.forms()).collect()
    }
}

fn dedup_non_empty(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .filter(|v| seen.insert(v.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with(lexical: Vec<&str>) -> RetrievalPlan {
        RetrievalPlan {
            plan_id: PlanId::new("pln_1"),
            turn_id: TurnId(1),
            generation: 1,
            requires_memory: true,
            confidence: 1.2,
            intent: RetrievalIntent::PersonalRecommendation,
            entities: Vec::new(),
            topics: Vec::new(),
            predicates: Vec::new(),
            lexical_queries: lexical.into_iter().map(str::to_string).collect(),
            scopes: Vec::new(),
            temporal: None,
            source_transcript_hash: "hash".into(),
        }
    }

    #[test]
    fn normalization_applies_every_cap() {
        let mut plan = plan_with(vec!["a", "b", "c", "d", "e"]);
        plan.entities = (0..9)
            .map(|i| RetrievalEntity::surface(format!("e{i}")))
            .collect();
        plan.topics = (0..20).map(|i| format!("t{i}")).collect();
        plan.scopes = vec![MemoryKind::Preference; 9];

        let normalized = plan.normalized();
        assert_eq!(normalized.entities.len(), limits::ENTITIES);
        assert_eq!(normalized.topics.len(), limits::TOPICS);
        assert_eq!(normalized.lexical_queries.len(), limits::LEXICAL_QUERIES);
        assert_eq!(normalized.scopes.len(), 1, "duplicate scopes collapse");
        assert!(normalized.confidence <= 1.0);
    }

    #[test]
    fn overlong_queries_are_trimmed_to_their_leading_terms() {
        let long = (0..40)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let normalized = plan_with(vec![&long]).normalized();
        assert_eq!(
            normalized.lexical_queries[0].split_whitespace().count(),
            limits::QUERY_TERMS
        );
    }

    #[test]
    fn blank_and_duplicate_terms_are_dropped() {
        let mut plan = plan_with(vec!["quiet restaurant", "  ", "Quiet Restaurant"]);
        plan.topics = vec!["food".into(), "".into(), "FOOD".into()];
        let normalized = plan.normalized();
        assert_eq!(normalized.lexical_queries.len(), 1);
        assert_eq!(normalized.topics, vec!["food"]);
    }

    #[test]
    fn a_plan_with_nothing_to_search_for_cannot_require_memory() {
        let normalized = plan_with(vec!["", "   "]).normalized();
        assert!(!normalized.requires_memory);
        assert_eq!(normalized.intent, RetrievalIntent::None);
    }

    #[test]
    fn a_skip_plan_requires_nothing() {
        let plan = RetrievalPlan::skip(TurnId(3), 7, "what is the capital of France");
        assert!(!plan.requires_memory);
        assert!(!plan.intent.requires_memory());
    }

    #[test]
    fn the_cache_key_ignores_query_ordering_but_not_content() {
        let a = plan_with(vec!["quiet restaurant", "wife preference"]).normalized();
        let b = plan_with(vec!["wife preference", "quiet restaurant"]).normalized();
        assert_eq!(a.cache_key(), b.cache_key());

        let c = plan_with(vec!["loud restaurant"]).normalized();
        assert_ne!(a.cache_key(), c.cache_key());
    }
}
