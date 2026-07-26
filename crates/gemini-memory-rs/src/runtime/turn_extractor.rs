//! Memory ingestion as a first-class [`TurnExtractor`].
//!
//! The runtime already has an out-of-band extraction pipeline: it accumulates
//! transcripts, segments them by turn boundary, fires extractors after each
//! turn under a trigger policy, and promotes their results into `State`. Memory
//! ingestion is exactly that shape, so it plugs into that pipeline rather than
//! running a second one beside it.
//!
//! What this does *not* cover is speculative retrieval on partial transcripts —
//! a turn extractor by construction only sees finalized turns. That path keeps
//! its own fast-lane bridge in [`super::events`], which is the honest boundary
//! between the two mechanisms.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

use gemini_adk_rs::live::extractor::{ExtractionTrigger, TurnExtractor};
use gemini_adk_rs::live::transcript::TranscriptTurn;
use gemini_adk_rs::llm::LlmError;

use crate::core::TurnId;
use crate::engine::MemorySession;
use crate::ingestion::LedgerOutcome;

/// The `State` key the pipeline stores this extractor's summary under.
pub const MEMORY_EXTRACTOR_NAME: &str = "memory";

/// Drives memory ingestion from the runtime's turn-boundary extraction pipeline.
pub struct MemoryTurnExtractor {
    session: Arc<MemorySession>,
    min_words: usize,
    window: usize,
}

impl MemoryTurnExtractor {
    /// Ingest from `session`, skipping turns shorter than three words.
    ///
    /// The floor exists because "ok", "yeah" and "mm hmm" are most of a voice
    /// conversation and none of them are evidence; spending an extraction on
    /// them is pure cost.
    pub fn new(session: Arc<MemorySession>) -> Self {
        Self {
            session,
            min_words: 3,
            window: 3,
        }
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

        // Turn completion also drives the reconciliation cadence, so the
        // pipeline firing this extractor is enough to keep the session ticking.
        let scheduled = self
            .session
            .on_turn_complete(turn_id)
            .await
            .map_err(|e| LlmError::Other(e.to_string()))?;

        let created = outcomes
            .iter()
            .filter(|o| matches!(o, LedgerOutcome::Created(_)))
            .count();
        let reinforced = outcomes
            .iter()
            .filter(|o| matches!(o, LedgerOutcome::Reinforced { .. }))
            .count();
        let rejected = outcomes
            .iter()
            .filter(|o| matches!(o, LedgerOutcome::Rejected(_)))
            .count();

        // Returned for observability, not for promotion into state: memory's
        // authoritative store is the ledger, not a `State` key.
        Ok(json!({
            "turn": turn.turn_number,
            "created": created,
            "reinforced": reinforced,
            "rejected": rejected,
            "session_facts": self.session.ledger().usable_candidates().len(),
            "scheduled": scheduled.iter().map(|w| format!("{w:?}")).collect::<Vec<_>>(),
        }))
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
        assert_eq!(summary["session_facts"], 1);
        assert_eq!(session.ledger().usable_candidates().len(), 1);
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
        assert_eq!(extractor.window_size(), 3);
    }
}
