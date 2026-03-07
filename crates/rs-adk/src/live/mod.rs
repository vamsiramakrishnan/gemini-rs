//! Live session management — callback-driven full-duplex event handling.

use std::future::Future;
use std::pin::Pin;

/// A boxed future type used across live session modules.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub mod background_agent_dispatch;
pub mod background_tool;
pub mod builder;
pub mod callbacks;
pub mod computed;
pub mod context_builder;
pub mod context_writer;
pub(crate) mod control_plane;
pub mod events;
pub mod extractor;
pub mod handle;
pub mod needs;
pub mod persistence;
pub mod phase;
pub(crate) mod processor;
pub mod session_signals;
pub mod soft_turn;
pub mod steering;
pub mod telemetry;
pub mod temporal;
pub mod transcript;
pub mod watcher;

pub use background_agent_dispatch::BackgroundAgentDispatcher;
pub use background_tool::{
    BackgroundToolTracker, DefaultResultFormatter, ResultFormatter, ToolExecutionMode,
};
pub use builder::LiveSessionBuilder;
pub use callbacks::{CallbackMode, EventCallbacks};
pub use computed::{ComputedRegistry, ComputedVar};
pub use context_builder::ContextBuilder;
pub use context_writer::{DeferredWriter, PendingContext};
pub use events::LiveEvent;
pub use extractor::{ExtractionTrigger, LlmExtractor, TurnExtractor};
pub use handle::LiveHandle;
pub use needs::{NeedsFulfillment, RepairAction, RepairConfig};
pub use persistence::{FsPersistence, MemoryPersistence, SessionPersistence, SessionSnapshot};
pub use phase::{
    InstructionModifier, Phase, PhaseInstruction, PhaseMachine, PhaseTransition, Transition,
    TransitionResult, TransitionTrigger,
};
pub use session_signals::{SessionSignals, SessionType};
pub use soft_turn::SoftTurnDetector;
pub use steering::{ContextDelivery, SteeringMode};
pub use telemetry::SessionTelemetry;
pub use temporal::{
    ConsecutiveFailureDetector, PatternDetector, RateDetector, SustainedDetector, TemporalPattern,
    TemporalRegistry, TurnCountDetector,
};
pub use transcript::{ToolCallSummary, TranscriptBuffer, TranscriptTurn, TranscriptWindow};
pub use watcher::{PredicateFn, WatchPredicate, Watcher, WatcherRegistry};
