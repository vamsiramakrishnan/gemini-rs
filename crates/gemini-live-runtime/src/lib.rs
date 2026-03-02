//! # gemini-live-runtime
//!
//! Agent runtime for the Gemini Multimodal Live API.
//! Provides the Agent trait, AgentSession (intercepting wrapper around SessionHandle),
//! tool dispatch, streaming tools, agent transfer, and middleware.

pub mod agent;
pub mod agent_session;
pub mod agent_tool;
pub mod agents;
pub mod context;
pub mod error;
pub mod llm_agent;
pub mod middleware;
pub mod router;
pub mod runner;
pub mod state;
pub mod telemetry;
pub mod tool;

// Ergonomic re-exports
pub use agent::Agent;
pub use agent_tool::AgentTool;
pub use agents::{LoopAgent, ParallelAgent, SequentialAgent};
pub use context::{AgentEvent, InvocationContext};
pub use error::{AgentError, ToolError};
pub use llm_agent::{LlmAgent, LlmAgentBuilder};
pub use middleware::{Middleware, MiddlewareChain};
pub use router::AgentRegistry;
pub use runner::Runner;
pub use state::State;
pub use tool::{SimpleTool, ToolDispatcher, ToolFunction, TypedTool};

// Wire re-export
pub use gemini_live_wire;
