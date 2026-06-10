#![warn(missing_docs)]
//! # gemini-adk-fluent-rs
//!
//! Fluent developer experience layer for the Gemini Live agent stack.
//! This is the highest-level crate in the workspace, providing a builder API,
//! operator algebra, and composition modules that sit on top of
//! [`gemini_adk_rs`] (agent runtime) and [`gemini_genai_rs`] (wire protocol).
//!
//! ## Module Organization
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`builder`] | Copy-on-write immutable `AgentBuilder` for declarative agent configuration |
//! | [`compose`] | S·C·T·P·M·A operator algebra for composing agent primitives |
//! | [`live`] | `Live` session handle — callback-driven full-duplex event handling |
//! | [`live_builders`] | Builder types for live session configuration |
//! | [`operators`] | Operator combinators for composing agents |
//! | [`patterns`] | Pre-built composition patterns for common use cases |
//! | [`testing`] | Test utilities and mock helpers |
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use gemini_adk_fluent_rs::prelude::*;
//!
//! let agent = AgentBuilder::new("my-agent")
//!     .model(GeminiModel::Gemini2_0Flash)
//!     .instruction("You are a helpful assistant.")
//!     .build();
//! ```
//!
//! ## Relationship to Other Crates
//!
//! - **`gemini-live`** (L0): Wire protocol, transport, types — re-exported via [`gemini_genai_rs`]
//! - **`gemini-adk-rs`** (L1): Agent runtime, tools, sessions — re-exported via [`gemini_adk_rs`]
//! - **`gemini-adk-fluent-rs`** (L2): This crate — ergonomic builder API and composition

pub mod a2a;
pub mod builder;
pub mod compose;
pub mod conversation;
pub mod flow_macros;
pub mod live;
pub mod live_builders;
pub mod motifs;
pub mod operators;
pub mod patterns;
pub mod policy;
pub mod simulation;
pub mod testing;

pub use gemini_adk_rs;
pub use gemini_genai_rs;

// ---------------------------------------------------------------------------
// Curated submodule homes (gap #9 — prelude hard carve).
//
// The kernel `prelude` (below) re-exports only the ~30 types a typical app
// touches. Everything else lives in these focused, discoverable modules so
// `use gemini_adk_fluent_rs::prelude::*` stays small and the rest is one
// `use gemini_adk_fluent_rs::{live, text, tools, …}` away. Import from the
// highest-level crate you need.
// ---------------------------------------------------------------------------

/// Text-agent runtime and combinators (carved out of the kernel `prelude`).
///
/// `LlmTextAgent`, the sequential/parallel/loop/fallback/route/race/timeout/map
/// combinators, and the `TextAgent` trait.
pub mod text {
    pub use gemini_adk_rs::text::*;
}

/// Tool definitions, dispatch, toolsets, and the confirmation flow.
pub mod tools {
    pub use gemini_adk_rs::confirmation::*;
    pub use gemini_adk_rs::tool::*;
    pub use gemini_adk_rs::toolset::*;
}

/// Concurrent typed state: `State`, `PrefixedState`, `StateKey`, prefix scopes.
pub mod state {
    pub use gemini_adk_rs::state::*;
}

/// Governed-conversation flow primitives: `Flow`, `Step`, `Guard`, `FlowMonitor`.
pub mod flow {
    pub use gemini_adk_rs::flow::*;
}

/// Agent builders, the agent trait, and the operator/pattern combinators.
///
/// Note: the L1 [`gemini_adk_rs::agent::Agent`] *trait* is re-exported here as
/// [`AgentTrait`] to avoid colliding with the L2 [`Agent`](crate::builder::Agent)
/// builder alias.
pub mod agents {
    pub use crate::builder::*;
    pub use crate::operators::*;
    pub use crate::patterns::*;
    #[doc(inline)]
    pub use gemini_adk_rs::agent::Agent as AgentTrait;
}

/// L0 wire-protocol types for raw WebSocket access.
pub mod wire {
    pub use gemini_genai_rs::prelude::*;
}

/// Clone multiple bindings for use in `move` closures, reducing Arc/clone boilerplate.
///
/// # Example
///
/// ```rust,ignore
/// use gemini_adk_fluent_rs::let_clone;
/// use std::sync::Arc;
///
/// let state = Arc::new(42);
/// let writer = Arc::new("hello");
///
/// let_clone!(state, writer);
/// tokio::spawn(async move {
///     println!("{state} {writer}");
/// });
/// ```
#[macro_export]
macro_rules! let_clone {
    ($($name:ident),+ $(,)?) => {
        $(let $name = $name.clone();)+
    };
}

