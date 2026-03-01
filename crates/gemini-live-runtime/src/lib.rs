//! # gemini-live-runtime
//!
//! Agent runtime for the Gemini Multimodal Live API.
//! Provides the Agent trait, AgentSession (intercepting wrapper around SessionHandle),
//! tool dispatch, streaming tools, agent transfer, and middleware.

pub mod agent;
pub mod agent_session;
pub mod agent_tool;
pub mod context;
pub mod error;
pub mod llm_agent;
pub mod middleware;
pub mod router;
pub mod runner;
pub mod state;
pub mod telemetry;
pub mod tool;

// Re-export key types for convenience
pub use agent_tool::AgentTool;
pub use llm_agent::{LlmAgent, LlmAgentBuilder};
pub use runner::Runner;
pub use tool::TypedTool;

// Re-export wire types that runtime users need
pub use gemini_live_wire;
