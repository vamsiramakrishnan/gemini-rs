//! Agent-to-Agent (A2A) protocol types and converters.

/// A2A protocol data types (messages, tasks, artifacts, parts).
pub mod types;
/// Bidirectional conversion between A2A parts and GenAI parts.
pub mod part_converter;
/// Bidirectional conversion between A2A messages and agent events.
pub mod event_converter;

pub use types::{
    A2aArtifact, A2aFileContent, A2aMessage, A2aPart, A2aTask,
    TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent,
};
pub use part_converter::{to_a2a_parts, to_a2a_part, to_genai_parts, to_genai_part};
pub use event_converter::{to_a2a_message, to_adk_event};
