//! The [`MemoryBinding`] implementation — how a `SessionSpec`'s `memory`
//! section reaches the real engine.
//!
//! The spec side (in `gemini-adk-fluent-rs`) is pure data: slots, and
//! `remember` effects. This module is the other half of that seam: it hands
//! the declaration to [`LiveMemoryExt::with_memory_slots`] (tools, ingestion,
//! reconciliation, slot projection) and routes `remember` effects through
//! [`MemorySession::apply_explicit_command`] — the same path the
//! `manage_memory` tool takes, so a spec-authored remember and a user-asked
//! remember are indistinguishable downstream.

use std::sync::Arc;

use gemini_adk_fluent_rs::live::Live;
use gemini_adk_fluent_rs::spec::{MemoryBinding, MemorySpec};

use super::live::LiveMemoryExt;
use super::turn_extractor::MemorySlot;
use crate::core::MutationIntent;
use crate::engine::MemorySession;

/// [`MemoryBinding`] over a [`MemorySession`].
///
/// ```no_run
/// # use std::sync::Arc;
/// # use gemini_memory_rs::prelude::*;
/// # use gemini_memory_rs::runtime::SessionMemoryBinding;
/// # use gemini_adk_fluent_rs::spec::SpecResources;
/// let engine = MemoryEngine::in_memory(UserId::new("usr_1"));
/// let session = Arc::new(engine.begin_session(SessionId::new("ses_1")));
/// let resources = SpecResources {
///     memory: Some(Arc::new(SessionMemoryBinding::new(session))),
///     ..Default::default()
/// };
/// ```
pub struct SessionMemoryBinding {
    session: Arc<MemorySession>,
}

impl SessionMemoryBinding {
    /// Bind a memory session for spec-driven installation.
    pub fn new(session: Arc<MemorySession>) -> Self {
        Self { session }
    }

    /// The underlying session.
    pub fn session(&self) -> &Arc<MemorySession> {
        &self.session
    }
}

impl MemoryBinding for SessionMemoryBinding {
    fn install(&self, live: Live, memory: &MemorySpec) -> Live {
        let slots: Vec<MemorySlot> = memory
            .slots
            .iter()
            .filter_map(|s| {
                // Spec validation already rejects `derived:` targets; any slot
                // the constructor still refuses is dropped rather than
                // panicking a connect.
                MemorySlot::try_new(&s.predicate, &s.to).ok()
            })
            .collect();
        live.with_memory_slots(self.session.clone(), slots)
    }

    fn remember(&self, note: String) {
        let session = self.session.clone();
        tokio::spawn(async move {
            let turn = session.current_turn();
            let _ = session
                .apply_explicit_command(MutationIntent::Remember, &note, turn)
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SessionId, TurnId, UserId};
    use crate::engine::MemoryEngine;
    use gemini_adk_fluent_rs::spec::MemorySlotSpec;

    fn session() -> Arc<MemorySession> {
        let engine = MemoryEngine::in_memory(UserId::new("usr_1"));
        let session = Arc::new(engine.begin_session(SessionId::new("ses_1")));
        session.begin_turn(TurnId(1));
        session
    }

    #[tokio::test]
    async fn remember_commits_through_the_explicit_command_path() {
        let session = session();
        let binding = SessionMemoryBinding::new(session.clone());
        binding.remember("The caller prefers evening appointments".into());
        // The write is fire-and-forget; give the spawned task a beat.
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if !session.known_statements().is_empty() {
                break;
            }
        }
        assert!(
            session
                .known_statements()
                .iter()
                .any(|s| s.contains("evening")),
            "statements: {:?}",
            session.known_statements()
        );
    }

    #[tokio::test]
    async fn install_wires_slots_onto_the_builder() {
        let binding = SessionMemoryBinding::new(session());
        let memory = MemorySpec {
            slots: vec![MemorySlotSpec {
                predicate: "dietary_identity".into(),
                to: "user:diet".into(),
            }],
        };
        // Building without panicking is the contract here — the slot wiring
        // itself is covered by the runtime's own tests.
        let _ = binding.install(Live::builder(), &memory);
    }
}
