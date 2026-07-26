//! Memory as a first-class [`TurnExtractor`].
//!
//! This is the whole runtime integration. The Live runtime already has an
//! out-of-band extraction pipeline that accumulates transcripts, segments them
//! by turn boundary, fires extractors under a trigger policy, and **promotes
//! their fields into governed `State`**. Memory is exactly that shape, so it
//! rides that pipeline instead of running a second one beside it.
//!
//! The promotion step is what makes memory useful to an application rather than
//! merely present. A remembered fact projected into a `State` slot is read by
//! everything the platform already has:
//!
//! - `phase.needs(&["user:diet"])` — satisfied from memory, so a returning user
//!   is not asked again for something they already said last week;
//! - `phase.requires(&["user:diet"])` — a hard gate a memory can open;
//! - `Flow` guards, `done(captured(["user:diet"]))`;
//! - `P::with_state(&["user:diet"])` — the value in the phase instruction;
//! - watchers and repair, which read the same keys.
//!
//! Each turn the extractor does three things: ingest the finalized utterance,
//! prepare the next turn's retrieval snapshot, and project what memory knows
//! into slots.

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

use gemini_adk_rs::live::extractor::{ExtractionTrigger, FieldPromotion, TurnExtractor};
use gemini_adk_rs::live::transcript::TranscriptTurn;
use gemini_adk_rs::llm::LlmError;
use gemini_adk_rs::state::State;

use crate::core::{CanonicalPredicate, TurnId};
use crate::engine::MemorySession;
use crate::ingestion::LedgerOutcome;

/// The `State` key the pipeline stores this extractor's raw summary under.
pub const MEMORY_EXTRACTOR_NAME: &str = "memory";

/// A mapping from a memory predicate to the governed `State` slot it fills.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySlot {
    /// The canonical predicate to look for, e.g. `dietary_identity`.
    pub predicate: CanonicalPredicate,
    /// The `State` key to fill, e.g. `user:diet`.
    pub state_key: String,
}

impl MemorySlot {
    /// Map `predicate` onto `state_key`.
    ///
    /// Use the platform's `scope:key` convention — `user:diet`, not `user.diet`.
    /// The gates themselves do not care: `needs`, `requires` and `Guard::is_set`
    /// route through `State::contains`, which treats the key as an opaque
    /// string. What the colon buys is composition with the prefix scopes, so
    /// `state.user().get::<String>("diet")` finds the slot. A dotted key reads
    /// back `None` there, silently, for a developer doing exactly what the
    /// platform documentation says.
    ///
    /// `derived:` is the wrong home despite fitting semantically: its fallback
    /// lives only in `get`/`with`, and `contains` has none — so a `derived:`
    /// slot would be invisible to precisely the gates memory exists to satisfy.
    pub fn new(predicate: impl AsRef<str>, state_key: impl Into<String>) -> Self {
        Self {
            predicate: CanonicalPredicate::new(predicate),
            state_key: state_key.into(),
        }
    }
}

/// Drives memory from the runtime's turn-boundary extraction pipeline.
pub struct MemoryTurnExtractor {
    session: Arc<MemorySession>,
    slots: Vec<MemorySlot>,
    promotions: Vec<FieldPromotion>,
    min_words: usize,
    window: usize,
}

impl MemoryTurnExtractor {
    /// Ingest from `session`, skipping turns shorter than three words.
    ///
    /// The floor exists because "ok", "yeah" and "mm hmm" are most of a voice
    /// conversation and none of them are evidence.
    pub fn new(session: Arc<MemorySession>) -> Self {
        Self {
            session,
            slots: Vec::new(),
            promotions: Vec::new(),
            min_words: 3,
            window: 3,
        }
    }

