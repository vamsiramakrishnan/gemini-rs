//! Turning finalized user speech into candidate observations.
//!
//! This is a different question from retrieval planning, asked by a different
//! call with a different schema: *did the user just reveal something worth
//! keeping?* Conflating the two produces a model that stores what it was asked
//! to recall.
//!
//! Extraction never runs on a partial transcript. A partial may be revised, and
//! evidence that can be revised is not evidence.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;

use crate::core::{
    CanonicalPredicate, EntityRef, Explicitness, MemoryError, MemoryKind, MemoryObservation,
    MemoryValue, MutationIntent, ObservationId, ProposedPersistence, SensitivityClass, SessionId,
    SpeakerAttribution, TemporalScope, TranscriptEvidence, TurnId, normalize_token,
};

/// The system instruction for the observation extractor.
pub const OBSERVATION_EXTRACTION_INSTRUCTION: &str = "\
You extract candidate memories from a single finalized user utterance for a \
personal memory system.

Rules:
- Only extract things the USER said about themselves or their life. Never \
  extract from the assistant's turns, and never from speech attributed to \
  anyone else.
- Distinguish durable facts (preferences, relationships, identity, routines) \
  from time-bounded events (plans, moods, one-off situations). Mark the latter \
  episodic with an expected expiry.
- Mark a transient feeling or a passing remark as session-only or discard it.
- Set explicitness honestly. If the user did not say it, it is an inference, \
  however obvious it seems.
- Never infer sensitive attributes (health, religion, politics, sexuality) the \
  user did not state outright.
- Recognise explicit memory commands ('remember that…', 'forget…', 'actually, \
  I…') and set mutation_intent accordingly.
- Reuse a predicate from the 'Predicates already in use' list whenever the new \
  fact is about the same thing, even when it CONTRADICTS the stored one. A \
  correction must land on the same predicate as the fact it corrects, or it \
  becomes a second record instead of replacing the first. Invent a new \
  predicate only when nothing in the list covers the fact.
- Write the fact itself in English, always, whatever language the user spoke. \
  statement, predicate, value, subject and qualifier are the canonical record: \
  one user saying 'main vegetarian hoon', 'naan vegetarian', and 'I am \
  vegetarian' must produce the SAME predicate and the SAME value, so that the \
  three reinforce one memory instead of creating three.
- search_terms are the exception and must NOT be normalized to English. They \
  are not a transcription of this sentence — they are your guess at the words \
  a FUTURE QUESTION would use. Write 4-8 of them: the topic word in the user's \
  language even when this sentence never used it, its English equivalent, and \
  the obvious synonyms. A user who says 'main vegetarian hoon' will later ask \
  'mera khaana ka preference kya hai' or 'what do I eat', so this fact needs \
  khaana, khana, food, diet, eat — not just the words hoon and khata that \
  happen to appear above.
- Return an empty list when the utterance reveals nothing worth keeping. That \
  is the common case.";

/// What the observation extractor is given.
#[derive(Debug, Clone)]
pub struct ObservationExtractionContext {
    /// The finalized user utterance.
    pub transcript: String,
    /// Preceding user turns, for pronoun resolution.
    pub recent_user_turns: Vec<String>,
    /// The preceding assistant turn, for reference resolution only.
    pub recent_assistant_turn: Option<String>,
    /// Predicates already in use in this user's corpus, most-used first.
    ///
    /// Reconciliation matches on subject and predicate, so a correction only
    /// supersedes the fact it corrects when the two agree on a name. Left to
    /// invent one per call, the model writes `dietary_preference` on Monday
    /// and `dietary_identity` on Tuesday, and the correction becomes a second
    /// active record instead of replacing the first. Showing it the names
    /// already in use is the same move as learning entities from the corpus:
    /// the vocabulary comes from the data, not from a list in the binary and
    /// not from the model's imagination each time.
    pub known_predicates: Vec<String>,
    /// Who the utterance is attributed to.
    pub speaker: SpeakerAttribution,
    /// The logical session.
    pub session_id: SessionId,
    /// The turn.
    pub turn_id: TurnId,
    /// Evaluation time, for resolving relative dates.
    pub now: DateTime<Utc>,
}

