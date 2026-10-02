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

/// [`MemoryBinding`] over one [`MemorySession`] and authenticated memory subject.
///
/// Task activations share this session's backend, while each declares its own
/// projected slots. Use one binding per logical Live session. Accepted task
/// effects and contextual memory tools serialize with awaited reconciliation;
/// task cancellation does not undo an effect that already started.
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
    task_session: Arc<super::task_binding::TaskMemorySession>,
}

impl SessionMemoryBinding {
    /// Bind a memory session for spec-driven installation.
    pub fn new(session: Arc<MemorySession>) -> Self {
        Self {
            task_session: super::task_binding::TaskMemorySession::new(session.clone()),
            session,
        }
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

    fn task_memory(
        &self,
        memory: &MemorySpec,
    ) -> Result<Arc<dyn gemini_adk_rs::tasks::TaskMemoryService>, String> {
        let slots = memory
            .slots
            .iter()
            .map(|slot| {
                MemorySlot::try_new(&slot.predicate, &slot.to).map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.task_session.view(slots))
    }

    fn task_tools(&self) -> Vec<Arc<dyn gemini_adk_rs::tool::ToolFunction>> {
        self.task_session.tools()
    }

    fn install_task_lifecycle(&self, live: Live) -> Live {
        let shared = self.task_session.clone();
        live.on_teardown(move || {
            let shared = shared.clone();
            async move {
                if let Err(error) = shared.finish().await {
                    tracing::error!(%error, "task memory reconciliation failed");
                }
            }
        })
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
