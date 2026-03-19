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
pub mod live;
pub mod live_builders;
pub mod operators;
pub mod patterns;
pub mod testing;

pub use gemini_adk_rs;
pub use gemini_genai_rs;

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
    pub use crate::a2a::{A2AServer, AgentRegistry, RemoteAgent, SkillDeclaration};
    pub use crate::builder::*;
    pub use crate::compose::{Ctx, A, C, E, G, M, P, S, T};
    pub use crate::live::Live;
    pub use crate::live_builders::*;
    pub use crate::operators::*;
    pub use crate::patterns::*;
    pub use crate::testing::*;
    // Note: gemini_adk_rs::agent::Agent trait is NOT re-exported here because
    // it conflicts with the L2 Agent type alias (= AgentBuilder).
    // Use gemini_adk_rs::agent::Agent directly if you need the L1 trait.
    pub use gemini_adk_rs::agent_session::*;
    pub use gemini_adk_rs::error::{AgentError, ToolError};
    pub use gemini_adk_rs::live::{
        CallbackMode, ContextDelivery, DefaultResultFormatter, DeferredWriter, EventCallbacks,
        ExtractionTrigger, FsPersistence, LiveEvent, LiveHandle, LiveSessionBuilder, LlmExtractor,
        MemoryPersistence, NeedsFulfillment, PendingContext, RepairAction, RepairConfig,
        ResultFormatter, SessionPersistence, SessionSnapshot, SoftTurnDetector, SteeringMode,
        ToolExecutionMode, TranscriptBuffer, TranscriptTurn, TurnExtractor,
    };
    pub use gemini_adk_rs::llm::{BaseLlm, GeminiLlm, GeminiLlmParams, LlmRequest, LlmResponse};
    pub use gemini_adk_rs::state::{State, StateKey};
    pub use gemini_adk_rs::text::{
        DispatchTextAgent, FallbackTextAgent, FnTextAgent, JoinTextAgent, LlmTextAgent,
        LoopTextAgent, MapOverTextAgent, ParallelTextAgent, RaceTextAgent, RouteRule,
        RouteTextAgent, SequentialTextAgent, TapTextAgent, TaskRegistry, TextAgent,
        TimeoutTextAgent,
    };
    // New ADK-JS parity types
    pub use gemini_adk_rs::confirmation::ToolConfirmation;
    pub use gemini_adk_rs::context::{CallbackContext, ToolContext};
    pub use gemini_adk_rs::credentials::{
        AuthCredential, CredentialService, InMemoryCredentialService,
    };
    pub use gemini_adk_rs::instruction::inject_session_state;
    pub use gemini_adk_rs::llm::LlmRegistry;
    pub use gemini_adk_rs::run_config::{RunConfig, StreamingMode};
    pub use gemini_adk_rs::text_runner::InMemoryRunner;
    pub use gemini_adk_rs::toolset::{StaticToolset, Toolset};
    pub use gemini_genai_rs::prelude::*;
}