impl ObservationExtractionContext {
    /// A context for a finalized user turn.
    pub fn user_turn(
        transcript: impl Into<String>,
        session_id: SessionId,
        turn_id: TurnId,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            transcript: transcript.into(),
            recent_user_turns: Vec::new(),
            recent_assistant_turn: None,
            known_predicates: Vec::new(),
            speaker: SpeakerAttribution::User,
            session_id,
            turn_id,
            now,
        }
    }

    /// Attribute the utterance to someone other than the enrolled user.
    pub fn attributed_to(mut self, speaker: SpeakerAttribution) -> Self {
        self.speaker = speaker;
        self
    }

    /// Offer the predicate names the corpus already uses.
    pub fn with_known_predicates(mut self, predicates: Vec<String>) -> Self {
        self.known_predicates = predicates;
        self
    }
}

/// Extracts candidate memories from an utterance.
#[async_trait]
pub trait MemoryObservationExtractor: Send + Sync {
    /// Extract observations. An empty result is normal and expected.
    async fn extract(
        &self,
        context: ObservationExtractionContext,
    ) -> Result<Vec<MemoryObservation>, MemoryError>;
}

/// The JSON Schema a structured-output extraction should be constrained to.
pub fn observation_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(MemoryObservation);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

/// Runs an extractor under a deadline.
///
/// Unlike retrieval planning there is no rule-based result worth substituting
/// for a failed model call, so a timeout yields nothing. Missing one turn's
/// evidence is recoverable — the durable transcript event allows a retry — and
/// far preferable to blocking the pipeline.
pub struct BoundedObservationExtractor {
    inner: Arc<dyn MemoryObservationExtractor>,
    timeout: Duration,
}

impl BoundedObservationExtractor {
    /// Bound `inner` to `timeout`.
    pub fn new(inner: Arc<dyn MemoryObservationExtractor>, timeout: Duration) -> Self {
        Self { inner, timeout }
    }
}

#[async_trait]
impl MemoryObservationExtractor for BoundedObservationExtractor {
    async fn extract(
        &self,
        context: ObservationExtractionContext,
    ) -> Result<Vec<MemoryObservation>, MemoryError> {
        match tokio::time::timeout(self.timeout, self.inner.extract(context)).await {
            Ok(result) => result,
            Err(_) => Err(MemoryError::DeadlineExceeded {
                operation: "observation extraction",
                budget_ms: self.timeout.as_millis() as u64,
            }),
        }
    }
}

/// A rule-based extractor covering explicit statements and memory commands.
///
/// This is a floor, not a ceiling: it exists so the engine is useful and
/// testable without a model in the loop, and so an unavailable extraction model
/// still captures the cases that matter most — the ones where the user said
/// "remember this" in so many words.
#[derive(Debug, Default)]
pub struct RuleBasedObservationExtractor;

impl RuleBasedObservationExtractor {
    /// A new extractor.
    pub fn new() -> Self {
        Self
    }
}

/// Command phrases and the intent they signal, longest first so
/// "don't forget" is not read as "forget".
const COMMANDS: &[(&str, MutationIntent)] = &[
    ("what do you remember", MutationIntent::List),
    ("what do you know about me", MutationIntent::List),
    ("please remember that", MutationIntent::Remember),
    ("please remember", MutationIntent::Remember),
    ("remember that", MutationIntent::Remember),
    ("do not forget that", MutationIntent::Remember),
    ("dont forget that", MutationIntent::Remember),
    ("do not forget", MutationIntent::Remember),
    ("dont forget", MutationIntent::Remember),
    ("from now on", MutationIntent::Remember),
    ("delete everything about", MutationIntent::Delete),
    ("delete what you know about", MutationIntent::Delete),
    ("forget that", MutationIntent::Forget),
    ("forget about", MutationIntent::Forget),
    ("forget", MutationIntent::Forget),
    ("i am no longer", MutationIntent::Correct),
    ("im no longer", MutationIntent::Correct),
    ("correct that", MutationIntent::Correct),
    ("actually i am", MutationIntent::Correct),
    ("actually im", MutationIntent::Correct),
];

