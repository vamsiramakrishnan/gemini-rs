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
pub mod contract;
pub(crate) mod control_plane;
pub mod effect_executor;
pub mod events;
pub mod extractor;
pub mod handle;
pub mod input_vad;
pub mod needs;
pub mod persistence;
pub mod phase;
pub(crate) mod processor;
pub mod reactor;
pub mod replay;
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
pub use contract::{
    ComputedContract, ControlContract, ExtractorContract, PhaseContract, PreparationContract,
    PromotionContract, RuntimeContract, ToolContract, TransitionContract, WatcherContract,
};
pub use effect_executor::LiveEffectExecutor;
pub use events::LiveEvent;
pub use extractor::{ExtractionTrigger, FieldPromotion, LlmExtractor, MergePolicy, TurnExtractor};
pub use handle::LiveHandle;
pub use input_vad::{BackendInputVad, BackendVadSnapshot};
pub use needs::{NeedsFulfillment, RepairAction, RepairConfig};
pub use persistence::{FsPersistence, MemoryPersistence, SessionPersistence, SessionSnapshot};
pub use phase::{
    EnterContextFn, InstructionModifier, Phase, PhaseHook, PhaseInstruction, PhaseMachine,
    PhasePreparation, PhaseTransition, StateGuard, Transition, TransitionEvaluation,
    TransitionResult, TransitionTrigger,
};
pub use processor::{Delivery, DeliveryConfig};
pub use reactor::{
    EffectMode, EffectPolicy, LiveEffect, LiveReactor, Reaction, ReactorEvent, ReactorRule,
    VoiceRuntimeState,
};
pub use replay::{attach_session, collect_events_until_idle, replay_session, ReplaySession};
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
