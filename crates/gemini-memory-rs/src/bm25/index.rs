//! A fielded BM25 index over a single user's memory.
//!
//! ## Why this rather than a search engine
//!
//! A personal memory corpus is hundreds to low thousands of short records. At
//! that size an in-process inverted index answers in tens of microseconds,
//! rebuilds from the OKF corpus in milliseconds, and needs no segment
//! management, no writer lifecycle, and no crash-recovery story of its own —
//! the canonical Markdown *is* the recovery story. The index is a derived,
//! disposable artefact by design (§6.1), and treating it that way removes a
//! whole class of "index disagrees with corpus" failures.
//!
//! The [`MemoryIndex`] surface is intentionally narrow so a segment-based
//! backend can be substituted later without touching callers.

use chrono::{DateTime, Utc};
use std::collections::HashMap;

use super::explain::{BoostKind, ScoreComponent, SearchExplanation};
use super::schema::{Field, IndexedMemory, MemoryOrigin, tokenize};
use crate::core::{MemoryId, MemoryKind};

/// BM25 term-frequency saturation.
const K1: f32 = 1.2;
/// BM25 length normalization.
const B: f32 = 0.75;

/// One term occurrence set within one document field.
#[derive(Debug, Clone, Copy)]
struct Posting {
    doc: usize,
    field: Field,
    tf: u32,
}

/// What a search is looking for.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Raw query text; tokenized on execution.
    pub text: String,
    /// Entity surface forms that should receive an exact-match boost.
    pub entities: Vec<String>,
    /// Restrict to these kinds; empty means any.
    pub kinds: Vec<MemoryKind>,
    /// Terms that may *rank* a hit but never *admit* one.
    ///
    /// A memory lookup is nearly always phrased about its owner — "what's my
    /// usual coffee", or, when a model writes the query, "the user's usual
    /// coffee". Those words are the corpus's own subject form and its aliases,
    /// so they have a posting in almost every record. Treated as ordinary
    /// terms they cost two things at once: precision, because a question the
    /// corpus cannot answer still matches everything and comes back with five
    /// arbitrary facts; and time, because every query then walks the whole
    /// corpus and sorts it. Measured, that was a recall going from 1 ms at 250
    /// records to 118 ms at 16,000 — superlinear, and eight times past the
    /// engine's own 15 ms lexical deadline.
    ///
    /// Listing them here keeps what they are good for and drops what they are
    /// not. They still contribute their score to a record some *topical* term
    /// already matched, which is what lets "what coating do I have" prefer the
    /// owner's record over forty belonging to other people. They no longer put
    /// a record into the running on their own.
    pub boost_only: Vec<String>,
    /// Maximum hits to return.
    pub limit: usize,
}

impl Query {
    /// A plain text query.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            entities: Vec::new(),
            kinds: Vec::new(),
            boost_only: Vec::new(),
            limit: 20,
        }
    }

    /// Boost documents whose subject or entities match these surface forms.
    pub fn with_entities<I, S>(mut self, entities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.entities = entities
            .into_iter()
            .map(|e| crate::core::normalize_token(&e.into()))
            .filter(|e| !e.is_empty())
            .collect();
        self
    }

    /// Restrict the search to certain memory kinds.
    pub fn with_kinds(mut self, kinds: Vec<MemoryKind>) -> Self {
        self.kinds = kinds;
        self
    }

    /// Mark terms that may rank a hit but never admit one.
    pub fn with_boost_only<I, S>(mut self, terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.boost_only = terms.into_iter().map(Into::into).collect();
        self
    }

    /// Cap the result count.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// A scored match.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The matched record.
    pub id: MemoryId,
    /// Final score after boosts.
    pub score: f32,
    /// The sentence to show the model.
    pub statement: String,
    /// The kind of memory matched.
    pub kind: MemoryKind,
    /// Where the record came from.
    pub origin: MemoryOrigin,
    /// Why it scored what it did.
    pub explanation: SearchExplanation,
}

/// An in-process fielded BM25 index.
#[derive(Debug, Default)]
pub struct MemoryIndex {
    docs: Vec<Option<IndexedMemory>>,
    by_id: HashMap<MemoryId, usize>,
    postings: HashMap<String, Vec<Posting>>,
    doc_frequency: HashMap<String, u32>,
    field_length_total: [u64; 7],
    live_docs: usize,
    revision: u64,
}

