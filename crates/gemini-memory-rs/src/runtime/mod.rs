//! Wiring the memory engine into a Gemini Live session.
//!
//! Memory rides the runtime's own mechanisms rather than adding new ones: it is
//! a [`TurnExtractor`](gemini_adk_rs::live::extractor::TurnExtractor) on the
//! existing extraction pipeline, and two tools on the existing dispatcher.
//! Facts it recalls are projected into governed `State`, where the phase
//! machine, `Flow` guards, watchers and repair already read.

#[cfg(feature = "fluent")]
pub mod live;
#[cfg(feature = "fluent")]
pub mod spec_binding;
pub mod tools;
pub mod turn_extractor;

#[cfg(feature = "fluent")]
pub use live::{memory_tools, LiveMemoryExt};
#[cfg(feature = "fluent")]
pub use spec_binding::SessionMemoryBinding;
pub use tools::{
    manage_memory_tool, recall_context_tool, ManageArgs, RecallArgs, RecallScope, MANAGE_TOOL,
    MEMORY_TOOLS, RECALL_TOOL,
};
pub use turn_extractor::{MemorySlot, MemorySlotError, MemoryTurnExtractor, MEMORY_EXTRACTOR_NAME};
