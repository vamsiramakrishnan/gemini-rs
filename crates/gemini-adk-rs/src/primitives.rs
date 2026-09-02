//! # The L1 contract — the conversation runtime
//!
//! This module is the *engineered surface* of the runtime layer: the closed
//! set of primitives L1 exposes upward, curated and named. L0 moves frames;
//! this layer gives those frames meaning — and enforces it.
//!
//! **L1 promises:** a concurrent session runtime over the L0 stream — typed
//! shared [`State`], tool dispatch, governed flows with load-time compilation
//! and a self-explaining monitor, out-of-band extraction that fills the state
//! guards read, phases and watchers, transcripts, persistence, and a
//! [`LiveHandle`] that can always answer *why* (`explain`, truth traces)
//! and steer *now* (`update_step_posture`).
//!
//! **L1 never:** opens its own idea of a socket beyond L0's transport,
//! renders application prose, or hides an enforcement decision — every denial
//! carries its reason, every stuck guard can print its atoms.
//!
//! The primitives, by concern:
//!
//! | Concern | Primitives |
//! |---|---|
//! | Shared truth | [`State`], [`StateKey`], [`PrefixedState`] |
//! | Capability | [`ToolFunction`], [`SimpleTool`], [`TypedTool`], [`ToolDispatcher`] |
//! | Governance | [`Flow`], [`Step`], [`Guard`], [`Pred`], [`Constraint`], [`CompiledFlow`], [`FlowMonitor`], [`Enforcement`], [`Marking`], [`Verdict`] |
//! | Explanation | [`FlowExplanation`], [`GuardTrace`], [`Violation`] |
//! | Understanding | [`TurnExtractor`], [`LlmExtractor`], [`FieldPromotion`], [`ExtractionTrigger`] |
//! | Steering | [`Phase`], [`PhaseMachine`], [`Transition`], [`InstructionModifier`], [`Watcher`] |
//! | The session | [`LiveSessionBuilder`], [`LiveHandle`], [`LiveEvent`], [`EventCallbacks`], [`TranscriptBuffer`] |
//! | Memory of it | [`SessionPersistence`], [`SessionSnapshot`], [`FsPersistence`], [`MemoryPersistence`] |
//! | Models | [`BaseLlm`], [`LlmRequest`], [`LlmResponse`] |

pub use crate::state::{PrefixedState, State, StateKey};

pub use crate::tool::{SimpleTool, ToolDispatcher, ToolFunction, TypedTool};

pub use crate::flow::{
    CompiledFlow, Constraint, Enforcement, Flow, FlowExplanation, FlowMonitor, Guard, GuardTrace,
    Marking, Pred, Step, Verdict, Violation,
};

pub use crate::live::extractor::{ExtractionTrigger, FieldPromotion, LlmExtractor, TurnExtractor};

pub use crate::live::builder::LiveSessionBuilder;
pub use crate::live::callbacks::EventCallbacks;
pub use crate::live::events::LiveEvent;
pub use crate::live::handle::LiveHandle;
pub use crate::live::phase::{InstructionModifier, Phase, PhaseMachine, Transition};
pub use crate::live::transcript::TranscriptBuffer;
pub use crate::live::watcher::Watcher;

pub use crate::live::persistence::{
    FsPersistence, MemoryPersistence, SessionPersistence, SessionSnapshot,
};

pub use crate::llm::{BaseLlm, LlmRequest, LlmResponse};

#[cfg(test)]
mod contract {
    //! The drift guard: every primitive the module docs name must exist and
    //! be reachable from this path.
    #[test]
    fn every_named_primitive_is_reachable() {
        use super::*;
        fn is_type<T: ?Sized>() {}
        is_type::<State>();
        is_type::<StateKey<u32>>();
        is_type::<PrefixedState>();
        is_type::<dyn ToolFunction>();
        is_type::<SimpleTool>();
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        struct Args {}
        is_type::<TypedTool<Args>>();
        is_type::<ToolDispatcher>();
        is_type::<Flow>();
        is_type::<Step>();
        is_type::<Guard>();
        is_type::<Pred>();
        is_type::<Constraint>();
        is_type::<CompiledFlow>();
        is_type::<FlowMonitor>();
        is_type::<Enforcement>();
        is_type::<Marking>();
        is_type::<Verdict>();
        is_type::<FlowExplanation>();
        is_type::<GuardTrace>();
        is_type::<Violation>();
        is_type::<dyn TurnExtractor>();
        is_type::<LlmExtractor>();
        is_type::<FieldPromotion>();
        is_type::<ExtractionTrigger>();
        is_type::<Phase>();
        is_type::<PhaseMachine>();
        is_type::<Transition>();
        is_type::<InstructionModifier>();
        is_type::<Watcher>();
        is_type::<LiveSessionBuilder>();
        is_type::<LiveHandle>();
        is_type::<LiveEvent>();
        is_type::<EventCallbacks>();
        is_type::<TranscriptBuffer>();
        is_type::<dyn SessionPersistence>();
        is_type::<SessionSnapshot>();
        is_type::<FsPersistence>();
        is_type::<MemoryPersistence>();
        is_type::<dyn BaseLlm>();
        is_type::<LlmRequest>();
        is_type::<LlmResponse>();
    }
}