/// Convenience re-exports for common types across all layers.
pub mod prelude {
    pub use crate::a2a::{A2AServer, A2aRegistry, RemoteAgent, SkillDeclaration};
    pub use crate::builder::*;
    pub use crate::compose::{Ctx, A, C, E, G, M, P, S, T};
    pub use crate::conversation::{
        CommitSpec, CompiledConversation, CompiledOverlay, Conversation, ConversationError,
        ConversationSpec, FlowStack, OverlaySpec, RepairPolicy, Resume, StageSpec, TransitionSpec,
    };
    pub use crate::live::Live;
    pub use crate::live_builders::*;
    pub use crate::motifs::Motif;
    pub use crate::operators::*;
    pub use crate::patterns::*;
    pub use crate::policy::{CommitPolicy, Policy};
    pub use crate::simulation::{Scenario, Sim, SimStep};
    pub use crate::testing::*;
    pub use crate::voice_flow;
    // The L1 `gemini_adk_rs::agent::Agent` *trait* collides with the L2 `Agent`
    // type alias (= AgentBuilder), so it is re-exported under the disambiguated
    // name `AgentTrait` (also available at `gemini_adk_fluent_rs::agents::AgentTrait`).
    #[doc(inline)]
    pub use gemini_adk_rs::agent::Agent as AgentTrait;
    pub use gemini_adk_rs::agent_session::*;
    pub use gemini_adk_rs::error::{AgentError, AgentResult, ToolError};
    pub use gemini_adk_rs::extract::{Recognizer, RecordExtractor};
    pub use gemini_adk_rs::flow::{
        render_ground, run as run_on_enter, CompiledFlow, Enforcement as FlowMode, Flow, FlowError,
        FlowErrors, FlowExplanation, FlowMonitor, Guard, StepAction, ToolPolicy, Verdict,
        Violation,
    };
    pub use gemini_adk_rs::frame::{
        ConfirmPolicy, FrameSpec, SlotRecognizer, SlotSpec, SlotValidator,
    };
    // Live session surface. Advanced *Contract / formatter types are intentionally
    // NOT in the prelude (import them from `gemini_adk_rs::live` when needed) —
    // the prelude carries the common session types, not the whole control plane.
    pub use gemini_adk_rs::live::{
        CallbackMode, ContextDelivery, DeferredWriter, EventCallbacks, ExtractionTrigger,
        FieldPromotion, FsPersistence, LiveEvent, LiveHandle, LiveSessionBuilder, LlmExtractor,
        MemoryPersistence, NeedsFulfillment, PendingContext, RepairAction, RepairConfig,
        RuntimeContract, SessionPersistence, SessionSnapshot, SoftTurnDetector, SteeringMode,
        ToolExecutionMode, TranscriptBuffer, TranscriptTurn, TurnExtractor,
    };
    pub use gemini_adk_rs::llm::{BaseLlm, GeminiLlm, GeminiLlmParams, LlmRequest, LlmResponse};
    pub use gemini_adk_rs::orchestration::{
        self, call as call_agent, provenance, Mode as AgentMode, Resolver,
    };
    pub use gemini_adk_rs::state::{SlotEvidence, State, StateKey};
    pub use gemini_adk_rs::text::{
        DispatchTextAgent, FallbackTextAgent, FnTextAgent, JoinTextAgent, LlmTextAgent,
        LoopTextAgent, MapOverTextAgent, ParallelTextAgent, RaceTextAgent, RouteRule,
        RouteTextAgent, SequentialTextAgent, TapTextAgent, TaskRegistry, TextAgent,
        TimeoutTextAgent,
    };
    // New ADK-JS parity types
    pub use gemini_adk_rs::confirmation::{
        ConfirmationProvider, ConfirmationRequest, StaticConfirmation, ToolConfirmation,
    };
    pub use gemini_adk_rs::context::{CallbackContext, ToolContext};
    pub use gemini_adk_rs::credentials::{
        AuthCredential, CredentialService, InMemoryCredentialService,
    };
    pub use gemini_adk_rs::instruction::inject_session_state;
    pub use gemini_adk_rs::llm::LlmRegistry;
    pub use gemini_adk_rs::run_config::{RunConfig, StreamingMode};
    pub use gemini_adk_rs::text_runner::InMemoryRunner;
    pub use gemini_adk_rs::toolset::{StaticToolset, Toolset};
    // Core tool types — surfaced at L2 so application code building tools
    // doesn't have to reach into the L1 crate directly.
    pub use gemini_adk_rs::tool::{SimpleTool, ToolDispatcher, ToolFunction, TypedTool};
    // The `#[tool]` attribute macro — turns an `async fn` into a registrable tool.
    // Re-exported from L1 (which re-exports it from the proc-macro crate), so the
    // fluent prelude doesn't need a direct dependency on `gemini-adk-macros-rs`.
    pub use gemini_adk_rs::tool;
    // Brings in *both* the `Extract` struct (type namespace) and the
    // `#[derive(Extract)]` macro (macro namespace) under one name, so the
    // builder (`Extract::record(..)`) and the derive are both usable.
    pub use gemini_adk_rs::Extract;
    // The `#[derive(Frame)]` macro — generates a `Frame` impl from `#[slot(..)]`.
    pub use gemini_adk_rs::Frame;
    pub use gemini_genai_rs::prelude::*;
}
