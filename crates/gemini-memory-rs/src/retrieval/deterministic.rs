//! Rule-based retrieval planning.
//!
//! This runs on partial transcripts, where a model call would be both too slow
//! and too speculative, and it runs first on final transcripts so the model
//! extractor has something to refine rather than invent. It is also the
//! fallback when the out-of-band extractor is unavailable — degrading to
//! keyword retrieval is far better than degrading to no memory.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

use super::plan::{RetrievalEntity, RetrievalIntent, RetrievalPlan, TemporalConstraint};
use crate::bm25::{tokenize, MemoryIndex};
use crate::core::{normalize_token, stable_hash, CanonicalPredicate, MemoryKind, PlanId, TurnId};

/// A signal the rules recognised in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalSignal {
    /// A name or alias already present in memory.
    KnownEntity(String),
    /// Language about liking, preferring or avoiding.
    PreferencePredicate,
    /// A kinship or relationship term.
    RelationshipReference,
    /// A reference to a past event.
    PriorEventReference,
    /// A direct request to recall.
    ExplicitRecall,
    /// A request for a personalized suggestion.
    PersonalRecommendation,
    /// A comparison between options.
    Comparison,
    /// A named time window.
    TemporalRecall(String),
}

impl RetrievalSignal {
    /// Whether this signal alone justifies bypassing the speculation debounce.
    pub fn is_strong(&self) -> bool {
        matches!(
            self,
            Self::KnownEntity(_) | Self::ExplicitRecall | Self::RelationshipReference
        )
    }
}

/// Surface forms the engine already knows to be entities.
///
/// Built from the corpus, so "Rhea" is recognised because there are memories
/// about Rhea — not because of a name list.
#[derive(Debug, Clone, Default)]
pub struct KnownEntities {
    forms: HashMap<String, String>,
}

impl KnownEntities {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Learn every subject and alias present in an index.
    pub fn from_index(index: &MemoryIndex) -> Self {
        let mut table = Self::new();
        for doc in index.documents() {
            let canonical = doc.subject_form.clone();
            if canonical.is_empty() || canonical == "user" {
                continue;
            }
            for form in &doc.entity_forms {
                table.insert(form, &canonical);
            }
            table.insert(&canonical, &canonical);
        }
        table
    }

    /// Register a surface form for a canonical entity.
    pub fn insert(&mut self, surface: &str, canonical: &str) {
        let key = normalize_token(surface);
        if !key.is_empty() {
            self.forms.insert(key, canonical.to_string());
        }
    }

    /// Resolve a surface form.
    pub fn resolve(&self, surface: &str) -> Option<&str> {
        self.forms
            .get(&normalize_token(surface))
            .map(String::as_str)
    }

    /// How many forms are known.
    pub fn len(&self) -> usize {
        self.forms.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.forms.is_empty()
    }

    /// Find every known form occurring in `text`, longest first so "my wife"
    /// wins over a bare "wife".
    fn matches_in(&self, text: &str) -> Vec<(String, String)> {
        let haystack = normalize_token(text);
        let mut hits: Vec<(String, String)> = self
            .forms
            .iter()
            .filter(|(form, _)| contains_word_sequence(&haystack, form))
            .map(|(form, canonical)| (form.clone(), canonical.clone()))
            .collect();
        hits.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        hits
    }
}

/// Whether `needle` occurs in `haystack` on word boundaries.
fn contains_word_sequence(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hay: Vec<&str> = haystack.split_whitespace().collect();
    let ned: Vec<&str> = needle.split_whitespace().collect();
    if ned.is_empty() || ned.len() > hay.len() {
        return false;
    }
    hay.windows(ned.len()).any(|w| w == ned.as_slice())
}

