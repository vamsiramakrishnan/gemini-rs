//! Live session management — callback-driven full-duplex event handling.

pub mod builder;
pub mod callbacks;
pub mod computed;
pub mod extractor;
pub mod handle;
pub mod phase;
pub(crate) mod processor;
pub mod session_signals;
pub mod transcript;

pub use builder::LiveSessionBuilder;
pub use callbacks::EventCallbacks;
pub use computed::{ComputedRegistry, ComputedVar};
pub use extractor::{LlmExtractor, TurnExtractor};
pub use handle::LiveHandle;
pub use phase::{Phase, PhaseInstruction, PhaseMachine, PhaseTransition, Transition};
pub use session_signals::{SessionSignals, SessionType};
pub use transcript::{TranscriptBuffer, TranscriptTurn};
