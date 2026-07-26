//! One-call installation into a `Live` session.
//!
//! Memory has three touchpoints on a live conversation, and wiring them by hand
//! is three chances to get it subtly wrong — a fast-lane callback that
//! allocates, a turn extractor that never gets registered, a tool that holds a
//! stale session. [`LiveMemoryExt::with_memory`] installs all three together:
//!
//! - the two tools, composed so they can be `|`-ed with any others;
//! - the ingestion [`MemoryTurnExtractor`], on the runtime's own extraction
//!   pipeline;
//! - the fast-lane transcript bridge that drives speculative retrieval, which
//!   the turn pipeline structurally cannot do because it only sees finalized
//!   turns.

use std::sync::Arc;

use gemini_adk_fluent_rs::compose::tools::ToolComposite;
use gemini_adk_fluent_rs::live::Live;
use gemini_adk_rs::state::State;

use super::events::{channel, MemoryEventSender, DEFAULT_CHANNEL_DEPTH};
use super::tools::{manage_memory_tool, recall_context_tool};
use super::turn_extractor::MemoryTurnExtractor;
use crate::core::TurnId;
use crate::engine::MemorySession;

/// Both memory tools, as a composite that can be combined with `|`.
///
/// ```no_run
/// # use std::sync::Arc;
/// # use gemini_memory_rs::runtime::live::memory_tools;
/// # use gemini_adk_fluent_rs::compose::T;
/// # fn demo(session: Arc<gemini_memory_rs::engine::MemorySession>) {
/// let tools = memory_tools(session) | T::google_search();
/// # let _ = tools;
/// # }
/// ```
pub fn memory_tools(session: Arc<MemorySession>) -> ToolComposite {
    ToolComposite::from_function(Arc::new(recall_context_tool(session.clone())))
        | ToolComposite::from_function(Arc::new(manage_memory_tool(session)))
}

/// The handle a caller keeps after installing memory on a session.
pub struct MemoryInstallation {
    /// The fast-lane bridge. Clone it into any additional callbacks.
    pub sender: MemoryEventSender,
    /// The session state the control loop publishes into.
    pub state: State,
    /// The control-loop task, which ends when the session is sealed.
    pub control_loop: tokio::task::JoinHandle<()>,
}

impl MemoryInstallation {
    /// Signal that the conversation is over and wait for reconciliation.
    pub async fn finish(self) {
        self.sender.session_ended();
        let _ = self.control_loop.await;
    }
}

/// Installs the memory subsystem onto a `Live` builder.
pub trait LiveMemoryExt: Sized {
    /// Wire memory into this session: tools, ingestion, and speculation.
    ///
    /// Returns the builder alongside the handle needed to drive and close the
    /// subsystem. Transcription is enabled by the extractor registration, so
    /// callers need not remember to turn it on.
    fn with_memory(self, session: Arc<MemorySession>) -> (Self, MemoryInstallation);
}

impl LiveMemoryExt for Live {
    fn with_memory(self, session: Arc<MemorySession>) -> (Self, MemoryInstallation) {
        let state = State::new();
        let (sender, receiver) = channel(DEFAULT_CHANNEL_DEPTH);
        let control_loop = tokio::spawn(super::control::run_memory_control_loop(
            receiver,
            session.clone(),
            state.clone(),
        ));

        // The turn counter is owned here rather than read from the session,
        // because the fast lane must not take a lock to learn which turn it is.
        let turn = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let builder = self
            .with_tools(memory_tools(session.clone()))
            .extractor(Arc::new(MemoryTurnExtractor::new(session)))
            .on_vad_start({
                let sender = sender.clone();
                let turn = turn.clone();
                move || {
                    let next = turn.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    sender.user_activity_started(TurnId(next));
                }
            })
            .on_input_transcript({
                let sender = sender.clone();
                let turn = turn.clone();
                move |text, is_final| {
                    let current = turn.load(std::sync::atomic::Ordering::Relaxed).max(1);
                    sender.input_transcript(TurnId(current), text, is_final);
                }
            })
            .on_turn_complete({
                let sender = sender.clone();
                let turn = turn.clone();
                move || {
                    let current = turn.load(std::sync::atomic::Ordering::Relaxed).max(1);
                    let sender = sender.clone();
                    async move {
                        sender.turn_completed(TurnId(current));
                    }
                }
            });

        (
            builder,
            MemoryInstallation {
                sender,
                state,
                control_loop,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SessionId, UserId};
    use crate::engine::MemoryEngine;

    fn engine_and_session() -> (Arc<MemoryEngine>, Arc<MemorySession>) {
        let engine = Arc::new(MemoryEngine::in_memory(UserId::new("usr_1")));
        let session = Arc::new(engine.begin_session(SessionId::new("ses_1")));
        (engine, session)
    }

    fn session() -> Arc<MemorySession> {
        engine_and_session().1
    }

    #[test]
    fn the_composite_exposes_both_tools_and_composes_with_others() {
        let composite = memory_tools(session());
        assert_eq!(composite.len(), 2);

        let names: Vec<String> = composite
            .entries
            .iter()
            .filter_map(|e| match e {
                gemini_adk_fluent_rs::compose::tools::ToolCompositeEntry::Function(f) => {
                    Some(gemini_adk_rs::tool::ToolFunction::name(f.as_ref()).to_string())
                }
                _ => None,
            })
            .collect();
        assert!(names.contains(&super::super::tools::RECALL_TOOL.to_string()));
        assert!(names.contains(&super::super::tools::MANAGE_TOOL.to_string()));

        let combined = memory_tools(session()) | gemini_adk_fluent_rs::compose::T::google_search();
        assert_eq!(combined.len(), 3);
    }

    #[tokio::test]
    async fn installation_carries_a_turn_from_transcript_to_durable_memory() {
        let (engine, session) = engine_and_session();
        let (_builder, installation) = Live::builder()
            .instruction("You are a companion.")
            .with_memory(session);

        // Drive a turn entirely through the fast-lane bridge, exactly as the
        // Live callbacks would.
        installation.sender.user_activity_started(TurnId(1));
        installation
            .sender
            .input_transcript(TurnId(1), "I am", false);
        installation
            .sender
            .input_transcript(TurnId(1), "I am pescatarian", true);
        installation.sender.turn_completed(TurnId(1));
        installation.finish().await;

        let stored = engine.repository().all(engine.user()).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].statement.contains("pescatarian"));
    }

    #[tokio::test]
    async fn the_bridge_never_blocks_even_when_the_loop_is_gone() {
        let (_builder, installation) = Live::builder().with_memory(session());
        installation.control_loop.abort();
        // Offering into a dead loop returns rather than panicking or blocking.
        for _ in 0..1000 {
            installation
                .sender
                .partial_transcript(TurnId(1), "still talking");
        }
    }
}