/// Recall phrases.
///
/// Code-switched forms are first-class here, not an afterthought. Most Indian
/// users do not speak one language per sentence, and a rule planner that only
/// recognises English silently answers "no memory needed" to
/// "mujhe yaad dilao, mera khaana ka preference kya hai" — which is a request
/// to recall, in so many words.
const RECALL_PHRASES: &[&str] = &[
    // English
    "do you remember",
    "what do you remember",
    "what do you know about",
    "remind me",
    "did i tell you",
    "i told you",
    "you said",
    "have i mentioned",
    // Hinglish
    "yaad hai",
    "yaad dila",
    "yaad karo",
    "yaad aata",
    "pata hai",
    "maine bataya",
    "maine kaha",
    "tumhe pata",
    "aapko pata",
    // Tanglish
    "ninaivirukka",
    "gnabagam",
    "theriyuma",
    "sollu naan",
];

const RECOMMENDATION_PHRASES: &[&str] = &[
    // English
    "should i",
    "should we",
    "recommend",
    "suggest",
    "where should",
    "what should",
    "any ideas",
    "help me pick",
    "book a",
    "find me",
    // Hinglish
    "kya karu",
    "kya karein",
    "kya karna",
    "kahan jaye",
    "kahan chale",
    "batao",
    "bata do",
    "suggest karo",
    "dhundo",
    // Tanglish
    "enna pannalam",
    "enga polam",
    "sollunga",
    "venuma",
];

const PRIOR_EVENT_PHRASES: &[&str] = &[
    "last time",
    "the other day",
    "earlier",
    "again",
    "before",
    "previously",
    "last week",
    "last night",
    "yesterday",
];

const PREFERENCE_WORDS: &[&str] = &[
    "like",
    "likes",
    "liked",
    "love",
    "loves",
    "hate",
    "hates",
    "prefer",
    "prefers",
    "preferred",
    "favourite",
    "favorite",
    "allergic",
    "avoid",
    "avoids",
    "usual",
    "always",
    "never",
];

const COMPARISON_PHRASES: &[&str] = &["better than", "instead of", "rather than", "compared to"];

const KINSHIP_TERMS: &[&str] = &[
    // Hinglish possessives, which carry the same relationship signal.
    "meri wife",
    "mera husband",
    "meri patni",
    "mera pati",
    "meri biwi",
    "mera beta",
    "meri beti",
    "meri maa",
    "mere papa",
    "mera bhai",
    "meri behen",
    "mera dost",
    // Tanglish
    "en wife",
    "en husband",
    "en amma",
    "en appa",
    "en thambi",
    "en friend",
    // English
    "my wife",
    "my husband",
    "my partner",
    "my mother",
    "my father",
    "my mum",
    "my dad",
    "my son",
    "my daughter",
    "my sister",
    "my brother",
    "my friend",
    "my colleague",
    "my boss",
    "my team",
];

/// Words that carry no topical signal even though they survive tokenization.
///
/// Two groups. The first is ordinary function words. The second is the
/// vocabulary of *asking* — "remember", "preference", "like" — which the plan
/// already captures as an intent and a signal. Leaving those in the query text
/// makes every preference record match every preference question, because
/// "preference" is a word each of them contains by construction rather than
/// because it is what the user is asking about.
const NON_TOPICAL: &[&str] = &[
    // Function words.
    "i",
    "me",
    "my",
    "we",
    "us",
    "our",
    "you",
    "your",
    "he",
    "she",
    "they",
    "them",
    "what",
    "when",
    "where",
    "who",
    "how",
    "why",
    "should",
    "would",
    "could",
    "can",
    "get",
    "got",
    "go",
    "going",
    "want",
    "need",
    "think",
    "some",
    "any",
    "this",
    "there",
    "just",
    "about",
    "did",
    "does",
    "do",
    "s",
    "t",
    "m",
    "re",
    "ve",
    "ll",
    "d",
    // The vocabulary of asking, already captured as intent.
    "remember",
    "memory",
    "recall",
    "remind",
    "know",
    "tell",
    "told",
    "say",
    "said",
    "preference",
    "like",
    "love",
    "hate",
    "prefer",
    // Romanized Hindi and Tamil function words. Without these, "hai", "nahi"
    // and "enakku" are treated as high-signal content terms and dominate
    // ranking in exactly the sentences where real content words are rarest.
    "hai",
    "hain",
    "hoon",
    "hun",
    "tha",
    "thi",
    "hota",
    "raha",
    "rahi",
    "kar",
    "karo",
    "karu",
    "karna",
    "kya",
    "kaise",
    "kab",
    "kahan",
    "kyun",
    "mera",
    "meri",
    "mere",
    "mujhe",
    "maine",
    "tum",
    "tumhara",
    "tumhe",
    "aap",
    "aapko",
    "apna",
    "hum",
    "humara",
    "aur",
    "bhi",
    "toh",
    "yeh",
    "woh",
    "nahi",
    "nahin",
    "bilkul",
    "thoda",
    "bahut",
    "yaad",
    "dilao",
    "bata",
    "batao",
    "pata",
    "naan",
    "enakku",
    "enna",
    "unga",
    "epdi",
    "romba",
    "oru",
    "ille",
    "illai",
    "irukku",
    "theriyuma",
    "sollu",
    "sollunga",
    "vanthu",
    "appuram",
];

