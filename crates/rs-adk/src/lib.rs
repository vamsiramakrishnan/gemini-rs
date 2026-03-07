#![warn(missing_docs)]
//! # rs-adk
//!
//! Full Rust equivalent of Google's `@google/adk` framework.
//! Agents, tools, sessions, events, middleware, and runtime.

pub mod a2a;
pub mod agent;
pub mod agent_session;
pub mod agent_tool;
pub mod agents;
pub mod artifacts;
pub mod auth;
pub mod callback;
pub mod code_executors;
pub mod confirmation;
pub mod context;
pub mod credentials;
pub mod error;
pub mod events;
pub mod instruction;
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
pub mod text_runner;
pub mod tool;
pub mod tools;
pub mod toolset;
pub mod utils;

#[cfg(test)]
pub(crate) mod test_helpers;

// Ergonomic re-exports
pub use a2a::{to_a2a_message, to_a2a_parts, to_adk_event, to_genai_parts, A2aMessage, A2aPart};
pub use agent::Agent;
pub use agent_tool::AgentTool;
pub use agents::{LoopAgent, ParallelAgent, SequentialAgent};
#[cfg(feature = "gcs-artifacts")]
pub use artifacts::GcsArtifactService;
pub use artifacts::{Artifact, ArtifactService, FileArtifactService, InMemoryArtifactService};
pub use auth::{
    AuthConfig, AuthHandler, AuthScheme, CredentialExchanger, CredentialExchangerRegistry,
    OAuthGrantType,
};
pub use callback::{AfterToolCallback, BeforeToolCallback, BeforeToolResult, ToolCallResult};
pub use code_executors::{
    BuiltInCodeExecutor, CodeExecutionInput, CodeExecutionResult, CodeExecutor, CodeFile,
};
pub use confirmation::ToolConfirmation;
pub use context::{AgentEvent, CallbackContext, InvocationContext, ToolContext};
pub use credentials::{
    AuthCredential, CredentialError, CredentialService, InMemoryCredentialService,
};
pub use error::{AgentError, ToolError};
pub use events::{Event, EventActions, EventType, StructuredEvent};
pub use instruction::inject_session_state;
pub use live::{
    CallbackMode, EventCallbacks, LiveHandle, LiveSessionBuilder, LlmExtractor, ToolCallSummary,
    TranscriptBuffer, TranscriptTurn, TurnExtractor,
};
pub use llm::{BaseLlm, GeminiLlm, GeminiLlmParams, LlmRegistry, LlmRequest, LlmResponse};
pub use llm_agent::{LlmAgent, LlmAgentBuilder};
pub use memory::{InMemoryMemoryService, MemoryEntry, MemoryService};
pub use middleware::{Middleware, MiddlewareChain};
pub use plugin::{Plugin, PluginManager, PluginResult};
pub use processors::{
    ContentFilter, InstructionInserter, RequestProcessor, RequestProcessorChain, ResponseProcessor,
    ResponseProcessorChain,
};
pub use router::AgentRegistry;
pub use run_config::{RunConfig, StreamingMode};
pub use runner::Runner;
#[cfg(feature = "database-sessions")]
pub use session::DatabaseSessionService;
pub use session::{db_schema, InMemorySessionService, Session, SessionId, SessionService};
pub use state::PrefixedState;
pub use state::State;
pub use text::{
    DispatchTextAgent, FallbackTextAgent, FnTextAgent, JoinTextAgent, LlmTextAgent, LoopTextAgent,
    MapOverTextAgent, ParallelTextAgent, RaceTextAgent, RouteRule, RouteTextAgent,
    SequentialTextAgent, TapTextAgent, TaskRegistry, TextAgent, TimeoutTextAgent,
};
pub use text_agent_tool::TextAgentTool;
pub use text_runner::InMemoryRunner;
pub use tool::{SimpleTool, ToolDispatcher, ToolFunction, TypedTool};
pub use tools::long_running::LongRunningFunctionTool;
pub use tools::mcp::{McpConnectionParams, McpTool, McpToolset};
pub use tools::GoogleSearchTool;
pub use toolset::{StaticToolset, Toolset};
pub use utils::model_name::{extract_model_name, is_gemini2_or_above, is_gemini_model};
pub use utils::variant::{get_google_llm_variant, GoogleLlmVariant};

// Wire re-export
pub use rs_genai;
