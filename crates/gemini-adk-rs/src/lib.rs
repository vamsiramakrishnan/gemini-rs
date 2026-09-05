#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(unreachable_pub)]
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]
#![warn(missing_docs)]
//! # gemini-adk-rs
//!
//! The agent **runtime** for Gemini Live — layer 1 of a three-crate stack.
//! Below it, [`gemini_genai_rs`] speaks the wire protocol. Above it,
//! [`gemini-adk-fluent-rs`](https://docs.rs/gemini-adk-fluent-rs) wraps this
//! crate in the builder API most applications should start from. Come here
//! when you are building a custom processor, a tool backend, a persistence
//! layer, or an evaluation harness — or when you want to see what the fluent
//! builder actually assembles.
//!
//! ## What lives here
//!
//! | Concern | Start at |
//! |---------|----------|
//! | A Live session and its three-lane processor | [`live::LiveSessionBuilder`], [`live::LiveHandle`] |
//! | Tools the model can call | [`tool::ToolFunction`], [`tool::SimpleTool`], [`tool::TypedTool`], [`tool::ToolDispatcher`] |
//! | Concurrent typed state with prefix scopes | [`state::State`], [`state::StateKey`] |
//! | Text agents and combinators | [`text::LlmTextAgent`] and the `*TextAgent` family in [`text`] |
//! | Declarative conversation phases | [`live::PhaseMachine`], [`live::Phase`] |
//! | Governed flows enforced while the model speaks | [`flow::Flow`], [`flow::FlowMonitor`] |
//! | Turn extraction, watchers, temporal patterns | [`live::TurnExtractor`], [`live::watcher`], [`live::temporal`] |
//! | Session persistence and telemetry | [`live::persistence`], [`live::SessionTelemetry`] |
//!
//! Anything behind a Cargo feature is marked on its page — `vertex-ai-sessions`,
//! `database-sessions`, `templates`, `otel`, and the rest — and the full list is
//! in this crate's `Cargo.toml`.
//!
//! ## The shape of the runtime
//!
//! Every Live session runs one **router** and two lanes. The *fast lane* is
//! synchronous and handles audio, text deltas and transcripts in under a
//! millisecond per event; the *control lane* is async and runs tool calls,
//! phase transitions, extractors and watchers. A third, independent
//! *telemetry lane* observes both. The callback you register decides which
//! lane it runs on, and the rule is written once at the top of
//! [`live::callbacks`].
//!
//! ## A first taste
//!
//! [`state::State`] is the piece every layer shares — tools write it,
//! guards read it, extractors fill it — so it is the smallest thing worth
//! showing on its own:
//!
//! ```
//! use gemini_adk_rs::state::{State, StateKey};
//!
//! const TURNS: StateKey<u32> = StateKey::new("session:turn_count");
//!
//! let state = State::new();
//! state.set("user:name", "Alice");
//! state.modify("session:turn_count", 0u32, |n| n + 1);
//!
//! assert_eq!(state.get::<String>("user:name").as_deref(), Some("Alice"));
//! assert_eq!(state.get_key(&TURNS), Some(1));
//! ```
//!
//! For a running session, see the `examples/` directory of the repository or
//! the `Live::builder()` walkthrough in the fluent crate's documentation.

// The proc macros expand to `::gemini_adk_rs::…`; this makes that path valid
// inside the crate itself (its own tests and doctests included).
extern crate self as gemini_adk_rs;

pub mod a2a;
pub mod agent;
pub mod agent_config;
pub mod agent_session;
pub mod agent_tool;
pub mod agents;
pub mod artifacts;
pub mod auth;
pub mod code_executors;
pub mod confirmation;
pub mod context;
pub mod credentials;
pub mod error;
pub mod evaluation;
pub mod events;
pub mod expr;
pub mod extract;
pub mod flow;
pub mod frame;
pub mod instruction;
pub mod live;
pub mod llm;
pub mod llm_agent;
pub mod memory;
pub mod middleware;
pub mod optimization;
pub mod orchestration;
pub mod planners;
pub mod plugin;
pub mod primitives;
pub mod processors;
pub mod router;
pub mod run_config;
pub mod runner;
pub mod session;
pub mod skills;
pub mod state;
pub mod telemetry;
pub mod text;
pub mod text_agent_tool;
pub mod text_runner;
pub mod tool;
pub mod tools;
pub mod toolset;
pub mod utils;
pub mod workflow;