impl MemoryIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build an index from a set of documents.
    pub fn build(documents: impl IntoIterator<Item = IndexedMemory>) -> Self {
        let mut index = Self::new();
        for doc in documents {
            index.upsert(doc);
        }
        index
    }

    /// The index revision, bumped on every mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// How many live documents the index holds.
    pub fn len(&self) -> usize {
        self.live_docs
    }

    /// Whether the index holds no live documents.
    pub fn is_empty(&self) -> bool {
        self.live_docs == 0
    }

    /// Insert or replace a document.
    pub fn upsert(&mut self, doc: IndexedMemory) {
        self.remove(&doc.id);

        let slot = self.docs.len();
        let mut seen_terms: HashMap<&str, ()> = HashMap::new();
        for field in Field::ALL {
            let tokens = &doc.fields[field.slot()];
            self.field_length_total[field.slot()] += tokens.len() as u64;
            let mut counts: HashMap<&str, u32> = HashMap::new();
            for token in tokens {
                *counts.entry(token.as_str()).or_insert(0) += 1;
            }
            for (term, tf) in counts {
                self.postings
                    .entry(term.to_string())
                    .or_default()
                    .push(Posting {
                        doc: slot,
                        field,
                        tf,
                    });
                seen_terms.insert(term, ());
            }
        }
        for term in seen_terms.keys() {
            *self.doc_frequency.entry((*term).to_string()).or_insert(0) += 1;
        }

        self.by_id.insert(doc.id.clone(), slot);
        self.docs.push(Some(doc));
        self.live_docs += 1;
        self.revision += 1;
    }

    /// Remove a document. Removing a missing document is a no-op.
    pub fn remove(&mut self, id: &MemoryId) -> bool {
        let Some(slot) = self.by_id.remove(id) else {
            return false;
        };
        let Some(doc) = self.docs[slot].take() else {
            return false;
        };

        let mut seen: HashMap<&str, ()> = HashMap::new();
        for field in Field::ALL {
            let tokens = &doc.fields[field.slot()];
            self.field_length_total[field.slot()] -= tokens.len() as u64;
            for token in tokens {
                seen.insert(token.as_str(), ());
            }
        }
        for term in seen.keys() {
            if let Some(df) = self.doc_frequency.get_mut(*term) {
                *df = df.saturating_sub(1);
            }
            if let Some(postings) = self.postings.get_mut(*term) {
                postings.retain(|p| p.doc != slot);
            }
        }

        self.live_docs -= 1;
        self.revision += 1;
        true
    }

    /// Drop every document.
    pub fn clear(&mut self) {
        let revision = self.revision + 1;
        *self = Self::default();
        self.revision = revision;
    }

    /// Fetch an indexed document.
    pub fn get(&self, id: &MemoryId) -> Option<&IndexedMemory> {
        self.by_id
            .get(id)
            .and_then(|slot| self.docs[*slot].as_ref())
    }

    /// Every live document.
    pub fn documents(&self) -> impl Iterator<Item = &IndexedMemory> {
        self.docs.iter().filter_map(|d| d.as_ref())
    }

    fn average_field_length(&self, field: Field) -> f32 {
        if self.live_docs == 0 {
            return 1.0;
        }
        let total = self.field_length_total[field.slot()] as f32;
        (total / self.live_docs as f32).max(1.0)
    }

    fn idf(&self, term: &str) -> f32 {
        let df = f32::from(*self.doc_frequency.get(term).unwrap_or(&0) as u16);
        let n = self.live_docs as f32;
        // Lucene-style BM25 idf: always positive, so a term present in every
        // document contributes little rather than negatively.
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    /// Run a query at time `now`.
    pub fn search(&self, query: &Query, now: DateTime<Utc>) -> Vec<SearchHit> {
        let terms = tokenize(&query.text);
        if terms.is_empty() && query.entities.is_empty() {
            return Vec::new();
        }

        // Two passes. Terms that carry topic put records into the running;
        // terms that only say *whose* memory this is are scored afterwards,
        // against the records already there. See `Query::boost_only`.
        let (admitting, ranking): (Vec<&String>, Vec<&String>) = terms
            .iter()
            .partition(|term| !query.boost_only.iter().any(|b| b == *term));

        let mut accumulated: HashMap<usize, Vec<ScoreComponent>> = HashMap::new();
        for (term, admits) in admitting
            .iter()
            .map(|t| (*t, true))
            .chain(ranking.iter().map(|t| (*t, false)))
        {
            let Some(postings) = self.postings.get(term) else {
                continue;
            };
            let idf = self.idf(term);
            for posting in postings {
                if !admits && !accumulated.contains_key(&posting.doc) {
                    continue;
                }
                let Some(doc) = self.docs[posting.doc].as_ref() else {
                    continue;
                };
                if !doc.is_retrievable(now) {
                    continue;
                }
                if !query.kinds.is_empty() && !query.kinds.contains(&doc.kind) {
                    continue;
                }
                let avg = self.average_field_length(posting.field);
                let len = doc.field_len(posting.field) as f32;
                let tf = posting.tf as f32;
                let norm = tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg));
                let contribution = idf * norm * posting.field.weight();
                accumulated
                    .entry(posting.doc)
                    .or_default()
                    .push(ScoreComponent {
                        term: term.clone(),
                        field: posting.field,
                        score: contribution,
                    });
            }
        }

        // An entity-only query (a bare "what about Rhea") still has to find the
        // records about that entity, so exact entity matches seed candidates
        // rather than only boosting existing ones.
        if !query.entities.is_empty() {
            for (slot, doc) in self.docs.iter().enumerate() {
                let Some(doc) = doc else { continue };
                if !doc.is_retrievable(now) {
                    continue;
                }
                if !query.kinds.is_empty() && !query.kinds.contains(&doc.kind) {
                    continue;
                }
                if query
                    .entities
                    .iter()
                    .any(|e| e == &doc.subject_form || doc.entity_forms.contains(e))
                {
                    accumulated.entry(slot).or_default();
                }
            }
        }

        let mut hits: Vec<SearchHit> = accumulated
            .into_iter()
            .filter_map(|(slot, components)| {
                let doc = self.docs[slot].as_ref()?;
                let lexical: f32 = components.iter().map(|c| c.score).sum();
                let mut explanation = SearchExplanation {
                    memory_id: doc.id.clone(),
                    components,
                    boosts: Vec::new(),
                    lexical_score: lexical,
                    final_score: lexical,
                };
                let score = apply_boosts(doc, query, now, lexical, &mut explanation);
                explanation.final_score = score;
                Some(SearchHit {
                    id: doc.id.clone(),
                    score,
                    statement: doc.statement.clone(),
                    kind: doc.kind,
                    origin: doc.origin,
                    explanation,
                })
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        hits.truncate(if query.limit == 0 { 20 } else { query.limit });
        hits
    }
}

