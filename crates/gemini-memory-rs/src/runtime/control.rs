//! The memory control loop.
//!
//! One task, owning all the work the fast lane refuses to do. It drains the
//! transcript channel, speculates on partials, extracts evidence from finals,
//! and keeps the shared [`State`] in step so a tool handler can answer from a
//! state read.

use std::sync::Arc;
use std::time::Instant;

use gemini_adk_rs::state::State;
use tokio::sync::mpsc;

use super::events::MemoryRuntimeEvent;
use super::keys;
use crate::core::TurnId;
use crate::engine::MemorySession;
use crate::retrieval::PreparedMemorySnapshot;
use crate::transcript::{SpeculationDecision, SpeculationGate, TranscriptAccumulator};

/// Run the control loop until the channel closes or the session ends.
///
/// Returns when the conversation is over, having reconciled if it was able to.
pub async fn run_memory_control_loop(
    mut events: mpsc::Receiver<MemoryRuntimeEvent>,
    session: Arc<MemorySession>,
    state: State,
) {
    let mut accumulator = TranscriptAccumulator::new(TurnId::ZERO);
    let mut gate = SpeculationGate::new(&session.config().transcript);

    while let Some(event) = events.recv().await {
        match event {
            MemoryRuntimeEvent::UserActivityStarted { turn_id } => {
                begin_turn(&session, &state, turn_id);
                accumulator.begin_turn(turn_id);
                gate.reset();
            }

            MemoryRuntimeEvent::PartialTranscript { turn_id, text } => {
                if turn_id != accumulator.turn_id() {
                    accumulator.begin_turn(turn_id);
                    gate.reset();
                }
                let hypothesis = accumulator.push_partial(&text);
                let _ = state.set_key(&keys::TRANSCRIPT_PARTIAL, hypothesis.clone());

                // Speculation is best-effort: a dropped or gated revision costs
                // nothing, because the final transcript will speculate anyway.
                if gate.consider(&hypothesis, false, Instant::now()) == SpeculationDecision::Fire {
                    let _ = session.prepare(turn_id, &hypothesis.stable_prefix).await;
                    publish_prepared(&session, &state);
                }
            }

            MemoryRuntimeEvent::FinalTranscript { turn_id, text } => {
                if turn_id != accumulator.turn_id() {
                    accumulator.begin_turn(turn_id);
                }
                accumulator.finalize(&text);
                let _ = state.set_key(&keys::TRANSCRIPT_FINAL, text.to_string());

                // Evidence first, then speculation. If the process dies between
                // the two, the turn is recoverable from the event log.
                let _ = session.observe_final_transcript(turn_id, &text).await;
                publish_overlay(&session, &state);

                let _ = session.prepare(turn_id, &text).await;
                publish_prepared(&session, &state);
            }

            MemoryRuntimeEvent::UserActivityEnded { .. } => {}

            MemoryRuntimeEvent::RecallRequested { query, turn_id } => {
                // Serving happens in the tool handler; this event exists so a
                // caller that routes tool calls through the loop can pre-warm.
                let _ = session.recall(&query, turn_id).await;
            }

            MemoryRuntimeEvent::TurnCompleted { turn_id } => {
                let _ = session.on_turn_complete(turn_id).await;
                publish_overlay(&session, &state);
                publish_prepared(&session, &state);
            }

            MemoryRuntimeEvent::IdleTick => {
                if session.is_idle() {
                    let _ = session.finish().await;
                    publish_overlay(&session, &state);
                    return;
                }
            }

            MemoryRuntimeEvent::SessionEnded => {
                let _ = session.finish().await;
                publish_overlay(&session, &state);
                return;
            }
        }
    }
}

/// Freeze the prepared snapshot as the answer source for a starting turn.
fn begin_turn(session: &MemorySession, state: &State, turn_id: TurnId) {
    let generation = session.begin_turn(turn_id);
    let _ = state.set_key(&keys::CURRENT_TURN, turn_id);
    let _ = state.set_key(&keys::MEMORY_GENERATION, generation);
    let _ = state.set_key(&keys::ACTIVE_TURN_MEMORY, session.active_snapshot());
}

fn publish_prepared(session: &MemorySession, state: &State) {
    let _ = state.set_key(&keys::PREPARED_MEMORY, session.prepared_snapshot());
}