#[cfg(test)]
pub(crate) mod test_helpers;

// ── Shared closure shapes ─────────────────────────────────────────────────
// Named once here so every module (phases, workflows, watchers, temporal
// patterns, callbacks, resolvers) spells the same shape the same way.

/// A boxed, sendable, `'static` future — the return type of every async
/// callback and hook in the runtime.
pub type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;

/// A synchronous predicate over shared [`State`]: `true` admits (a phase
/// transition, a workflow node, a guard).
pub type StatePredicate = std::sync::Arc<dyn Fn(&State) -> bool + Send + Sync>;

/// An async source of a JSON value — the seam for a tool call, an HTTP fetch,
/// an MCP request, or a workflow function node. `In` is what the source is
/// bound from: the whole [`State`] by default, or a pre-bound args
/// [`Value`](serde_json::Value) for extraction-kit field resolvers.
///
/// The `Err(String)` payload is a human-readable reason; it lands in
/// `{name}:error` state keys and in extraction diagnostics.
pub type AsyncSourceFn<In = State> = std::sync::Arc<
    dyn Fn(
            In,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>,
        > + Send
        + Sync,
>;

// Ergonomic re-exports — existing
pub use a2a::{A2aMessage, A2aPart, to_a2a_message, to_a2a_parts, to_adk_event, to_genai_parts};
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
pub use code_executors::{
    BuiltInCodeExecutor, CodeExecutionInput, CodeExecutionResult, CodeExecutor, CodeFile,
};
pub use confirmation::{
    ConfirmationProvider, ConfirmationRequest, StaticConfirmation, ToolConfirmation,
};
pub use context::{AgentEvent, CallbackContext, InvocationContext, ToolContext};
pub use credentials::{
    AuthCredential, CredentialError, CredentialService, InMemoryCredentialService,
};
pub use error::{AgentError, AgentResult, ConfigError, ToolError};
pub use events::{Event, EventActions, EventType, StructuredEvent};
pub use extract::{Extract, Recognizer, RecordExtractor};
pub use flow::{
    CompiledFlow, Enforcement, Flow, FlowError, FlowErrors, FlowExplanation, FlowMonitor, Guard,
    SharedFlowMonitor, StepAction, ToolSurface, Verdict, Violation, on_enter,
};
pub use frame::{ConfirmPolicy, Frame, FrameSpec, SlotRecognizer, SlotSpec, SlotValidator};
/// Re-exports the `#[tool]`/`#[derive(..)]` macros route their generated code
/// through, so downstream crates don't need the upstream crate names
/// (`serde`/`schemars`/`async_trait`/`serde_json`) in scope or under those exact
/// names. Not public API.
#[doc(hidden)]
pub mod __macros {
    pub use async_trait;
    pub use schemars;
    pub use serde;
    pub use serde_json;
}

/// The `#[derive(Extract)]` macro — builds an [`extract::Extract`] record from a
/// struct's `#[recognize(..)]` fields. Shares the name `Extract` with the
/// struct (macro vs type namespace), so both can be imported together.
///
/// See the [`gemini_adk_macros_rs::Extract`](macro@gemini_adk_macros_rs::Extract)
/// documentation for details.
pub use gemini_adk_macros_rs::Extract;
/// Derive macro that generates a [`frame::Frame`] impl from a struct's
/// `#[slot(..)]` fields. Shares the name `Frame` with the trait (macro vs type
/// namespace), so both can be imported together.
pub use gemini_adk_macros_rs::Frame;
/// The `#[tool]` attribute macro — turns an `async fn` into a registrable Gemini tool.
///
/// See the [`gemini_adk_macros_rs::tool`] documentation for details.
pub use gemini_adk_macros_rs::tool;
pub use instruction::inject_session_state;
pub use live::{
    EventCallbacks, ExecutionMode, LiveHandle, LiveSessionBuilder, LlmExtractor, PersistenceError,
    SessionHook, ToolCallSummary, TranscriptBuffer, TranscriptTurn, TurnExtractor,
};
pub use llm::{BaseLlm, GeminiLlm, GeminiLlmParams, LlmRegistry, LlmRequest, LlmResponse};
pub use llm_agent::{LlmAgent, LlmAgentBuilder};
pub use memory::{InMemoryMemoryService, MemoryEntry, MemoryService};
pub use middleware::{Middleware, MiddlewareChain};
pub use orchestration::{AgentMode, Resolver, call_agent, provenance};
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
pub use session::{InMemorySessionService, Session, SessionId, SessionService, db_schema};
pub use state::{FileJournalSink, JournalSink, MemoryJournalSink};
pub use state::{PrefixedState, ReadOnlyPrefixedState, StateError};
pub use state::{SlotEvidence, State, StateMutation, StateMutationOrigin};
pub use text::{
    DispatchTextAgent, FallbackTextAgent, FnTextAgent, JoinTextAgent, LlmTextAgent, LoopTextAgent,
    MapOverTextAgent, ParallelTextAgent, RaceTextAgent, RouteRule, RouteTextAgent,
    SequentialTextAgent, TapTextAgent, TaskRegistry, TextAgent, TimeoutTextAgent,
};
pub use text_agent_tool::TextAgentTool;
pub use text_runner::{RunEvent, TextRunner};
pub use tool::{SimpleTool, ToolDispatcher, ToolFunction, ToolPolicy, TypedTool};
pub use tools::GoogleSearchTool;
pub use tools::long_running::LongRunningFunctionTool;
pub use tools::mcp::{McpConnectionParams, McpTool, McpToolset};
pub use toolset::{StaticToolset, Toolset};
pub use utils::model_name::{extract_model_name, is_gemini_model, is_gemini2_or_above};
pub use utils::variant::{GoogleLlmVariant, get_google_llm_variant};

// New re-exports — A2A
pub use a2a::{AgentCard, AgentSkill, RemoteA2aAgent, RemoteA2aAgentConfig};

// New re-exports — Evaluation
pub use evaluation::{
    EvalCase, EvalMetric, EvalResult, EvalSet, Evaluator, Invocation, LlmAsJudge,
    PerInvocationResult, ResponseEvaluator, TrajectoryEvaluator,
};

// New re-exports — Planners
pub use planners::{BuiltInPlanner, PlanReActPlanner, Planner, PlannerError};

// New re-exports — Optimization
pub use optimization::{
    AgentOptimizer, EvalSample, OptimizerError, OptimizerResult, Sampler, SimplePromptOptimizer,
    SimplePromptOptimizerConfig,
};

// New re-exports — Code Executors
pub use code_executors::{
    ContainerCodeExecutor, ContainerCodeExecutorConfig, UnsafeLocalCodeExecutor,
};
#[cfg(feature = "vertex-ai-code-executor")]
pub use code_executors::{VertexAiCodeExecutor, VertexAiCodeExecutorConfig};

// New re-exports — Plugins
pub use plugin::{ContextFilterPlugin, GlobalInstructionPlugin, ReflectRetryToolPlugin};

// New re-exports — Memory
pub use memory::{VertexAiMemoryBankConfig, VertexAiMemoryBankService};
#[cfg(feature = "vertex-ai-rag")]
pub use memory::{VertexAiRagMemoryConfig, VertexAiRagMemoryService};

// New re-exports — Sessions
#[cfg(feature = "postgres-sessions")]
pub use session::{PostgresSessionConfig, PostgresSessionService};
pub use session::{SqliteSessionConfig, SqliteSessionService};
#[cfg(feature = "vertex-ai-sessions")]
pub use session::{VertexAiSessionConfig, VertexAiSessionService};

// New re-exports — Tools
pub use tools::retrieval::{BaseRetrievalTool, FilesRetrievalTool, RetrievalResult};
#[cfg(feature = "vertex-ai-rag")]
pub use tools::retrieval::{VertexAiRagConfig, VertexAiRagRetrievalTool};
pub use tools::{
    BashToolPolicy, DiscoveryEngineSearchTool, Example, ExampleTool, ExecuteBashTool, ExitLoopTool,
    GetUserChoiceTool, LoadMemoryTool, PreloadMemoryTool, TransferToAgentTool, UrlContextTool,
    VertexAiSearchConfig, VertexAiSearchTool,
};

// New re-exports — Agent Config
pub use agent_config::{
    AgentConfig, AgentConfigError, ToolConfig as AgentToolConfig, discover_agent_configs,
};

// Wire re-export
pub use gemini_genai_rs;
