#![warn(missing_docs)]
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
pub mod code_executors;
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
pub mod text_agent_tool;
pub mod tool;
pub mod tools;
pub mod toolset;
pub mod auth;
pub mod credentials;
pub mod instruction;
pub mod confirmation;
pub mod text_runner;
pub mod utils;
pub mod a2a;

#[cfg(test)]
pub(crate) mod test_helpers;

// Ergonomic re-exports
pub use agent::Agent;
pub use agent_tool::AgentTool;
pub use text_agent_tool::TextAgentTool;
pub use agents::{LoopAgent, ParallelAgent, SequentialAgent};
pub use context::{AgentEvent, CallbackContext, InvocationContext, ToolContext};
pub use error::{AgentError, ToolError};
pub use events::{Event, EventActions, EventType, StructuredEvent};
pub use llm_agent::{LlmAgent, LlmAgentBuilder};
pub use middleware::{Middleware, MiddlewareChain};
pub use router::AgentRegistry;
pub use runner::Runner;
pub use session::{InMemorySessionService, Session, SessionId, SessionService, db_schema};
#[cfg(feature = "database-sessions")]
pub use session::DatabaseSessionService;
pub use state::State;
pub use tool::{SimpleTool, ToolDispatcher, ToolFunction, TypedTool};
pub use callback::{AfterToolCallback, BeforeToolCallback, BeforeToolResult, ToolCallResult};
pub use plugin::{Plugin, PluginManager, PluginResult};
pub use memory::{InMemoryMemoryService, MemoryEntry, MemoryService};
pub use artifacts::{Artifact, ArtifactService, FileArtifactService, InMemoryArtifactService};
#[cfg(feature = "gcs-artifacts")]
pub use artifacts::GcsArtifactService;
pub use credentials::{AuthCredential, CredentialError, CredentialService, InMemoryCredentialService};
pub use tools::GoogleSearchTool;
pub use tools::long_running::LongRunningFunctionTool;
pub use tools::mcp::{McpConnectionParams, McpTool, McpToolset};
pub use toolset::{StaticToolset, Toolset};
pub use instruction::inject_session_state;
pub use confirmation::ToolConfirmation;
pub use text_runner::InMemoryRunner;
pub use state::PrefixedState;
pub use live::{CallbackMode, EventCallbacks, LiveHandle, LiveSessionBuilder, LlmExtractor, ToolCallSummary, TranscriptBuffer, TranscriptTurn, TurnExtractor};
pub use llm::{BaseLlm, GeminiLlm, GeminiLlmParams, LlmRegistry, LlmRequest, LlmResponse};
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
pub use utils::model_name::{extract_model_name, is_gemini_model, is_gemini2_or_above};
pub use utils::variant::{GoogleLlmVariant, get_google_llm_variant};
pub use auth::{AuthConfig, AuthHandler, AuthScheme, OAuthGrantType, CredentialExchanger, CredentialExchangerRegistry};
pub use code_executors::{CodeExecutor, BuiltInCodeExecutor, CodeExecutionInput, CodeExecutionResult, CodeFile};
pub use a2a::{A2aMessage, A2aPart, to_a2a_message, to_adk_event, to_a2a_parts, to_genai_parts};

// Wire re-export
pub use rs_genai;