/// The rule-based planner.
#[derive(Debug, Default)]
pub struct DeterministicPlanner {
    known: KnownEntities,
}

impl DeterministicPlanner {
    /// A planner that knows no entities yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A planner primed with the corpus's entities.
    pub fn with_entities(known: KnownEntities) -> Self {
        Self { known }
    }

    /// Replace the entity table, e.g. after an index refresh.
    pub fn set_entities(&mut self, known: KnownEntities) {
        self.known = known;
    }

    /// The entity table in use.
    pub fn entities(&self) -> &KnownEntities {
        &self.known
    }

    /// Everything the rules recognise in `text`.
    pub fn signals(&self, text: &str) -> Vec<RetrievalSignal> {
        let lowered = text.to_lowercase();
        let normalized = normalize_token(text);
        let mut signals = Vec::new();

        for (surface, canonical) in self.known.matches_in(text) {
            let _ = surface;
            signals.push(RetrievalSignal::KnownEntity(canonical));
        }
        signals.dedup();

        if RECALL_PHRASES.iter().any(|p| lowered.contains(p)) {
            signals.push(RetrievalSignal::ExplicitRecall);
        }
        if RECOMMENDATION_PHRASES.iter().any(|p| lowered.contains(p)) {
            signals.push(RetrievalSignal::PersonalRecommendation);
        }
        if PRIOR_EVENT_PHRASES.iter().any(|p| lowered.contains(p)) {
            signals.push(RetrievalSignal::PriorEventReference);
        }
        if KINSHIP_TERMS.iter().any(|k| normalized.contains(k)) {
            signals.push(RetrievalSignal::RelationshipReference);
        }
        if COMPARISON_PHRASES.iter().any(|p| lowered.contains(p)) {
            signals.push(RetrievalSignal::Comparison);
        }
        if tokenize(text)
            .iter()
            .any(|t| PREFERENCE_WORDS.contains(&t.as_str()))
        {
            signals.push(RetrievalSignal::PreferencePredicate);
        }
        if let Some(label) = detect_temporal_label(&normalized) {
            signals.push(RetrievalSignal::TemporalRecall(label));
        }
        signals
    }

    /// Whether any recognised signal justifies bypassing the debounce.
    pub fn has_strong_signal(&self, text: &str) -> bool {
        self.signals(text).iter().any(RetrievalSignal::is_strong)
    }

