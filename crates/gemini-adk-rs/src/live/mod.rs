//! Live session management — callback-driven full-duplex event handling.

use std::sync::Arc;

use gemini_genai_rs::session::SessionWriter;

pub use crate::BoxFuture;
use crate::state::State;

/// How an async hook or effect runs relative to the control lane.
///
/// Every control-lane callback in [`EventCallbacks`] has a companion `_mode`
/// field (e.g. `on_turn_complete_mode`), and every reactor
/// [`EffectPolicy`] carries one. At the L2 fluent API level, `_concurrent`
/// suffixed setters (e.g. `on_turn_complete_concurrent()`) set the callback
/// and select [`Concurrent`](Self::Concurrent) in one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// Awaited inline — the control lane waits for completion before the next
    /// event or effect. Guarantees ordering and state consistency.
    #[default]
    Blocking,
    /// Spawned as a detached tokio task — the control lane continues
    /// immediately. Use for fire-and-forget work: logging, analytics, webhook
    /// dispatch, background agent triggering.
    Concurrent,
}

/// An async session hook: receives a clone of the shared [`State`] and the
/// session writer. The shape of phase `on_enter`/`on_exit`, phase
/// preparations, and temporal-pattern actions.
pub type SessionHook = Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>;

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
pub mod redaction;
pub mod replay;
pub mod session_signals;
pub mod soft_turn;
pub mod steering;
pub mod telemetry;
pub mod temporal;
pub mod transcript;
pub mod turn_commit;
pub mod watcher;

pub use background_agent_dispatch::BackgroundAgentDispatcher;
pub use background_tool::{
    BackgroundToolTracker, DefaultResultFormatter, ResultFormatter, ToolExecutionMode,
};
pub use builder::LiveSessionBuilder;
pub use callbacks::EventCallbacks;
pub use computed::{ComputedRegistry, ComputedVar};
pub use context_builder::ContextBuilder;
pub use context_writer::{DeferredWriter, PendingContext};
pub use contract::{
    ComputedContract, ControlContract, ExtractorContract, PhaseContract, PreparationContract,
    PromotionContract, RuntimeContract, ToolContract, TransitionContract, WatcherContract,
};
pub use effect_executor::LiveEffectExecutor;
pub use events::{LiveEvent, LiveEventStream};
pub use extractor::{ExtractionTrigger, FieldPromotion, LlmExtractor, MergePolicy, TurnExtractor};
pub use handle::LiveHandle;
pub use input_vad::{ActivityAuthority, BackendInputVad, BackendVadSnapshot, InputAudioProcessor};
pub use needs::{NeedsFulfillment, RepairAction, RepairConfig};
pub use persistence::{
    FsPersistence, MemoryPersistence, PersistenceError, SessionPersistence, SessionSnapshot,
};
pub use phase::{
    EnterContextFn, InstructionModifier, Phase, PhaseInstruction, PhaseMachine, PhasePreparation,
    Transition, TransitionEvaluation, TransitionRecord, TransitionResult, TransitionTrigger,
};
pub use processor::{Delivery, DeliveryConfig};
pub use reactor::{
    EffectPolicy, LiveEffect, LiveReactor, Reaction, ReactorEvent, ReactorRule, VoiceRuntimeState,
};
pub use replay::{ReplaySession, attach_session, collect_events_until_idle, replay_session};
pub use session_signals::{SessionSignals, SessionType};
pub use soft_turn::SoftTurnDetector;
pub use steering::{ContextDelivery, SteeringMode};
pub use telemetry::SessionTelemetry;
pub use temporal::{
    ConsecutiveFailureDetector, PatternDetector, RateDetector, SustainedDetector, TemporalPattern,
    TemporalRegistry, TurnCountDetector,
};
pub use transcript::{ToolCallSummary, TranscriptBuffer, TranscriptTurn, TranscriptWindow};
pub use turn_commit::{TurnCommitConfig, TurnCommitPolicy, TurnSignal};
pub use watcher::{PredicateFn, WatchPredicate, Watcher, WatcherRegistry};
