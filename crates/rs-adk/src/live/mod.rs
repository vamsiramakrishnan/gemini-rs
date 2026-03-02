//! Live session management — callback-driven full-duplex event handling.

use std::future::Future;
use std::pin::Pin;

/// A boxed future type used across live session modules.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub mod background_tool;
pub mod builder;
pub mod callbacks;
pub mod computed;
pub mod extractor;
pub mod handle;
pub mod phase;
pub(crate) mod processor;
pub mod session_signals;
pub mod temporal;
pub mod transcript;
pub mod watcher;

pub use background_tool::{BackgroundToolTracker, DefaultResultFormatter, ResultFormatter, ToolExecutionMode};
pub use builder::LiveSessionBuilder;
pub use callbacks::EventCallbacks;
pub use computed::{ComputedRegistry, ComputedVar};
pub use extractor::{LlmExtractor, TurnExtractor};
pub use handle::LiveHandle;
pub use phase::{Phase, PhaseInstruction, PhaseMachine, PhaseTransition, Transition};
pub use session_signals::{SessionSignals, SessionType};
pub use temporal::{
    ConsecutiveFailureDetector, PatternDetector, RateDetector, SustainedDetector,
    TemporalPattern, TemporalRegistry, TurnCountDetector,
};
pub use transcript::{TranscriptBuffer, TranscriptTurn};
pub use watcher::{PredicateFn, WatchPredicate, Watcher, WatcherRegistry};