    /// Build a plan from a transcript.
    pub fn plan(
        &self,
        text: &str,
        turn_id: TurnId,
        generation: u64,
        now: DateTime<Utc>,
    ) -> RetrievalPlan {
        let signals = self.signals(text);
        if signals.is_empty() {
            return RetrievalPlan::skip(turn_id, generation, text);
        }

        let mut entities: Vec<RetrievalEntity> = Vec::new();
        for (surface, canonical) in self.known.matches_in(text) {
            if entities
                .iter()
                .any(|e| e.canonical.as_deref() == Some(&canonical))
            {
                continue;
            }
            entities.push(RetrievalEntity::resolved(surface, canonical));
        }
        for kinship in KINSHIP_TERMS {
            if normalize_token(text).contains(kinship) {
                let surface = kinship.trim_start_matches("my ").to_string();
                if !entities.iter().any(|e| e.surface == surface) {
                    entities.push(RetrievalEntity::surface(surface));
                }
            }
        }

        let topics: Vec<String> = tokenize(text)
            .into_iter()
            .filter(|t| !NON_TOPICAL.contains(&t.as_str()))
            .filter(|t| t.len() > 2)
            .collect();

        let intent = infer_intent(&signals);
        let predicates = infer_predicates(&signals);
        let scopes = infer_scopes(&signals, intent);

        // Three independent queries, fused later: entities with topics finds
        // "what does Rhea like"; topics alone finds preference records that
        // never name an entity; the entity alone finds everything about them.
        let mut lexical_queries = Vec::new();
        let entity_terms: Vec<String> = entities.iter().map(|e| e.surface.clone()).collect();
        if !entity_terms.is_empty() && !topics.is_empty() {
            lexical_queries.push(format!("{} {}", entity_terms.join(" "), topics.join(" ")));
        }
        if !topics.is_empty() {
            lexical_queries.push(topics.join(" "));
        }
        if !entity_terms.is_empty() {
            lexical_queries.push(entity_terms.join(" "));
        }

        let temporal = signals.iter().find_map(|s| match s {
            RetrievalSignal::TemporalRecall(label) => Some(temporal_window(label, now)),
            _ => None,
        });

        let confidence = if signals.iter().any(RetrievalSignal::is_strong) {
            0.85
        } else {
            0.6
        };

        RetrievalPlan {
            plan_id: PlanId::generate(),
            turn_id,
            generation,
            requires_memory: intent.requires_memory(),
            confidence,
            intent,
            entities,
            topics,
            predicates,
            lexical_queries,
            scopes,
            kind_filter: Vec::new(),
            temporal,
            source_transcript_hash: stable_hash(text),
        }
        .normalized()
    }
}

fn infer_intent(signals: &[RetrievalSignal]) -> RetrievalIntent {
    // Ordered by how unambiguous each signal is about what the user wants.
    if signals.contains(&RetrievalSignal::ExplicitRecall) {
        return RetrievalIntent::ExplicitRecall;
    }
    if signals.contains(&RetrievalSignal::PersonalRecommendation) {
        return RetrievalIntent::PersonalRecommendation;
    }
    if signals.contains(&RetrievalSignal::Comparison) {
        return RetrievalIntent::Comparison;
    }
    if signals.contains(&RetrievalSignal::PriorEventReference) {
        return RetrievalIntent::PriorEventReference;
    }
    if signals.contains(&RetrievalSignal::RelationshipReference)
        || signals
            .iter()
            .any(|s| matches!(s, RetrievalSignal::KnownEntity(_)))
    {
        return RetrievalIntent::RelationshipReference;
    }
    if signals.contains(&RetrievalSignal::PreferencePredicate) {
        return RetrievalIntent::Ambient;
    }
    RetrievalIntent::None
}

fn infer_predicates(signals: &[RetrievalSignal]) -> Vec<CanonicalPredicate> {
    let mut predicates = Vec::new();
    if signals.contains(&RetrievalSignal::PreferencePredicate) {
        predicates.push(CanonicalPredicate::new("preference"));
    }
    if signals.contains(&RetrievalSignal::RelationshipReference) {
        predicates.push(CanonicalPredicate::new("relationship"));
    }
    predicates
}

