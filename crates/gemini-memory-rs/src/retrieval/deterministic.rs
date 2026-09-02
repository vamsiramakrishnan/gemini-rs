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
use crate::bm25::{MemoryIndex, tokenize};
use crate::core::{CanonicalPredicate, MemoryKind, PlanId, TurnId, normalize_token, stable_hash};

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
/// These tables are *hints*, and English-only on purpose. An earlier version
/// tried to make them exhaustive — Hindi and Tamil phrase lists, romanized
/// stop words, a kinship table — because the planner used them to decide
/// whether to search at all, so a gap in the list meant a Hinglish question
/// got no memory. That put the rule planner in the business of understanding
/// language, which is not a business a phrase table can be in: there is no
/// list length at which "mujhe yaad dilao" and "enakku theriyuma" and the next
/// thousand ways to ask are all covered.
///
/// So the decision moved. Whether to search is answered by "are there content
/// words", and what comes back is answered by BM25 and the score threshold —
/// both of which are language-agnostic, because a term the corpus does not
/// contain simply has no postings. What a matched phrase now buys is a
/// slightly better-shaped plan: an intent label, a scope preference, a
/// predicate guess. Missing one costs a little ranking quality on that turn.
/// It no longer costs the memory.
const RECALL_PHRASES: &[&str] = &[
    "do you remember",
    "what do you remember",
    "what do you know about",
    "remind me",
    "did i tell you",
    "i told you",
    "you said",
    "have i mentioned",
];