/// Statement openers that introduce a first-person fact.
const SELF_STATEMENTS: &[(&str, MemoryKind)] = &[
    ("i am allergic to", MemoryKind::Identity),
    ("im allergic to", MemoryKind::Identity),
    ("i do not eat", MemoryKind::Preference),
    ("i dont eat", MemoryKind::Preference),
    ("i never eat", MemoryKind::Preference),
    ("i always", MemoryKind::Routine),
    ("i usually", MemoryKind::Routine),
    ("i have started", MemoryKind::Routine),
    ("ive started", MemoryKind::Routine),
    ("i prefer", MemoryKind::Preference),
    ("i love", MemoryKind::Preference),
    ("i like", MemoryKind::Preference),
    ("i hate", MemoryKind::Preference),
    ("i work at", MemoryKind::Identity),
    ("i live in", MemoryKind::Identity),
    ("i am", MemoryKind::Identity),
    ("im", MemoryKind::Identity),
];

/// Words suggesting a time-bounded rather than durable statement.
const EPISODIC_MARKERS: &[&str] = &[
    "tonight",
    "today",
    "tomorrow",
    "this morning",
    "this afternoon",
    "this evening",
    "last night",
    "yesterday",
    "this week",
    "right now",
    "at the moment",
];

/// Categories that must be stated, never inferred.
const SENSITIVE_MARKERS: &[&str] = &[
    "diagnosed",
    "medication",
    "therapy",
    "depressed",
    "anxiety",
    "pregnant",
    "church",
    "mosque",
    "temple",
    "synagogue",
    "voted",
    "political",
];

/// Normalize an utterance for phrase matching.
///
/// [`normalize_token`] turns every non-alphanumeric character into a separator.
/// That is right for identifiers and fingerprints and wrong for a spoken
/// sentence: it splits "I'm" into "i m" and "don't" into "don t", so the
/// contracted forms this module's tables are *written in* — `im`, `im allergic
/// to`, `dont forget` — could never match, and every one of them was dead.
///
/// It matters more here than it looks. This is a voice product; a transcript of
/// someone talking is mostly contractions, so the effect was that "I'm allergic
/// to sesame" produced no evidence at all while "I am allergic to sesame"
/// produced a durable fact. Eliding the apostrophe first is the smallest change
/// that makes the tables reachable, and it leaves `normalize_token` — which
/// fingerprints the existing corpus — untouched.
fn normalize_utterance(raw: &str) -> String {
    let elided: String = raw
        .chars()
        .filter(|c| !matches!(c, '\'' | '\u{2019}' | '\u{02BC}'))
        .collect();
    normalize_token(&elided)
}

