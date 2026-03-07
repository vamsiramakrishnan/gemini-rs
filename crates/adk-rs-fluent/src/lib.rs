#![warn(missing_docs)]
//! # adk-rs-fluent
//!
//! Fluent developer experience layer for the Gemini Live agent stack.
//! This is the highest-level crate in the workspace, providing a builder API,
//! operator algebra, and composition modules that sit on top of
//! [`rs_adk`] (agent runtime) and [`rs_genai`] (wire protocol).
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
//! use adk_rs_fluent::prelude::*;
//!
//! let agent = AgentBuilder::new("my-agent")
//!     .model(GeminiModel::Gemini2_0Flash)
//!     .instruction("You are a helpful assistant.")
//!     .build();
//! ```
//!
//! ## Relationship to Other Crates
//!
//! - **`rs-genai`** (L0): Wire protocol, transport, types — re-exported via [`rs_genai`]
//! - **`rs-adk`** (L1): Agent runtime, tools, sessions — re-exported via [`rs_adk`]
//! - **`adk-rs-fluent`** (L2): This crate — ergonomic builder API and composition

pub mod builder;
pub mod compose;
pub mod live;
pub mod live_builders;
pub mod operators;
pub mod patterns;
pub mod testing;

pub use rs_adk;
pub use rs_genai;

/// Clone multiple bindings for use in `move` closures, reducing Arc/clone boilerplate.
///
/// # Example
///
/// ```rust,ignore
/// use adk_rs_fluent::let_clone;
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
    pub use crate::builder::*;
    pub use crate::compose::{Ctx, A, C, M, P, S, T};
    pub use crate::live::Live;
    pub use crate::live_builders::*;
    pub use crate::operators::*;
    pub use crate::patterns::*;
    pub use crate::testing::*;
    pub use rs_adk::agent::*;
    pub use rs_adk::agent_session::*;
    pub use rs_adk::live::{
        CallbackMode, DefaultResultFormatter, EventCallbacks, ExtractionTrigger, FsPersistence,
        LiveHandle, LiveSessionBuilder, LlmExtractor, MemoryPersistence, NeedsFulfillment,
        RepairAction, RepairConfig, ResultFormatter, SessionPersistence, SessionSnapshot,
        SoftTurnDetector, SteeringMode, ToolExecutionMode, TranscriptBuffer, TranscriptTurn,
        TurnExtractor,
    };
    pub use rs_adk::llm::BaseLlm;
    pub use rs_adk::state::State;
    pub use rs_adk::text::{
        DispatchTextAgent, FallbackTextAgent, FnTextAgent, JoinTextAgent, LlmTextAgent,
        LoopTextAgent, MapOverTextAgent, ParallelTextAgent, RaceTextAgent, RouteRule,
        RouteTextAgent, SequentialTextAgent, TapTextAgent, TaskRegistry, TextAgent,
        TimeoutTextAgent,
    };
    // New ADK-JS parity types
    pub use rs_adk::confirmation::ToolConfirmation;
    pub use rs_adk::context::{CallbackContext, ToolContext};
    pub use rs_adk::credentials::{AuthCredential, CredentialService, InMemoryCredentialService};
    pub use rs_adk::instruction::inject_session_state;
    pub use rs_adk::llm::LlmRegistry;
    pub use rs_adk::run_config::{RunConfig, StreamingMode};
    pub use rs_adk::text_runner::InMemoryRunner;
    pub use rs_adk::toolset::{StaticToolset, Toolset};
    pub use rs_genai::prelude::*;
}
