//! # gemini-live-runtime
//!
//! Agent runtime for the Gemini Multimodal Live API.
//! Provides the Agent trait, AgentSession (intercepting wrapper around SessionHandle),
//! tool dispatch, streaming tools, agent transfer, and middleware.

pub mod agent;
pub mod agent_session;
pub mod context;
pub mod error;
pub mod middleware;
pub mod router;
pub mod state;
pub mod tool;

// Re-export wire types that runtime users need
pub use gemini_live_wire;
