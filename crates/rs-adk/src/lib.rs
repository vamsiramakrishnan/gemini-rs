//! # rs-adk
//!
//! Full Rust equivalent of Google's `@google/adk` framework.
//! Agents, tools, sessions, events, middleware, and runtime.

pub mod agent;
pub mod agent_session;
pub mod agent_tool;
pub mod agents;
pub mod context;
pub mod error;
pub mod events;
pub mod llm_agent;
pub mod middleware;
pub mod router;
pub mod runner;
pub mod session;
pub mod state;
pub mod telemetry;
pub mod tool;

// Ergonomic re-exports
pub use agent::Agent;
pub use agent_tool::AgentTool;
pub use agents::{LoopAgent, ParallelAgent, SequentialAgent};
pub use context::{AgentEvent, InvocationContext};
pub use error::{AgentError, ToolError};
pub use events::{Event, EventActions};
pub use llm_agent::{LlmAgent, LlmAgentBuilder};
pub use middleware::{Middleware, MiddlewareChain};
pub use router::AgentRegistry;
pub use runner::Runner;
pub use session::{InMemorySessionService, Session, SessionId, SessionService};
pub use state::State;
pub use tool::{SimpleTool, ToolDispatcher, ToolFunction, TypedTool};

// Wire re-export
pub use rs_genai;