#[async_trait]
impl MemoryObservationExtractor for RuleBasedObservationExtractor {
    async fn extract(
        &self,
        context: ObservationExtractionContext,
    ) -> Result<Vec<MemoryObservation>, MemoryError> {
        // Attribution is checked here as well as at admission: an extractor
        // should not be producing candidates it knows are inadmissible.
        if !context.speaker.may_be_stored() {
            return Ok(Vec::new());
        }

        let normalized = normalize_utterance(&context.transcript);
        let evidence = TranscriptEvidence::new(&context.transcript);
        let mut observations = Vec::new();

        if let Some((phrase, intent)) = COMMANDS
            .iter()
            .find(|(phrase, _)| normalized.contains(phrase))
        {
            let raw_remainder = normalized
                .split_once(phrase)
                .map(|(_, rest)| rest.trim().to_string())
                .unwrap_or_default();
            // "remember that I am pescatarian now" carries the fact "the user
            // is pescatarian" — stripping the self-reference is what lets it
            // fingerprint against the same fact stated plainly.
            let remainder = strip_self_reference(&raw_remainder);
            observations.push(build(
                &context,
                &evidence,
                command_kind(*intent),
                CanonicalPredicate::new(command_predicate(*intent, &remainder)),
                MemoryValue::Text(remainder.clone()),
                command_statement(*intent, &remainder),
                Explicitness::ExplicitCommand,
                1.0,
                ProposedPersistence::Durable,
                TemporalScope::Persistent,
                SensitivityClass::Normal,
                Some(*intent),
            ));
            return Ok(observations);
        }

        // Every matching clause, not just the first. "I'm pescatarian and I
        // prefer quiet places" is one utterance carrying two facts, and taking
        // only the leading clause silently drops the other.
        for (opener, kind) in SELF_STATEMENTS
            .iter()
            .filter(|(opener, _)| starts_clause(&normalized, opener))
        {
            let value = clause_after(&normalized, opener);
            if value.is_empty() {
                continue;
            }

            let episodic = EPISODIC_MARKERS.iter().any(|m| normalized.contains(m));
            let (kind, scope, persistence) = if episodic {
                (
                    MemoryKind::Episodic,
                    TemporalScope::Momentary,
                    ProposedPersistence::Episodic,
                )
            } else {
                (
                    *kind,
                    TemporalScope::Persistent,
                    ProposedPersistence::Durable,
                )
            };

            let sensitivity = if SENSITIVE_MARKERS.iter().any(|m| normalized.contains(m)) {
                SensitivityClass::Sensitive
            } else {
                SensitivityClass::Normal
            };

            // Openers overlap: "I am allergic to nuts" matches both
            // `i am allergic to` (value "nuts") and `i am` (value "allergic to
            // nuts"). They describe the same clause, so only the more specific
            // one is kept — otherwise a single fact is counted twice.
            if observations.iter().any(|o: &MemoryObservation| {
                let existing = o.value.display();
                existing.contains(&value) || value.contains(&existing)
            }) {
                continue;
            }
            let predicate = CanonicalPredicate::new(predicate_for(opener, &value));
            if observations
                .iter()
                .any(|o: &MemoryObservation| o.predicate == predicate)
            {
                continue;
            }
            observations.push(build(
                &context,
                &evidence,
                kind,
                predicate,
                MemoryValue::Text(value.clone()),
                statement_for(opener, &value),
                Explicitness::ExplicitStatement,
                0.9,
                persistence,
                scope,
                sensitivity,
                None,
            ));
        }

        Ok(observations)
    }
}

/// The clause following `opener`, stopping at the next clause boundary.
///
/// Without the stop, "I am pescatarian and I prefer quiet places" would store
/// the dietary fact with a value of "pescatarian and i prefer quiet places".
fn clause_after(text: &str, opener: &str) -> String {
    const SEPARATORS: [&str; 4] = [" and ", " but ", " so ", " because "];
    let Some((_, rest)) = text.split_once(opener) else {
        return String::new();
    };
    let mut clause = rest.trim();
    for separator in SEPARATORS {
        if let Some((head, _)) = clause.split_once(separator) {
            clause = head.trim();
        }
    }
    clause.to_string()
}

