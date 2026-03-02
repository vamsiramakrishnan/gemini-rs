//! Live session management — callback-driven full-duplex event handling.

pub mod builder;
pub mod callbacks;
pub mod extractor;
pub mod handle;
pub(crate) mod processor;
pub mod transcript;

pub use builder::LiveSessionBuilder;
pub use callbacks::EventCallbacks;
pub use extractor::{LlmExtractor, TurnExtractor};
pub use handle::LiveHandle;
pub use transcript::{TranscriptBuffer, TranscriptTurn};