const RECOMMENDATION_PHRASES: &[&str] = &[
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

/// English kinship terms, kept only so `RelationshipReference` can be labelled
/// for scoping. Detection is not required for retrieval: a relationship in the
/// corpus is found by its own aliases.
const KINSHIP_TERMS: &[&str] = &[
    "my wife",
    "my husband",
    "my partner",
    "my mother",
    "my father",
    "my son",
    "my daughter",
    "my sister",
    "my brother",
    "my friend",
    "my colleague",
    "my boss",
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
    "goes",
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
    "says",
    "said",
    "preference",
    "like",
    "love",
    "hate",
    "prefer",
    // The corpus's own subject form.
    //
    // Every record about the user carries `user` in its subject field, which
    // is weighted 3.0 — so as a query term it matches most of the corpus and
    // discriminates within none of it. It reaches queries at all because the
    // `recall_context` argument is written by the model, which naturally
    // phrases a memory lookup in the third person ("the user's usual coffee
    // order"). Left in, a question memory has no answer to still clears the
    // score floor on the strength of the word "user" alone, and the model is
    // handed five arbitrary facts to improvise from.
    "user",
];

/// The topical terms of an utterance or query.
///
/// What survives after function words, the vocabulary of asking, and the
/// corpus's own subject form are removed. Short tokens go too: at two
/// characters a token is almost always a fragment of a contraction.
///
/// Shared by the planner and by the synchronous tool path so the two cannot
/// drift into disagreeing about what a query is made of.
pub fn topical_terms(text: &str) -> Vec<String> {
    tokenize(text)
        .into_iter()
        .filter(|t| !NON_TOPICAL.contains(&t.as_str()))
        .filter(|t| t.len() > 2)
        .collect()
}

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

        // Content words are the gate, not recognised phrases.
        //
        // The previous rule — "no recognised signal, no memory" — made the
        // planner responsible for understanding language, which it cannot do.
        // It answered "no memory needed" to any phrasing outside its word
        // lists, which meant every language but English.
        //
        // Searching locally costs tens of microseconds. Searching and finding
        // nothing is the same observable outcome as not searching, minus the
        // failure mode. So the default is to search, and the score threshold in
        // the assembler decides whether anything comes back. Deciding a query
        // needs no memory at all is left to the model planner, which
        // understands the sentence.
        let topics: Vec<String> = topical_terms(text);

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

        // No kinship table. A relationship the engine has never heard of has no
        // memories to retrieve, so failing to spot it costs nothing; one it has
        // heard of is in the corpus already, carrying the aliases the
        // extraction model wrote in whatever language the user used.

        if topics.is_empty() && entities.is_empty() {
            return RetrievalPlan::skip(turn_id, generation, text);
        }

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
            subject_hint: None,
            predicate_hint: None,
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
    // Ambient rather than None: the caller has already decided there is
    // something to search for, so the intent only chooses scoping hints.
    RetrievalIntent::Ambient
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
        // A corpus-derived alias, not a kinship table: the fact "Rhea is the
        // user's wife" lists both forms as entities, so both land here.
        known.insert("wife", "rhea");
        known.insert("Kushal", "kushal");
        DeterministicPlanner::with_entities(known)
    }

    fn plan_for(text: &str) -> RetrievalPlan {
        planner().plan(text, TurnId(1), 1, Utc::now())
    }

    #[test]
    fn a_generic_factual_question_carries_only_its_content_words() {
        // The planner no longer rules on whether a question "needs memory" —
        // it cannot know that without understanding the sentence. It strips
        // function words and hands the residue to BM25, which scores a
        // world-knowledge question against a personal corpus at nothing. The
        // skip lives in the score threshold, where it can be right.
        let plan = plan_for("what is the capital of France");
        assert_eq!(
            plan.topics,
            vec!["capital".to_string(), "france".to_string()]
        );
        assert!(plan.entities.is_empty());
    }

    #[test]
    fn an_utterance_with_no_content_words_skips_memory_entirely() {
        // Nothing to search *with* is the one case the planner can call, and
        // it is a lexical fact rather than a semantic judgement.
        let plan = plan_for("what do you think");
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
    fn a_relationship_the_corpus_never_heard_of_is_just_a_topic() {
        // There is no kinship table, and none is needed. A brother the engine
        // has never been told about has no memories to retrieve, so failing to
        // classify him as an entity costs exactly nothing; the word still goes
        // to the index as a topic, where it will match the moment a fact about
        // him exists — in whatever language that fact was spoken.
        let plan = plan_for("what does my brother like to drink");
        assert!(plan.entities.is_empty());
        assert!(plan.topics.contains(&"brother".to_string()));
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
        assert!(
            plan.lexical_queries
                .iter()
                .any(|q| q.contains("rhea") || q.to_lowercase().contains("rhea"))
        );
    }

    #[test]
    fn a_hinglish_question_reaches_the_index_without_a_hindi_word_list() {
        // There is no Hindi phrase table here. "yaad dilao" is not recognised
        // as a recall verb, and does not need to be — the content words go to
        // the index, and the facts stored from Hinglish speech carry Hinglish
        // search terms to meet them.
        let plan = plan_for("Mujhe yaad dilao, mera khaana ka preference kya hai?");
        assert!(plan.requires_memory);
        assert!(
            plan.topics.contains(&"khaana".to_string()),
            "the content word was dropped: {:?}",
            plan.topics
        );
        // "hai" survives the filter, and that is fine: a term no document
        // contains has no postings, scores nothing, and costs nothing. Paying
        // a stop-word list to remove it would buy exactly zero.
        assert!(!plan.lexical_queries.is_empty());
    }

    #[test]
    fn a_hinglish_possessive_still_resolves_a_corpus_entity() {
        let plan = plan_for("Meri wife ko kaunsa restaurant pasand hai?");
        assert!(plan.requires_memory);
        assert!(
            plan.entities
                .iter()
                .any(|e| e.canonical.as_deref() == Some("rhea")),
            "an entity the corpus knows was not resolved: {:?}",
            plan.entities
        );
    }

    #[test]
    fn a_tanglish_question_reaches_the_index() {
        let plan = plan_for("Enakku enna coffee pidikkum theriyuma?");
        assert!(plan.requires_memory);
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
        assert!(
            !planner
                .signals("that was annoying")
                .iter()
                .any(|s| matches!(s, RetrievalSignal::KnownEntity(_)))
        );
        assert!(
            planner
                .signals("ann called")
                .iter()
                .any(|s| matches!(s, RetrievalSignal::KnownEntity(_)))
        );
    }
}
