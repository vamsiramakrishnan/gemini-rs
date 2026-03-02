//! # rs-adk
//!
//! Full Rust equivalent of Google's `@google/adk` framework.
//! Agents, tools, sessions, events, middleware, and runtime.

pub mod agent;
pub mod agent_session;
pub mod agent_tool;
pub mod agents;
pub mod artifacts;
pub mod callback;
pub mod context;
pub mod error;
pub mod events;
pub mod live;
pub mod llm;
pub mod llm_agent;
pub mod memory;
pub mod middleware;
pub mod plugin;
pub mod processors;
pub mod router;
pub mod run_config;
pub mod runner;
pub mod session;
pub mod state;
pub mod telemetry;
pub mod text;
pub mod tool;
pub mod toolset;
pub mod credentials;
pub mod instruction;
pub mod confirmation;
pub mod text_runner;
pub mod utils;

#[cfg(test)]
pub(crate) mod test_helpers;

// Ergonomic re-exports
pub use agent::Agent;
pub use agent_tool::AgentTool;
pub use agents::{LoopAgent, ParallelAgent, SequentialAgent};
pub use context::{AgentEvent, CallbackContext, InvocationContext, ToolContext};
pub use error::{AgentError, ToolError};
pub use events::{Event, EventActions, EventType, StructuredEvent};
pub use llm_agent::{LlmAgent, LlmAgentBuilder};
pub use middleware::{Middleware, MiddlewareChain};
pub use router::AgentRegistry;
pub use runner::Runner;
pub use session::{InMemorySessionService, Session, SessionId, SessionService};
pub use state::State;
pub use tool::{SimpleTool, ToolDispatcher, ToolFunction, TypedTool};
pub use callback::{AfterToolCallback, BeforeToolCallback, BeforeToolResult, ToolCallResult};
pub use plugin::{Plugin, PluginManager, PluginResult};
pub use memory::{InMemoryMemoryService, MemoryEntry, MemoryService};
pub use artifacts::{Artifact, ArtifactService, InMemoryArtifactService};
pub use credentials::{AuthCredential, CredentialError, CredentialService, InMemoryCredentialService};
pub use toolset::{StaticToolset, Toolset};
pub use instruction::inject_session_state;
pub use confirmation::ToolConfirmation;
pub use text_runner::InMemoryRunner;
pub use state::PrefixedState;
pub use live::{EventCallbacks, LiveHandle, LiveSessionBuilder, LlmExtractor, TranscriptBuffer, TranscriptTurn, TurnExtractor};
pub use llm::{BaseLlm, LlmRegistry, LlmRequest, LlmResponse};
pub use run_config::{RunConfig, StreamingMode};
pub use text::{
    DispatchTextAgent, FallbackTextAgent, FnTextAgent, JoinTextAgent, LlmTextAgent,
    LoopTextAgent, MapOverTextAgent, ParallelTextAgent, RaceTextAgent, RouteRule,
    RouteTextAgent, SequentialTextAgent, TapTextAgent, TaskRegistry, TextAgent,
    TimeoutTextAgent,
};
pub use processors::{
    ContentFilter, InstructionInserter, RequestProcessor, RequestProcessorChain,
    ResponseProcessor, ResponseProcessorChain,
};

// Wire re-export
pub use rs_genai;