fn infer_scopes(signals: &[RetrievalSignal], intent: RetrievalIntent) -> Vec<MemoryKind> {
    let mut scopes = Vec::new();
    match intent {
        RetrievalIntent::ExplicitRecall => {
            scopes.extend([
                MemoryKind::Identity,
                MemoryKind::Preference,
                MemoryKind::Relationship,
                MemoryKind::Routine,
            ]);
        }
        RetrievalIntent::PersonalRecommendation | RetrievalIntent::Comparison => {
            scopes.extend([
                MemoryKind::Preference,
                MemoryKind::RelationshipPreference,
                MemoryKind::LocationPreference,
            ]);
        }
        RetrievalIntent::PriorEventReference => {
            scopes.extend([MemoryKind::Episodic, MemoryKind::Commitment]);
        }
        RetrievalIntent::RelationshipReference => {
            scopes.extend([
                MemoryKind::Relationship,
                MemoryKind::RelationshipPreference,
                MemoryKind::Episodic,
            ]);
        }
        RetrievalIntent::Ambient => scopes.push(MemoryKind::Preference),
        RetrievalIntent::None => {}
    }
    if signals
        .iter()
        .any(|s| matches!(s, RetrievalSignal::TemporalRecall(_)))
        && !scopes.contains(&MemoryKind::Episodic)
    {
        scopes.push(MemoryKind::Episodic);
    }
    scopes
}

fn detect_temporal_label(normalized: &str) -> Option<String> {
    const LABELS: &[&str] = &[
        "yesterday",
        "last night",
        "this morning",
        "last week",
        "this week",
        "tonight",
        "tomorrow",
        "the other day",
    ];
    LABELS
        .iter()
        .find(|l| normalized.contains(*l))
        .map(|l| (*l).to_string())
}