    /// Project memory facts into governed `State` slots.
    ///
    /// Each entry maps a canonical predicate to the state key a phase or flow
    /// reads. A slot filled from memory satisfies `needs`/`requires` exactly as
    /// one filled by the user would, which is the point: the application does
    /// not have to know whether it learned something now or last month.
    ///
    /// Slots are promoted with `KeepKnown`, so anything the current
    /// conversation established wins over what memory recalls.
    pub fn slots(mut self, slots: impl IntoIterator<Item = MemorySlot>) -> Self {
        self.slots = slots.into_iter().collect();
        self.promotions = self
            .slots
            .iter()
            .map(|slot| FieldPromotion {
                field: slot.state_key.clone(),
                state_key: slot.state_key.clone(),
                merge: gemini_adk_rs::live::extractor::MergePolicy::KeepKnown,
                accept: None,
            })
            .collect();
        self
    }

    /// Require at least `words` in the user's utterance before extracting.
    pub fn min_words(mut self, words: usize) -> Self {
        self.min_words = words;
        self
    }

    /// How many recent turns the extractor asks the pipeline for.
    pub fn window(mut self, turns: usize) -> Self {
        self.window = turns;
        self
    }

    /// The slot values memory can currently fill.
    fn slot_values(&self) -> Map<String, Value> {
        let mut out = Map::new();
        if self.slots.is_empty() {
            return out;
        }
        for (predicate, value) in self.session.known_values() {
            if let Some(slot) = self.slots.iter().find(|s| s.predicate == predicate) {
                out.entry(slot.state_key.clone()).or_insert(value);
            }
        }
        out
    }
}

#[async_trait]
impl TurnExtractor for MemoryTurnExtractor {
    fn name(&self) -> &str {
        MEMORY_EXTRACTOR_NAME
    }

    fn window_size(&self) -> usize {
        self.window
    }

    fn trigger(&self) -> ExtractionTrigger {
        ExtractionTrigger::EveryTurn
    }

    fn promotion_rules(&self) -> &[FieldPromotion] {
        &self.promotions
    }

    fn should_extract(&self, window: &[TranscriptTurn]) -> bool {
        window
            .last()
            .is_some_and(|turn| turn.user.split_whitespace().count() >= self.min_words)
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        let Some(turn) = window.last() else {
            return Ok(json!({}));
        };
        let turn_id = TurnId(u64::from(turn.turn_number));

        let outcomes = self
            .session
            .observe_final_transcript(turn_id, &turn.user)
            .await
            .map_err(|e| LlmError::Other(e.to_string()))?;

        // Turn completion drives the reconciliation cadence, so the pipeline
        // firing this extractor is enough to keep the session ticking.
        let scheduled = self
            .session
            .on_turn_complete(turn_id)
            .await
            .map_err(|e| LlmError::Other(e.to_string()))?;

        // Prepare the *next* turn's context now, while the model is speaking.
        // This is the "prepare asynchronously, consume synchronously" rule: by
        // the time a `recall_context` call arrives, the answer is already sat
        // in the session.
        //
        // Prepare *then* begin, in that order. `begin_turn` promotes whatever
        // `prepare` wrote last, so beginning first published the speculation
        // from the previous round and left this one sitting unread in
        // `prepared` for a whole turn: the snapshot serving turn N was built
        // from the transcript of turn N−2. That is the difference between
        // someone saying "I'm meeting Rhea for dinner" and being understood on
        // the next sentence, or on the one after that.
        let next = TurnId(turn_id.0 + 1);
        let _ = self.session.prepare(next, &turn.user).await;
        self.session.begin_turn(next);

        let mut payload = self.slot_values();
        payload.insert("turn".into(), json!(turn.turn_number));
        payload.insert(
            "created".into(),
            json!(outcomes
                .iter()
                .filter(|o| matches!(o, LedgerOutcome::Created(_)))
                .count()),
        );
        payload.insert(
            "reinforced".into(),
            json!(outcomes
                .iter()
                .filter(|o| matches!(o, LedgerOutcome::Reinforced { .. }))
                .count()),
        );
        payload.insert(
            "rejected".into(),
            json!(outcomes
                .iter()
                .filter(|o| matches!(o, LedgerOutcome::Rejected(_)))
                .count()),
        );
        payload.insert(
            "session_facts".into(),
            json!(self.session.ledger().usable_candidates().len()),
        );
        payload.insert(
            "scheduled".into(),
            json!(scheduled
                .iter()
                .map(|w| format!("{w:?}"))
                .collect::<Vec<_>>()),
        );
        Ok(Value::Object(payload))
    }

