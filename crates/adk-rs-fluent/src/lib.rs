//! # adk-rs-fluent
//!
//! Fluent DX for Gemini — builder API, operator algebra, composition modules.
//! The highest-level crate in the rs-genai workspace.

pub mod builder;
pub mod compose;
pub mod live;
pub mod live_builders;
pub mod operators;
pub mod patterns;
pub mod testing;

pub use rs_adk;
pub use rs_genai;

pub mod prelude {
    pub use crate::builder::*;
    pub use crate::compose::{A, C, M, P, S, T};
    pub use crate::live::Live;
    pub use crate::live_builders::*;
    pub use crate::operators::*;
    pub use crate::patterns::*;
    pub use crate::testing::*;
    pub use rs_adk::agent::*;
    pub use rs_adk::agent_session::*;
    pub use rs_adk::live::{
        EventCallbacks, LiveHandle, LiveSessionBuilder, LlmExtractor, TranscriptBuffer,
        TranscriptTurn, TurnExtractor,
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