fn temporal_window(label: &str, now: DateTime<Utc>) -> TemporalConstraint {
    let (after, before) = match label {
        "yesterday" | "last night" => (Some(now - Duration::days(2)), Some(now)),
        "this morning" | "tonight" => {
            (Some(now - Duration::days(1)), Some(now + Duration::days(1)))
        }
        "last week" => (Some(now - Duration::days(14)), Some(now)),
        "this week" => (Some(now - Duration::days(7)), Some(now + Duration::days(7))),
        "tomorrow" => (Some(now), Some(now + Duration::days(2))),
        _ => (Some(now - Duration::days(14)), Some(now)),
    };
    TemporalConstraint {
        after,
        before,
        label: Some(label.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planner() -> DeterministicPlanner {
        let mut known = KnownEntities::new();
        known.insert("Rhea", "rhea");
        known.insert("my wife", "rhea");
        known.insert("Kushal", "kushal");
        DeterministicPlanner::with_entities(known)
    }

    fn plan_for(text: &str) -> RetrievalPlan {
        planner().plan(text, TurnId(1), 1, Utc::now())
    }

    #[test]
    fn a_generic_factual_question_skips_memory_entirely() {
        let plan = plan_for("what is the capital of France");
        assert!(!plan.requires_memory);
        assert!(plan.lexical_queries.is_empty());
    }

    #[test]
    fn an_explicit_recall_request_is_recognised() {
        let plan = plan_for("what do you remember about my dietary preferences");
        assert!(plan.requires_memory);
        assert_eq!(plan.intent, RetrievalIntent::ExplicitRecall);
        assert!(plan.scopes.contains(&MemoryKind::Preference));
    }

    #[test]
    fn a_personal_recommendation_pulls_preference_scopes() {
        let plan = plan_for("where should we eat dinner tonight");
        assert_eq!(plan.intent, RetrievalIntent::PersonalRecommendation);
        assert!(plan.scopes.contains(&MemoryKind::Preference));
        assert!(plan.topics.contains(&"dinner".to_string()));
    }

    #[test]
    fn known_entities_are_resolved_from_their_aliases() {
        let plan = plan_for("book a table for my wife");
        let resolved: Vec<_> = plan
            .entities
            .iter()
            .filter_map(|e| e.canonical.as_deref())
            .collect();
        assert!(resolved.contains(&"rhea"), "got {:?}", plan.entities);
    }

    #[test]
    fn an_unknown_kinship_term_still_becomes_an_entity() {
        let plan = plan_for("what does my brother like to drink");
        assert!(plan.entities.iter().any(|e| e.surface == "brother"));
        assert_eq!(plan.intent, RetrievalIntent::RelationshipReference);
    }

    #[test]
    fn prior_event_references_scope_to_episodes_with_a_time_window() {
        let plan = plan_for("what happened at dinner last week");
        assert!(plan.scopes.contains(&MemoryKind::Episodic));
        let temporal = plan.temporal.expect("a time window");
        assert_eq!(temporal.label.as_deref(), Some("last week"));
        assert!(temporal.after.is_some() && temporal.before.is_some());
    }

    #[test]
    fn several_independent_queries_are_produced_for_fusion() {
        let plan = plan_for("what restaurants does Rhea like");
        assert!(plan.lexical_queries.len() >= 2);
        assert!(plan
            .lexical_queries
            .iter()
            .any(|q| q.contains("rhea") || q.to_lowercase().contains("rhea")));
    }

    #[test]
    fn a_hinglish_recall_request_is_recognised() {
        let plan = plan_for("Mujhe yaad dilao, mera khaana ka preference kya hai?");
        assert!(
            plan.requires_memory,
            "a Hinglish recall request was read as needing no memory"
        );
        assert_eq!(plan.intent, RetrievalIntent::ExplicitRecall);
        assert!(
            plan.topics.contains(&"khaana".to_string()),
            "the one content word was dropped: {:?}",
            plan.topics
        );
        assert!(
            !plan.topics.contains(&"hai".to_string()),
            "a Hindi copula was treated as a topic: {:?}",
            plan.topics
        );
    }

    #[test]
    fn a_hinglish_kinship_term_is_an_entity() {
        let plan = plan_for("Meri wife ko kaunsa restaurant pasand hai?");
        assert!(plan.requires_memory);
        assert!(
            plan.entities
                .iter()
                .any(|e| e.canonical.as_deref() == Some("rhea") || e.surface.contains("wife")),
            "no entity resolved from a Hinglish possessive: {:?}",
            plan.entities
        );
    }

    #[test]
    fn a_tanglish_question_is_recognised() {
        let plan = plan_for("Enakku enna coffee pidikkum theriyuma?");
        assert!(
            plan.requires_memory,
            "a Tanglish recall request was read as needing no memory"
        );
        assert!(plan.topics.contains(&"coffee".to_string()));
    }

    #[test]
    fn strong_signals_bypass_the_speculation_debounce() {
        let planner = planner();
        assert!(planner.has_strong_signal("tell me about Rhea"));
        assert!(planner.has_strong_signal("do you remember what I said"));
        assert!(!planner.has_strong_signal("it is quite warm today"));
    }

    #[test]
    fn entities_are_learned_from_the_corpus_not_a_name_list() {
        let index = MemoryIndex::new();
        assert!(KnownEntities::from_index(&index).is_empty());

        let mut known = KnownEntities::new();
        known.insert("Rhea", "rhea");
        assert_eq!(known.resolve("rhea"), Some("rhea"));
        assert_eq!(known.resolve("RHEA"), Some("rhea"));
        assert_eq!(known.resolve("Someone Else"), None);
    }

    #[test]
    fn entity_matching_respects_word_boundaries() {
        let mut known = KnownEntities::new();
        known.insert("ann", "ann");
        let planner = DeterministicPlanner::with_entities(known);
        // "annoying" must not match the entity "ann".
        assert!(!planner
            .signals("that was annoying")
            .iter()
            .any(|s| matches!(s, RetrievalSignal::KnownEntity(_))));
        assert!(planner
            .signals("ann called")
            .iter()
            .any(|s| matches!(s, RetrievalSignal::KnownEntity(_))));
    }
}