    async fn extract_with_state(
        &self,
        window: &[TranscriptTurn],
        state: &State,
    ) -> Result<Value, LlmError> {
        let value = self.extract(window).await?;

        // Slots are also written directly, not only returned for promotion:
        // an application that registered no promotion rules still gets its
        // slots, and a phase evaluated in the same turn sees them.
        for slot in &self.slots {
            if state.contains(&slot.state_key) {
                continue;
            }
            if let Some(filled) = value.get(&slot.state_key) {
                let _ = state.set(slot.state_key.clone(), filled.clone());
            }
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SessionId, UserId};
    use crate::engine::MemoryEngine;
    use std::time::Instant;

    fn turn(number: u32, user: &str) -> TranscriptTurn {
        TranscriptTurn {
            turn_number: number,
            user: user.to_string(),
            model: String::new(),
            tool_calls: Vec::new(),
            timestamp: Instant::now(),
        }
    }

    fn session() -> Arc<MemorySession> {
        let engine = MemoryEngine::in_memory(UserId::new("usr_1"));
        Arc::new(engine.begin_session(SessionId::new("ses_1")))
    }

    #[tokio::test]
    async fn a_finalized_turn_becomes_a_session_candidate() {
        let session = session();
        let extractor = MemoryTurnExtractor::new(session.clone());
        let window = [turn(1, "I am pescatarian")];

        assert!(extractor.should_extract(&window));
        let summary = extractor.extract(&window).await.unwrap();

        assert_eq!(summary["created"], 1);
        assert_eq!(session.ledger().usable_candidates().len(), 1);
    }

    #[tokio::test]
    async fn a_remembered_fact_fills_the_slot_a_phase_gates_on() {
        let session = session();
        let extractor = MemoryTurnExtractor::new(session.clone())
            .slots([MemorySlot::new("dietary_identity", "user:diet")]);
        let state = State::new();

        extractor
            .extract_with_state(&[turn(1, "I am pescatarian")], &state)
            .await
            .unwrap();

        // This is the key `phase.needs(..)` and a `Flow` guard read; that they
        // really do read it is asserted against a driven `PhaseMachine` and
        // `FlowMonitor` in `tests/governed_integration.rs`.
        assert_eq!(
            state.get::<String>("user:diet").as_deref(),
            Some("pescatarian"),
            "memory did not fill the slot the application gates on"
        );
    }

    #[tokio::test]
    async fn what_the_conversation_established_wins_over_what_memory_recalls() {
        let session = session();
        let extractor = MemoryTurnExtractor::new(session.clone())
            .slots([MemorySlot::new("dietary_identity", "user:diet")]);
        let state = State::new();
        state.set("user:diet", "vegan").unwrap();

        extractor
            .extract_with_state(&[turn(1, "I am pescatarian")], &state)
            .await
            .unwrap();

        assert_eq!(
            state.get::<String>("user:diet").as_deref(),
            Some("vegan"),
            "memory overwrote a slot the live conversation had already set"
        );
    }

    #[tokio::test]
    async fn promotion_rules_are_declared_for_every_slot() {
        let extractor = MemoryTurnExtractor::new(session()).slots([
            MemorySlot::new("dietary_identity", "user:diet"),
            MemorySlot::new("venue_preference", "user:venue"),
        ]);
        let keys: Vec<&str> = extractor
            .promotion_rules()
            .iter()
            .map(|r| r.state_key.as_str())
            .collect();
        assert_eq!(keys, vec!["user:diet", "user:venue"]);
    }

    #[tokio::test]
    async fn a_session_with_no_slots_configured_promotes_nothing() {
        let extractor = MemoryTurnExtractor::new(session());
        assert!(extractor.promotion_rules().is_empty());
        let state = State::new();
        extractor
            .extract_with_state(&[turn(1, "I am pescatarian")], &state)
            .await
            .unwrap();
        assert!(state.keys().iter().all(|k| !k.starts_with("user.")));
    }

    #[tokio::test]
    async fn backchannel_turns_never_reach_an_extraction() {
        let extractor = MemoryTurnExtractor::new(session());
        for filler in ["ok", "mm hmm", "yeah"] {
            assert!(
                !extractor.should_extract(&[turn(1, filler)]),
                "`{filler}` should not be worth extracting"
            );
        }
        assert!(extractor.should_extract(&[turn(1, "I am pescatarian now")]));
    }

    #[tokio::test]
    async fn the_next_turns_context_is_prepared_before_it_is_asked_for() {
        let session = session();
        let extractor = MemoryTurnExtractor::new(session.clone());

        extractor
            .extract(&[turn(1, "I am pescatarian")])
            .await
            .unwrap();
        // A turn that asks something: this is where preparation pays.
        extractor
            .extract(&[turn(2, "what do you remember about my dietary preferences")])
            .await
            .unwrap();

        // The `recall_context` handler reads this; it must already be filled
        // by the time the model asks.
        assert!(
            !session.prepared_snapshot().is_empty(),
            "the next turn's context was not prepared during this turn"
        );
    }

    #[tokio::test]
    async fn a_turn_that_only_states_something_prepares_that_something() {
        // Preparation runs on every turn, because a local BM25 pass costs tens
        // of microseconds and guessing which utterances "deserve" one costs
        // recall. What a self-statement retrieves is its own fact — which is
        // what the model should have in hand on the turn after it was told.
        let session = session();
        MemoryTurnExtractor::new(session.clone())
            .extract(&[turn(1, "I am pescatarian")])
            .await
            .unwrap();
        let prepared = session.prepared_snapshot();
        assert!(prepared
            .facts
            .iter()
            .any(|f| f.statement.to_lowercase().contains("pescatarian")));
    }

    #[tokio::test]
    async fn a_turn_with_no_content_words_prepares_nothing() {
        // The one skip the planner can make without understanding language:
        // there is nothing to search *with*.
        let session = session();
        MemoryTurnExtractor::new(session.clone())
            .extract(&[turn(1, "what do you think")])
            .await
            .unwrap();
        assert!(session.prepared_snapshot().is_empty());
    }

    #[tokio::test]
    async fn restating_a_fact_reinforces_rather_than_duplicating() {
        let session = session();
        let extractor = MemoryTurnExtractor::new(session.clone());

        extractor
            .extract(&[turn(1, "I am pescatarian")])
            .await
            .unwrap();
        let second = extractor
            .extract(&[turn(4, "I am pescatarian")])
            .await
            .unwrap();

        assert_eq!(second["reinforced"], 1);
        assert_eq!(session.ledger().usable_candidates().len(), 1);
    }

    #[tokio::test]
    async fn an_empty_window_is_a_no_op() {
        let extractor = MemoryTurnExtractor::new(session());
        assert_eq!(extractor.extract(&[]).await.unwrap(), json!({}));
        assert!(!extractor.should_extract(&[]));
    }

    #[test]
    fn it_registers_under_a_stable_name_and_fires_every_turn() {
        let extractor = MemoryTurnExtractor::new(session());
        assert_eq!(extractor.name(), MEMORY_EXTRACTOR_NAME);
        assert_eq!(extractor.trigger(), ExtractionTrigger::EveryTurn);
    }
}
