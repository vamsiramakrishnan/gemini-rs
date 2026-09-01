#![warn(unreachable_pub)]
#![forbid(unsafe_code)]
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
pub mod handoff;
pub mod live;
pub mod live_builders;
pub mod motifs;
pub mod operators;
pub mod patterns;
pub mod policy;
pub mod primitives;
pub mod simulation;
pub mod spec;
pub mod telephony;
pub mod testing;
pub mod voice;

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

/// Tool definitions, dispatch, toolsets, the confirmation flow, and frames.
pub mod tools {
    pub use gemini_adk_rs::confirmation::*;
    pub use gemini_adk_rs::extract::{Recognizer, RecordExtractor};
    pub use gemini_adk_rs::frame::{
        ConfirmPolicy, FrameSpec, SlotRecognizer, SlotSpec, SlotValidator,
    };
    pub use gemini_adk_rs::tool::*;
    pub use gemini_adk_rs::toolset::*;
}

/// LLM abstraction: params, requests/responses, and the registry.
pub mod llm {
    pub use gemini_adk_rs::llm::*;
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
/// `AgentTrait` to avoid colliding with the L2 `Agent` (the [`AgentBuilder`](crate::builder::AgentBuilder))
/// builder alias.
pub mod agents {
    pub use crate::builder::*;
    pub use crate::operators::*;
    pub use crate::patterns::*;
    #[doc(inline)]
    pub use gemini_adk_rs::agent::Agent as AgentTrait;
    pub use gemini_adk_rs::agent_session::*;
    pub use gemini_adk_rs::orchestration::{
        self, Mode as AgentMode, Resolver, call as call_agent, provenance,
    };
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

/// The kernel prelude — the ~40 types a typical application touches.
///
/// Gap #9 carved this down from the previous everything-prelude. Anything not
/// here now lives in a focused submodule and is one import away:
///
/// | Need | Import |
/// |------|--------|
/// | Full Live control plane (persistence, repair, steering, transcripts, contracts) | `use gemini_adk_fluent_rs::live::*;` |
/// | Text-agent runtime details | `use gemini_adk_fluent_rs::text::*;` |
/// | Toolsets, confirmation, frames | `use gemini_adk_fluent_rs::tools::*;` |
/// | State prefixes / `SlotEvidence` | `use gemini_adk_fluent_rs::state::*;` |
/// | Full flow vocabulary (`CompiledFlow`, `StepAction`, `Violation`, …) | `use gemini_adk_fluent_rs::flow::*;` |
/// | Agent trait + operator/pattern internals | `use gemini_adk_fluent_rs::agents::*;` |
/// | Conversation compiler (`Conversation`, `ConversationSpec`, …) | `use gemini_adk_fluent_rs::conversation::*;` |
/// | A2A, motifs, policy, simulation, testing, orchestration, credentials, run_config | the same-named module, e.g. `use gemini_adk_fluent_rs::simulation::*;` |
/// | Raw L0 wire types | `use gemini_adk_fluent_rs::wire::*;` |
pub mod prelude {
    // ── Voice I/O: `.talk()` on a connected handle (feature `voice-io`) ──
    #[cfg(feature = "voice-io")]
    pub use crate::voice::Talk;

    // ── Builders, composition algebra, operators, patterns (headline DX) ──
    pub use crate::builder::*;
    pub use crate::compose::{A, C, Ctx, E, G, M, P, S, T};
    pub use crate::live::Live;
    // Dynamic instructions (ADK instruction-provider pattern) + tool media.
    pub use crate::operators::*;
    pub use crate::patterns::*;
    pub use gemini_adk_rs::instruction::InstructionProvider;
    #[cfg(feature = "templates")]
    pub use gemini_adk_rs::instruction::TemplateInstruction;
    pub use gemini_adk_rs::tool::media as tool_media;
    // Build-time validation DX (contract checking, data-flow inference, harness).
    pub use crate::testing::{
        AgentHarness, ContractViolation, DataFlowEdge, LiveViolation, check_contracts, check_live,
        diagnose, infer_data_flow,
    };

    // The L1 `gemini_adk_rs::agent::Agent` *trait* collides with the L2 `Agent`
    // type alias (= AgentBuilder), so it is re-exported under the disambiguated
    // name `AgentTrait` (also available at `gemini_adk_fluent_rs::agents::AgentTrait`).
    #[doc(inline)]
    pub use gemini_adk_rs::agent::Agent as AgentTrait;

    // ── Errors ──
    pub use gemini_adk_rs::error::{AgentError, AgentResult, ToolError};

    // ── Governed flow (core vocabulary; full set in `crate::flow`) ──
    pub use gemini_adk_rs::flow::{
        Enforcement as FlowMode, Flow, FlowMonitor, Guard, ToolPolicy, Verdict,
    };

    // ── State (prefix scopes + `SlotEvidence` in `crate::state`) ──
    pub use gemini_adk_rs::state::{State, StateKey};

    // ── LLM (core; request/response/registry in `crate::text`) ──
    pub use gemini_adk_rs::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};

    // ── Tools ──
    pub use gemini_adk_rs::tool::{SimpleTool, ToolDispatcher, ToolFunction, TypedTool};
    // The `#[tool]` attribute macro — turns an `async fn` into a registrable tool.
    pub use gemini_adk_rs::tool;
    // Brings in both the `Extract` struct and the `#[derive(Extract)]` macro.
    pub use gemini_adk_rs::Extract;
    // The `#[derive(Frame)]` macro.
    pub use gemini_adk_rs::Frame;

    // ── Callback contexts (used in `M::` hooks) ──
    pub use gemini_adk_rs::context::{CallbackContext, ToolContext};

    // ── Common Live session types (full control plane in `crate::live`) ──
    pub use gemini_adk_rs::live::{
        ContextDelivery, EventCallbacks, ExtractionTrigger, FsPersistence, LiveHandle,
        LlmExtractor, MemoryPersistence, RepairConfig, SessionPersistence, SoftTurnDetector,
        SteeringMode, TranscriptBuffer, TranscriptTurn, TurnExtractor,
    };

    // ── Text-agent combinators (runtime details in `crate::text`) ──
    pub use gemini_adk_rs::text::{
        DispatchTextAgent, FallbackTextAgent, FnTextAgent, JoinTextAgent, LlmTextAgent,
        LoopTextAgent, MapOverTextAgent, ParallelTextAgent, RaceTextAgent, RouteRule,
        RouteTextAgent, SequentialTextAgent, TapTextAgent, TaskRegistry, TextAgent,
        TimeoutTextAgent,
    };

    // ── L0 wire protocol (GeminiModel, Voice, Content, Part, Role, …) ──
    pub use gemini_genai_rs::prelude::*;
}