/// Score a document that matched only by entity, with no lexical overlap.
///
/// Naming an entity is itself evidence of relevance, so an entity match seeds a
/// baseline score. Without it, "what about Rhea?" would score zero against
/// every record about Rhea.
const ENTITY_BASE: f32 = 2.0;

/// Half-life, in days, of an episodic record's relevance.
const RECENCY_HALF_LIFE_DAYS: f32 = 14.0;

/// Apply the non-lexical ranking signals (§13.3).
///
/// Signals are *multiplicative* on the lexical score rather than additive.
/// Additive boosts sound simpler but are unstable: BM25's IDF term shrinks as a
/// term becomes common, so a fixed `+1.5` can outweigh the entire relevance
/// signal and rank a recent-but-irrelevant record above an exact match.
/// Multiplying keeps relevance in charge and lets the signals modulate it.
///
/// The explanation still records each signal as the delta it actually caused,
/// so a rendered derivation adds up to the final score.
fn apply_boosts(
    doc: &IndexedMemory,
    query: &Query,
    now: DateTime<Utc>,
    lexical: f32,
    explanation: &mut SearchExplanation,
) -> f32 {
    let mut score = lexical;

    let exact_entity = query
        .entities
        .iter()
        .any(|e| e == &doc.subject_form || doc.entity_forms.contains(e));
    if exact_entity {
        score += ENTITY_BASE;
        explanation
            .boosts
            .push((BoostKind::ExactEntity, ENTITY_BASE));
    }

    let mut apply = |kind: BoostKind, factor: f32, score: &mut f32| {
        let before = *score;
        *score *= factor;
        explanation.boosts.push((kind, *score - before));
    };

    if doc.explicit {
        apply(BoostKind::ExplicitSource, 1.1, &mut score);
    }
    if doc.origin == MemoryOrigin::SessionOverlay {
        // Something the user said moments ago outranks a months-old record of
        // the same thing.
        apply(BoostKind::SessionOverlay, 1.5, &mut score);
    }

    let confidence = doc.confidence.clamp(0.0, 1.0);
    apply(BoostKind::Confidence, 0.7 + 0.6 * confidence, &mut score);
    if confidence < 0.5 {
        apply(BoostKind::LowConfidencePenalty, 0.7, &mut score);
    }

    // Episodic relevance decays; semantic memory does not.
    if doc.kind.is_episodic() || doc.temporal_scope != crate::core::TemporalScope::Persistent {
        let age_days = (now - doc.valid_from).num_days().max(0) as f32;
        let factor = 0.5 + 0.5 * (-age_days / RECENCY_HALF_LIFE_DAYS).exp();
        apply(BoostKind::Recency, factor, &mut score);
    }

    score.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::schema::IndexedMemory;
    use crate::core::{
        CanonicalMemory, CanonicalPredicate, EntityRef, EvidenceCounters, Explicitness,
        MemorySource, MemoryStatus, MemoryValue, PrivacyMetadata, RetrievalMetadata, SessionId,
        TemporalMetadata, TemporalScope, TurnId, UserId,
    };

    fn record(
        id: &str,
        kind: MemoryKind,
        subject: &str,
        statement: &str,
        tags: &[&str],
    ) -> CanonicalMemory {
        CanonicalMemory {
            id: MemoryId::new(id),
            owner: UserId::new("usr_1"),
            kind,
            predicate: CanonicalPredicate::new("venue_preference"),
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

    fn corpus() -> MemoryIndex {
        MemoryIndex::build([
            IndexedMemory::from_canonical(&record(
                "mem_quiet",
                MemoryKind::RelationshipPreference,
                "Rhea",
                "Rhea prefers quiet restaurants.",
                &["restaurant", "noise"],
            )),
            IndexedMemory::from_canonical(&record(
                "mem_diet",
                MemoryKind::Preference,
                "user",
                "The user is pescatarian.",
                &["food", "diet"],
            )),
            IndexedMemory::from_canonical(&record(
                "mem_music",
                MemoryKind::Preference,
                "user",
                "The user enjoys live music venues with friends.",
                &["music", "venue"],
            )),
        ])
    }

    #[test]
    fn finds_the_relevant_record_and_ranks_it_first() {
        let index = corpus();
        let hits = index.search(&Query::new("quiet restaurant"), Utc::now());
        assert_eq!(hits.first().map(|h| h.id.as_str()), Some("mem_quiet"));
    }

    #[test]
    fn a_query_with_no_matching_terms_returns_nothing() {
        let index = corpus();
        assert!(
            index
                .search(&Query::new("quantum chromodynamics"), Utc::now())
                .is_empty()
        );
        assert!(index.search(&Query::new(""), Utc::now()).is_empty());
    }

    #[test]
    fn an_entity_only_query_still_finds_that_entitys_records() {
        let index = corpus();
        let hits = index.search(&Query::new("").with_entities(["Rhea"]), Utc::now());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id.as_str(), "mem_quiet");
        assert!(
            hits[0]
                .explanation
                .boosts
                .iter()
                .any(|(kind, _)| *kind == BoostKind::ExactEntity)
        );
    }

    #[test]
    fn subject_matches_outrank_statement_matches() {
        let mut index = MemoryIndex::new();
        index.upsert(IndexedMemory::from_canonical(&record(
            "mem_subject",
            MemoryKind::Relationship,
            "Kushal",
            "A colleague.",
            &[],
        )));
        index.upsert(IndexedMemory::from_canonical(&record(
            "mem_mention",
            MemoryKind::Episodic,
            "user",
            "The user had dinner and mentioned Kushal in passing.",
            &[],
        )));
        let hits = index.search(&Query::new("kushal"), Utc::now());
        assert_eq!(hits[0].id.as_str(), "mem_subject");
    }

    #[test]
    fn session_overlay_facts_outrank_equivalent_canonical_ones() {
        let canonical = IndexedMemory::from_canonical(&record(
            "mem_old",
            MemoryKind::Preference,
            "user",
            "The user is vegetarian.",
            &["diet"],
        ));
        let overlay = IndexedMemory::from_canonical(&record(
            "mem_new",
            MemoryKind::Preference,
            "user",
            "The user is vegetarian now pescatarian.",
            &["diet"],
        ))
        .as_session_overlay();
        let index = MemoryIndex::build([canonical, overlay]);

        let hits = index.search(&Query::new("vegetarian diet"), Utc::now());
        assert_eq!(hits[0].id.as_str(), "mem_new");
    }

    #[test]
    fn superseded_and_expired_records_never_surface() {
        let mut superseded = IndexedMemory::from_canonical(&record(
            "mem_old",
            MemoryKind::Preference,
            "user",
            "The user is vegetarian.",
            &["diet"],
        ));
        superseded.status = MemoryStatus::Superseded;

        let mut expired = IndexedMemory::from_canonical(&record(
            "mem_gone",
            MemoryKind::Episodic,
            "user",
            "The user slept badly.",
            &["sleep"],
        ));
        expired.expires_at = Some(Utc::now() - chrono::Duration::hours(1));

        let index = MemoryIndex::build([superseded, expired]);
        assert!(
            index
                .search(&Query::new("vegetarian"), Utc::now())
                .is_empty()
        );
        assert!(index.search(&Query::new("sleep"), Utc::now()).is_empty());
    }

    #[test]
    fn removing_a_record_removes_it_from_results_and_statistics() {
        let mut index = corpus();
        assert_eq!(index.len(), 3);
        assert!(index.remove(&MemoryId::new("mem_quiet")));
        assert_eq!(index.len(), 2);
        assert!(
            index
                .search(&Query::new("quiet restaurant"), Utc::now())
                .is_empty()
        );
        // Removing again is a no-op rather than a corruption.
        assert!(!index.remove(&MemoryId::new("mem_quiet")));
    }

    #[test]
    fn upserting_replaces_rather_than_duplicates() {
        let mut index = corpus();
        let revised = IndexedMemory::from_canonical(&record(
            "mem_diet",
            MemoryKind::Preference,
            "user",
            "The user is pescatarian and avoids shellfish.",
            &["food", "diet"],
        ));
        index.upsert(revised);
        assert_eq!(index.len(), 3);
        let hits = index.search(&Query::new("shellfish"), Utc::now());
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn recent_episodes_outrank_stale_ones() {
        let now = Utc::now();
        let mut fresh = IndexedMemory::from_canonical(&record(
            "mem_fresh",
            MemoryKind::Episodic,
            "user",
            "Dinner in Bandra was too noisy.",
            &["restaurant"],
        ));
        fresh.temporal_scope = TemporalScope::RecentHistory;
        fresh.valid_from = now - chrono::Duration::days(1);

        let mut stale = IndexedMemory::from_canonical(&record(
            "mem_stale",
            MemoryKind::Episodic,
            "user",
            "Dinner in Bandra was too noisy.",
            &["restaurant"],
        ));
        stale.temporal_scope = TemporalScope::RecentHistory;
        stale.valid_from = now - chrono::Duration::days(60);

        let index = MemoryIndex::build([fresh, stale]);
        let hits = index.search(&Query::new("noisy dinner bandra"), now);
        assert_eq!(hits[0].id.as_str(), "mem_fresh");
    }

    #[test]
    fn kind_filters_restrict_the_search_scope() {
        let index = corpus();
        let hits = index.search(
            &Query::new("user").with_kinds(vec![MemoryKind::RelationshipPreference]),
            Utc::now(),
        );
        assert!(
            hits.iter()
                .all(|h| h.kind == MemoryKind::RelationshipPreference)
        );
    }

    #[test]
    fn an_explanation_adds_up_to_the_score_it_explains() {
        let index = corpus();
        for hit in index.search(
            &Query::new("quiet restaurant").with_entities(["Rhea"]),
            Utc::now(),
        ) {
            let summed: f32 = hit.explanation.lexical_score
                + hit.explanation.boosts.iter().map(|(_, d)| d).sum::<f32>();
            assert!(
                (summed - hit.score).abs() < 1e-4,
                "explanation for {} sums to {summed}, score is {}",
                hit.id,
                hit.score
            );
        }
    }

    #[test]
    fn scores_are_never_negative_and_always_explained() {
        let index = corpus();
        for hit in index.search(&Query::new("restaurant diet music"), Utc::now()) {
            assert!(hit.score >= 0.0);
            assert!(!hit.explanation.components.is_empty());
            assert_eq!(hit.explanation.final_score, hit.score);
        }
    }
}