/// Whether `text` begins with `opener` on a clause boundary.
///
/// Guards against "i am" matching inside "what i am asking is" — which reads as
/// a statement of identity only if you ignore the sentence around it.
fn starts_clause(text: &str, opener: &str) -> bool {
    if text.starts_with(opener) {
        return true;
    }
    for separator in [" and ", " but ", " so ", " because "] {
        if text.contains(&format!("{separator}{opener} ")) {
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_arguments, reason = "an internal record constructor")]
fn build(
    context: &ObservationExtractionContext,
    evidence: &TranscriptEvidence,
    kind: MemoryKind,
    predicate: CanonicalPredicate,
    value: MemoryValue,
    statement: String,
    explicitness: Explicitness,
    confidence: f32,
    persistence: ProposedPersistence,
    temporal_scope: TemporalScope,
    sensitivity: SensitivityClass,
    mutation_intent: Option<MutationIntent>,
) -> MemoryObservation {
    let expected_expiry =
        crate::core::default_episodic_ttl(kind, temporal_scope).map(|ttl| context.now + ttl);
    MemoryObservation {
        observation_id: ObservationId::generate(),
        session_id: context.session_id.clone(),
        turn_id: context.turn_id,
        subject: EntityRef::user(),
        predicate,
        value,
        canonical_statement: statement,
        kind,
        explicitness,
        confidence,
        persistence,
        temporal_scope,
        valid_from: Some(context.now),
        expected_expiry,
        transcript_evidence: evidence.clone(),
        speaker_attribution: context.speaker,
        sensitivity,
        mutation_intent,
        search_terms: Vec::new(),
    }
}

fn command_kind(intent: MutationIntent) -> MemoryKind {
    match intent {
        MutationIntent::Forget | MutationIntent::Delete | MutationIntent::List => {
            MemoryKind::Identity
        }
        _ => MemoryKind::Preference,
    }
}

fn command_predicate(intent: MutationIntent, remainder: &str) -> String {
    match intent {
        MutationIntent::Forget | MutationIntent::Delete => "memory_removal".to_string(),
        MutationIntent::List => "memory_listing".to_string(),
        _ => predicate_for("", remainder),
    }
}

fn command_statement(intent: MutationIntent, remainder: &str) -> String {
    match intent {
        MutationIntent::Forget => format!("The user asked to forget: {remainder}."),
        MutationIntent::Delete => format!("The user asked to delete everything about {remainder}."),
        MutationIntent::List => "The user asked what is remembered about them.".to_string(),
        // A correction or an instruction to remember carries a fact. Rendering
        // it as "the user corrected: pescatarian" would store the act rather
        // than the content, and the model would recall the act.
        MutationIntent::Correct | MutationIntent::Remember => statement_for("", remainder),
    }
}

/// Strip the first-person framing from a command's payload.
///
/// "remember that I am pescatarian now" and "I am pescatarian" describe the
/// same fact; without this they fingerprint differently and reconcile as a
/// contradiction rather than as reinforcement.
fn strip_self_reference(remainder: &str) -> String {
    let mut value = remainder.trim();
    for prefix in ["that i am ", "that im ", "that i ", "i am ", "im ", "that "] {
        if let Some(stripped) = value.strip_prefix(prefix) {
            value = stripped.trim();
            break;
        }
    }
    for suffix in [" from now on", " anymore", " any more", " now"] {
        if let Some(stripped) = value.strip_suffix(suffix) {
            value = stripped.trim();
            break;
        }
    }
    value.to_string()
}

/// Derive a canonical predicate from the topic of a statement.
fn predicate_for(opener: &str, value: &str) -> String {
    const TOPICS: &[(&str, &str)] = &[
        ("vegetarian", "dietary_identity"),
        ("vegan", "dietary_identity"),
        ("pescatarian", "dietary_identity"),
        ("meat", "dietary_identity"),
        ("fish", "dietary_identity"),
        ("allergic", "allergy"),
        ("coffee", "beverage_preference"),
        ("tea", "beverage_preference"),
        ("gym", "exercise_routine"),
        ("run", "exercise_routine"),
        ("workout", "exercise_routine"),
        ("restaurant", "venue_preference"),
        ("music", "music_preference"),
    ];
    if let Some((_, predicate)) = TOPICS.iter().find(|(topic, _)| value.contains(topic)) {
        return (*predicate).to_string();
    }
    match opener {
        "i live in" => "residence".to_string(),
        "i work at" => "employer".to_string(),
        "i always" | "i usually" | "i have started" | "ive started" => "routine".to_string(),
        _ => "preference".to_string(),
    }
}

fn statement_for(opener: &str, value: &str) -> String {
    let subject = match opener {
        "i do not eat" | "i dont eat" | "i never eat" => "The user does not eat",
        "i prefer" => "The user prefers",
        "i love" => "The user loves",
        "i like" => "The user likes",
        "i hate" => "The user dislikes",
        "i work at" => "The user works at",
        "i live in" => "The user lives in",
        "i always" => "The user always",
        "i usually" => "The user usually",
        "i have started" | "ive started" => "The user has started",
        "i am allergic to" | "im allergic to" => "The user is allergic to",
        _ => "The user is",
    };
    format!("{subject} {value}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn extract(text: &str) -> Vec<MemoryObservation> {
        RuleBasedObservationExtractor::new()
            .extract(ObservationExtractionContext::user_turn(
                text,
                SessionId::new("ses_1"),
                TurnId(1),
                Utc::now(),
            ))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_explicit_preference_becomes_a_durable_candidate() {
        let observations = extract("I am pescatarian").await;
        assert_eq!(observations.len(), 1);
        let obs = &observations[0];
        assert_eq!(obs.predicate.as_str(), "dietary_identity");
        assert_eq!(obs.explicitness, Explicitness::ExplicitStatement);
        assert_eq!(obs.persistence, ProposedPersistence::Durable);
        assert_eq!(obs.canonical_statement, "The user is pescatarian.");
    }

    #[tokio::test]
    async fn a_memory_command_carries_its_intent() {
        let observations = extract("Please remember that I am pescatarian now").await;
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].mutation_intent,
            Some(MutationIntent::Remember)
        );
        assert_eq!(observations[0].explicitness, Explicitness::ExplicitCommand);
        assert_eq!(observations[0].confidence, 1.0);
    }

    #[tokio::test]
    async fn a_command_stores_the_fact_rather_than_the_act_of_commanding() {
        let commanded = extract("please remember that I am pescatarian now").await;
        assert_eq!(commanded[0].canonical_statement, "The user is pescatarian.");
        assert_eq!(commanded[0].predicate.as_str(), "dietary_identity");

        // And it fingerprints identically to the same fact stated plainly, so
        // the two reinforce instead of contradicting.
        let stated = extract("I am pescatarian").await;
        assert_eq!(commanded[0].fingerprint(), stated[0].fingerprint());
    }

    #[tokio::test]
    async fn a_correction_reads_as_the_corrected_fact() {
        let observations = extract("actually I am pescatarian").await;
        assert_eq!(
            observations[0].mutation_intent,
            Some(MutationIntent::Correct)
        );
        assert_eq!(
            observations[0].canonical_statement,
            "The user is pescatarian."
        );
    }

    #[tokio::test]
    async fn forget_and_delete_are_distinguished_from_remember() {
        assert_eq!(
            extract("forget that I like sushi").await[0].mutation_intent,
            Some(MutationIntent::Forget)
        );
        assert_eq!(
            extract("delete everything about my old job").await[0].mutation_intent,
            Some(MutationIntent::Delete)
        );
        // "don't forget" is an instruction to remember, not to forget.
        assert_eq!(
            extract("dont forget that I am allergic to nuts").await[0].mutation_intent,
            Some(MutationIntent::Remember)
        );
    }

    #[tokio::test]
    async fn a_time_bounded_statement_becomes_episodic_with_an_expiry() {
        let observations = extract("I am meeting Kushal for dinner tonight").await;
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].kind, MemoryKind::Episodic);
        assert_eq!(observations[0].persistence, ProposedPersistence::Episodic);
        assert!(observations[0].expected_expiry.is_some());
    }

    #[tokio::test]
    async fn bystander_and_assistant_speech_yields_nothing() {
        for speaker in [
            SpeakerAttribution::Bystander,
            SpeakerAttribution::Assistant,
            SpeakerAttribution::Unknown,
        ] {
            let observations = RuleBasedObservationExtractor::new()
                .extract(
                    ObservationExtractionContext::user_turn(
                        "I am pescatarian",
                        SessionId::new("ses_1"),
                        TurnId(1),
                        Utc::now(),
                    )
                    .attributed_to(speaker),
                )
                .await
                .unwrap();
            assert!(observations.is_empty(), "{speaker:?} produced candidates");
        }
    }

    #[tokio::test]
    async fn an_ordinary_utterance_reveals_nothing_worth_keeping() {
        assert!(extract("what is the weather like").await.is_empty());
        assert!(extract("okay thanks").await.is_empty());
    }

    #[tokio::test]
    async fn a_first_person_phrase_inside_a_question_is_not_a_statement() {
        // "what i am asking is whether…" contains "i am" but asserts nothing.
        assert!(
            extract("what i am asking is whether it is open")
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_contracted_utterance_carries_the_same_fact_as_its_expanded_form() {
        // This is a voice product: a transcript of someone speaking is mostly
        // contractions. `normalize_token` turns an apostrophe into a separator,
        // so "I'm" arrived as "i m" and every contracted opener in the tables
        // above — `im`, `im allergic to`, `dont forget` — was unreachable. The
        // effect was silent and total: "I am allergic to sesame" became a
        // durable fact and "I'm allergic to sesame" became nothing at all.
        for (contracted, expanded) in [
            ("I'm allergic to sesame", "I am allergic to sesame"),
            (
                "I've started swimming on Tuesdays",
                "I have started swimming on Tuesdays",
            ),
        ] {
            let (short, long) = (extract(contracted).await, extract(expanded).await);
            assert!(
                !short.is_empty(),
                "`{contracted}` produced no observation while `{expanded}` did"
            );
            assert_eq!(
                short[0].predicate, long[0].predicate,
                "`{contracted}` and `{expanded}` are the same fact and must land on \
                 the same predicate, or a correction will not supersede what it corrects"
            );
            assert_eq!(
                short[0].value, long[0].value,
                "`{contracted}` and `{expanded}` must carry the same value, or they \
                 fingerprint differently and reinforce nothing"
            );
        }
    }

    #[tokio::test]
    async fn statements_after_a_conjunction_are_still_recognised() {
        let observations = extract("we went out and i do not eat meat").await;
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].predicate.as_str(), "dietary_identity");
    }

    #[tokio::test]
    async fn one_utterance_carrying_two_facts_yields_two_observations() {
        let observations = extract("I am pescatarian and I prefer quiet places").await;
        let predicates: Vec<&str> = observations.iter().map(|o| o.predicate.as_str()).collect();
        assert!(
            predicates.contains(&"dietary_identity"),
            "the dietary fact was dropped: {predicates:?}"
        );
        assert!(
            predicates.contains(&"preference") || predicates.contains(&"venue_preference"),
            "the venue preference was dropped: {predicates:?}"
        );
    }

    #[tokio::test]
    async fn overlapping_openers_yield_one_fact_not_two() {
        let observations = extract("I am allergic to nuts").await;
        assert_eq!(
            observations.len(),
            1,
            "the same clause was captured twice: {:?}",
            observations
                .iter()
                .map(|o| (o.predicate.to_string(), o.value.display()))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_clause_value_stops_at_the_conjunction() {
        let observations = extract("I am pescatarian and I prefer quiet places").await;
        let dietary = observations
            .iter()
            .find(|o| o.predicate.as_str() == "dietary_identity")
            .expect("dietary fact");
        assert_eq!(
            dietary.canonical_statement, "The user is pescatarian.",
            "the value swallowed the following clause"
        );
    }

    struct Hangs;

    #[async_trait]
    impl MemoryObservationExtractor for Hangs {
        async fn extract(
            &self,
            _context: ObservationExtractionContext,
        ) -> Result<Vec<MemoryObservation>, MemoryError> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            unreachable!("the bound should fire first")
        }
    }

    #[tokio::test]
    async fn a_hanging_extractor_reports_a_retryable_deadline_rather_than_blocking() {
        let bounded = BoundedObservationExtractor::new(Arc::new(Hangs), Duration::from_millis(20));
        let err = bounded
            .extract(ObservationExtractionContext::user_turn(
                "I am pescatarian",
                SessionId::new("ses_1"),
                TurnId(1),
                Utc::now(),
            ))
            .await
            .unwrap_err();
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn a_health_statement_is_classified_sensitive() {
        let observations = extract("I am diagnosed with a heart condition").await;
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].sensitivity, SensitivityClass::Sensitive);
        // Stated outright, so it is still admissible — it is inference that
        // policy refuses, not the subject matter.
        assert!(observations[0].explicitness.is_explicit());
    }

    #[test]
    fn the_instruction_states_the_attribution_and_sensitivity_rules() {
        assert!(OBSERVATION_EXTRACTION_INSTRUCTION.contains("Only extract things the USER said"));
        assert!(OBSERVATION_EXTRACTION_INSTRUCTION.contains("Never infer sensitive attributes"));
    }

    #[test]
    fn the_observation_schema_is_a_json_object_schema() {
        let schema = observation_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["explicitness"].is_object());
    }
}