fn publish_overlay(session: &MemorySession, state: &State) {
    let _ = state.set_key(
        &keys::OVERLAY_SIZE,
        session.ledger().usable_candidates().len(),
    );
    let _ = state.set_key(&keys::OVERLAY_REVISION, session.ledger().revision());
    let _ = state.set_key(
        &keys::PENDING_EXPLICIT_MUTATION,
        !session.pending_explicit_commands().is_empty(),
    );
}

/// Read the snapshot the in-flight turn should be answered from.
///
/// Prefers the frozen active snapshot and falls back to the latest prepared
/// one, so a tool call arriving before any turn began still finds context.
pub fn snapshot_for_turn(state: &State) -> PreparedMemorySnapshot {
    state
        .get_key(&keys::ACTIVE_TURN_MEMORY)
        .or_else(|| state.get_key(&keys::PREPARED_MEMORY))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SessionId, UserId};
    use crate::engine::MemoryEngine;
    use crate::runtime::events::channel;

    fn setup() -> (Arc<MemoryEngine>, Arc<MemorySession>, State) {
        let engine = Arc::new(MemoryEngine::in_memory(UserId::new("usr_1")));
        let session = Arc::new(engine.begin_session(SessionId::new("ses_1")));
        (engine, session, State::new())
    }

    #[tokio::test]
    async fn a_conversation_flows_from_transcripts_to_durable_memory() {
        let (engine, session, state) = setup();
        let (sender, rx) = channel(64);
        let loop_handle = tokio::spawn(run_memory_control_loop(rx, session.clone(), state.clone()));

        sender.user_activity_started(TurnId(1));
        sender.input_transcript(TurnId(1), "I am", false);
        sender.input_transcript(TurnId(1), "I am pescatarian", true);
        sender.turn_completed(TurnId(1));
        sender.session_ended();

        loop_handle.await.unwrap();

        let stored = engine.repository().all(engine.user()).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].statement, "The user is pescatarian.");
    }

    #[tokio::test]
    async fn state_carries_the_frozen_snapshot_for_the_in_flight_turn() {
        let (_engine, session, state) = setup();
        let (sender, rx) = channel(64);
        let loop_handle = tokio::spawn(run_memory_control_loop(rx, session.clone(), state.clone()));

        sender.user_activity_started(TurnId(1));
        sender.input_transcript(TurnId(1), "I am pescatarian", true);
        sender.turn_completed(TurnId(1));

        sender.user_activity_started(TurnId(2));
        sender.input_transcript(
            TurnId(2),
            "what do you remember about my dietary preferences",
            true,
        );
        sender.turn_completed(TurnId(2));
        sender.session_ended();
        loop_handle.await.unwrap();

        assert_eq!(state.get_key(&keys::CURRENT_TURN), Some(TurnId(2)));
        assert!(state.get_key(&keys::MEMORY_GENERATION).unwrap_or(0) > 0);
        assert!(state.get_key(&keys::TRANSCRIPT_FINAL).is_some());
    }

    #[tokio::test]
    async fn partial_transcripts_never_become_evidence() {
        let (_engine, session, state) = setup();
        let (sender, rx) = channel(64);
        let loop_handle = tokio::spawn(run_memory_control_loop(rx, session.clone(), state.clone()));

        sender.user_activity_started(TurnId(1));
        // A partial the recognizer later revises away entirely.
        sender.input_transcript(TurnId(1), "I am vegetarian", false);
        sender.input_transcript(TurnId(1), "I am pescatarian", true);
        sender.turn_completed(TurnId(1));
        sender.session_ended();
        loop_handle.await.unwrap();

        let candidates = session.ledger().usable_candidates();
        assert!(
            candidates
                .iter()
                .all(|c| !c.canonical_statement.contains("vegetarian")),
            "a revised-away partial became evidence: {candidates:?}"
        );
    }

    #[tokio::test]
    async fn the_loop_ends_cleanly_when_the_channel_closes() {
        let (_engine, session, state) = setup();
        let (sender, rx) = channel(4);
        let loop_handle = tokio::spawn(run_memory_control_loop(rx, session, state));
        drop(sender);
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn a_tool_call_before_any_turn_still_finds_a_snapshot() {
        let state = State::new();
        assert!(snapshot_for_turn(&state).is_empty());
    }
}
