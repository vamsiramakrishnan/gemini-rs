//! Installing memory onto a `Live` session.
//!
//! Memory has exactly two touchpoints on a live conversation, and both are
//! mechanisms the runtime already owns:
//!
//! - **the extraction pipeline**, which runs [`MemoryTurnExtractor`] at each
//!   turn boundary and promotes its fields into governed `State`;
//! - **the tool dispatcher**, which serves `recall_context` and
//!   `manage_memory`.
//!
//! There is deliberately no third mechanism. An earlier revision of this module
//! carried its own channel, control loop and state keys; all of it duplicated
//! what `Live` already does, and every duplicated mechanism is a second place
//! for turn bookkeeping to drift.

use std::sync::Arc;

use gemini_adk_fluent_rs::compose::tools::ToolComposite;
use gemini_adk_fluent_rs::live::Live;

use super::tools::{manage_memory_tool, recall_context_tool};
use super::turn_extractor::{MemorySlot, MemoryTurnExtractor};
use crate::engine::MemorySession;

/// Both memory tools, as a composite that can be combined with `|`.
///
/// ```no_run
/// # use std::sync::Arc;
/// # use gemini_memory_rs::runtime::memory_tools;
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

/// Installs the memory subsystem onto a `Live` builder.
pub trait LiveMemoryExt: Sized {
    /// Wire memory in: ingestion, retrieval preparation, and the two tools.
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use gemini_adk_fluent_rs::live::Live;
    /// # use gemini_memory_rs::runtime::LiveMemoryExt;
    /// # fn demo(session: Arc<gemini_memory_rs::engine::MemorySession>) {
    /// Live::builder().with_memory(session);
    /// # }
    /// ```
    fn with_memory(self, session: Arc<MemorySession>) -> Self;

    /// Wire memory in, projecting remembered facts into governed `State` slots.
    ///
    /// A slot filled from memory satisfies `phase.needs(..)`,
    /// `phase.requires(..)` and `Flow` guards exactly as one filled by the user
    /// would — which is the point. The application asks for what it needs and
    /// does not have to care whether the answer arrived this minute or last
    /// month.
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use gemini_adk_fluent_rs::live::Live;
    /// # use gemini_memory_rs::runtime::{LiveMemoryExt, MemorySlot};
    /// # fn demo(session: Arc<gemini_memory_rs::engine::MemorySession>) {
    /// Live::builder()
    ///     .with_memory_slots(
    ///         session,
    ///         [
    ///             MemorySlot::new("dietary_identity", "user:diet"),
    ///             MemorySlot::new("venue_preference", "user:venue"),
    ///         ],
    ///     )
    ///     // Satisfied from memory for a returning user, so they are not
    ///     // asked again for something they already told us.
    ///     .phase("plan_dinner")
    ///         .needs(&["user:diet", "user:venue"])
    ///         .done();
    /// # }
    /// ```
    fn with_memory_slots(
        self,
        session: Arc<MemorySession>,
        slots: impl IntoIterator<Item = MemorySlot>,
    ) -> Self;
}

impl LiveMemoryExt for Live {
    fn with_memory(self, session: Arc<MemorySession>) -> Self {
        self.with_memory_slots(session, Vec::new())
    }

    fn with_memory_slots(
        self,
        session: Arc<MemorySession>,
        slots: impl IntoIterator<Item = MemorySlot>,
    ) -> Self {
        let extractor = MemoryTurnExtractor::new(session.clone()).slots(slots);
        let vocabulary = session.clone();
        self.with_tools(memory_tools(session))
            // Registering an extractor also enables transcription, so callers
            // need not remember to turn it on.
            .extractor(Arc::new(extractor))
            // The memory map, delivered as an amendment rather than folded into
            // the caller's instruction.
            //
            // `recall_context` takes `about` and `attribute`, and those are
            // worth nothing unless the model can name the values. Measured over
            // 93 questions, a model asked cold names the right predicate 2% of
            // the time — below the 8% at which a soft filter starts paying for
            // itself — and 69% when shown this list. End to end that is an
            // expected 78.3 of 93 against 84.3.
            //
            // An amendment, for three reasons. It composes with whatever
            // instruction the caller wrote instead of replacing it. It is
            // re-evaluated, so a corpus that grows mid-session is reflected
            // rather than frozen at connect — which matters because Live fixes
            // tool declarations at connect and this could not live in the
            // schema for that reason. And it returns `None` for an empty
            // corpus, so a new user is not handed an empty list to filter by.
            .instruction_amendment(move |_state| {
                let map = vocabulary.memory_map();
                (!map.is_empty()).then_some(map)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SessionId, UserId};
    use crate::engine::MemoryEngine;

    fn session() -> Arc<MemorySession> {
        let engine = MemoryEngine::in_memory(UserId::new("usr_1"));
        Arc::new(engine.begin_session(SessionId::new("ses_1")))
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

    #[test]
    fn installation_composes_with_the_rest_of_the_builder() {
        // The point of riding the platform's own mechanisms: memory is just
        // another extractor and another tool, so nothing else has to move.
        let _live = Live::builder()
            .instruction("You are a companion.")
            .with_memory_slots(
                session(),
                [MemorySlot::new("dietary_identity", "user:diet")],
            )
            .phase("plan")
            .needs(&["user:diet"])
            .done()
            .initial_phase("plan");
    }
}
