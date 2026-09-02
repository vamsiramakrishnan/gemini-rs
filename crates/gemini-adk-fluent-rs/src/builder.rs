//! AgentBuilder — copy-on-write immutable builder for fluent agent construction.
//!
//! Every mutation returns a new builder (original unchanged), so builders
//! are safely shareable as templates.

use std::sync::Arc;

use gemini_adk_rs::error::ConfigError;
use gemini_adk_rs::llm::BaseLlm;
use gemini_adk_rs::middleware::Middleware;
use gemini_adk_rs::text::{LlmTextAgent, TextAgent};
use gemini_adk_rs::tool::{ToolDispatcher, ToolFunction, ToolKind};
use gemini_genai_rs::prelude::{Modality, ModelId, Tool, Voice};

use crate::compose::context::ContextComposite;
use crate::compose::guards::GuardComposite;
use crate::compose::middleware::MiddlewareComposite;
use crate::compose::tools::ToolComposite;

type LlmProviderFn = Arc<dyn Fn(&gemini_adk_rs::State) -> Arc<dyn BaseLlm> + Send + Sync>;

/// Inner state of an AgentBuilder — shared via Arc for copy-on-write.
#[derive(Clone)]
struct AgentBuilderInner {
    name: String,
    model: Option<ModelId>,
    instruction: Option<String>,
    instruction_provider: Option<Arc<dyn gemini_adk_rs::instruction::InstructionProvider>>,
    llm_provider: Option<LlmProviderFn>,
    voice: Option<Voice>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<u32>,
    max_output_tokens: Option<u32>,
    stop_sequences: Vec<String>,
    response_modalities: Option<Vec<Modality>>,
    thinking_budget: Option<u32>,
    tools: Vec<ToolEntry>,
    built_in_tools: Vec<Tool>,
    writes: Vec<String>,
    reads: Vec<String>,
    sub_agents: Vec<AgentBuilder>,
    isolate: bool,
    stay: bool,
    description: Option<String>,
    output_schema: Option<serde_json::Value>,
    output_key: Option<String>,
    transfer_to_agent: Option<String>,
    /// Middleware layers to install on the compiled `LlmTextAgent`.
    middleware_layers: Vec<Arc<dyn Middleware>>,
    /// Configuration problems found by setters (which cannot fail), reported
    /// as one [`ConfigError`] by [`AgentBuilder::build`].
    config_errors: Vec<String>,
}

/// An entry in the builder's tool list — either a runtime ToolKind or a declaration.
#[derive(Clone)]
pub enum ToolEntry {
    /// A runtime tool with a handler function.
    Runtime(Arc<dyn ToolEntryTrait>),
    /// A wire-level tool declaration (e.g., built-in tools like Google Search).
    Declaration(Tool),
}

/// Trait for tool entries that can provide a name (for dedup/inspection).
pub trait ToolEntryTrait: Send + Sync + 'static {
    /// The tool's registered name.
    fn name(&self) -> &str;
    /// Convert this entry into the runtime `ToolKind` variant for dispatch.
    fn to_tool_kind(&self) -> ToolKind;
}

/// Copy-on-write immutable builder for agent construction.
///
/// Every setter returns a new `AgentBuilder`, leaving the original unchanged.
/// This makes builders safe to share as templates.
///
/// # Basic Usage
///
/// ```rust
/// use gemini_adk_fluent_rs::builder::AgentBuilder;
/// use gemini_genai_rs::prelude::ModelId;
///
/// let agent = AgentBuilder::new("analyst")
///     .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
///     .instruction("Analyze the given topic")
///     .temperature(0.3);
///
/// assert_eq!(agent.name(), "analyst");
/// assert_eq!(agent.get_temperature(), Some(0.3));
/// ```
///
/// # Copy-on-Write Pattern
///
/// Cloning a builder and modifying the clone leaves the original unchanged.
/// This is useful for creating template builders with shared defaults.
///
/// ```rust
/// use gemini_adk_fluent_rs::builder::AgentBuilder;
///
/// let base = AgentBuilder::new("researcher")
///     .instruction("You are a research assistant.")
///     .temperature(0.5);
///
/// let creative = base.clone().temperature(0.9);
/// let precise  = base.clone().temperature(0.1);
///
/// // Original unchanged
/// assert_eq!(base.get_temperature(), Some(0.5));
/// assert_eq!(creative.get_temperature(), Some(0.9));
/// assert_eq!(precise.get_temperature(), Some(0.1));
/// ```
///
/// # Sampling Parameters
///
/// ```rust
/// use gemini_adk_fluent_rs::builder::AgentBuilder;
///
/// let agent = AgentBuilder::new("sampler")
///     .temperature(0.7)
///     .top_p(0.95)
///     .top_k(40)
///     .max_output_tokens(4096);
///
/// assert_eq!(agent.get_top_p(), Some(0.95));
/// assert_eq!(agent.get_top_k(), Some(40));
/// assert_eq!(agent.get_max_output_tokens(), Some(4096));
/// ```
///
/// # Built-in Tools
///
/// ```rust
/// use gemini_adk_fluent_rs::builder::AgentBuilder;
///
/// let agent = AgentBuilder::new("searcher")
///     .google_search()
///     .code_execution()
///     .url_context();
///
/// assert_eq!(agent.tool_count(), 3);
/// ```
///
/// # Thinking Budget
///
/// ```rust
/// use gemini_adk_fluent_rs::builder::AgentBuilder;
///
/// let agent = AgentBuilder::new("thinker")
///     .thinking(2048);
///
/// assert_eq!(agent.get_thinking_budget(), Some(2048));
/// ```
#[derive(Clone)]
pub struct AgentBuilder {
    inner: Arc<AgentBuilderInner>,
}

impl AgentBuilder {
    /// Create a new builder with the given agent name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(AgentBuilderInner {
                name: name.into(),
                model: None,
                instruction: None,
                instruction_provider: None,
                llm_provider: None,
                voice: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                stop_sequences: Vec::new(),
                response_modalities: None,
                thinking_budget: None,
                tools: Vec::new(),
                built_in_tools: Vec::new(),
                writes: Vec::new(),
                reads: Vec::new(),
                sub_agents: Vec::new(),
                isolate: false,
                stay: false,
                description: None,
                output_schema: None,
                output_key: None,
                transfer_to_agent: None,
                middleware_layers: Vec::new(),
                config_errors: Vec::new(),
            }),
        }
    }

    // ── Private helper: clone-on-write ──

    fn mutate(&self) -> AgentBuilderInner {
        (*self.inner).clone()
    }

    fn with(inner: AgentBuilderInner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    // ── Accessors ──

    /// The agent name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Configured model, if any.
    pub fn get_model(&self) -> Option<&ModelId> {
        self.inner.model.as_ref()
    }

    /// Configured instruction, if any.
    pub fn get_instruction(&self) -> Option<&str> {
        self.inner.instruction.as_deref()
    }

    /// Configured voice, if any.
    pub fn get_voice(&self) -> Option<&Voice> {
        self.inner.voice.as_ref()
    }

    /// Configured temperature, if any.
    pub fn get_temperature(&self) -> Option<f32> {
        self.inner.temperature
    }

    /// Whether text-only mode is set.
    pub fn is_text_only(&self) -> bool {
        self.inner
            .response_modalities
            .as_ref()
            .map(|m| m == &[Modality::Text])
            .unwrap_or(false)
    }

    /// Configured thinking budget, if any.
    pub fn get_thinking_budget(&self) -> Option<u32> {
        self.inner.thinking_budget
    }

    /// State keys this agent writes.
    pub fn get_writes(&self) -> &[String] {
        &self.inner.writes
    }

    /// State keys this agent reads.
    pub fn get_reads(&self) -> &[String] {
        &self.inner.reads
    }

    /// Sub-agents registered.
    pub fn get_sub_agents(&self) -> &[AgentBuilder] {
        &self.inner.sub_agents
    }

    /// Whether agent runs in isolated state.
    pub fn is_isolated(&self) -> bool {
        self.inner.isolate
    }

    /// Whether agent stays after transfer.
    pub fn is_stay(&self) -> bool {
        self.inner.stay
    }

    /// Number of tool entries.
    pub fn tool_count(&self) -> usize {
        self.inner.tools.len() + self.inner.built_in_tools.len()
    }

    /// Configured top_p, if any.
    pub fn get_top_p(&self) -> Option<f32> {
        self.inner.top_p
    }

    /// Configured top_k, if any.
    pub fn get_top_k(&self) -> Option<u32> {
        self.inner.top_k
    }

    /// Configured max_output_tokens, if any.
    pub fn get_max_output_tokens(&self) -> Option<u32> {
        self.inner.max_output_tokens
    }

    /// Configured stop sequences.
    pub fn get_stop_sequences(&self) -> &[String] {
        &self.inner.stop_sequences
    }

    /// Configured description, if any.
    pub fn get_description(&self) -> Option<&str> {
        self.inner.description.as_deref()
    }

    /// Configured output schema, if any.
    pub fn get_output_schema(&self) -> Option<&serde_json::Value> {
        self.inner.output_schema.as_ref()
    }

    /// Get the configured output key.
    pub fn get_output_key(&self) -> Option<&str> {
        self.inner.output_key.as_deref()
    }

    /// Configured transfer target agent, if any.
    pub fn get_transfer_to(&self) -> Option<&str> {
        self.inner.transfer_to_agent.as_deref()
    }

    /// Number of registered middleware layers.
    pub fn middleware_layer_count(&self) -> usize {
        self.inner.middleware_layers.len()
    }

    // ── Fluent Setters (copy-on-write) ──

    /// Set the Gemini model.
    pub fn model(self, model: ModelId) -> Self {
        let mut inner = self.mutate();
        inner.model = Some(model);
        Self::with(inner)
    }

    /// Set the system instruction.
    pub fn instruction(self, inst: impl Into<String>) -> Self {
        let mut inner = self.mutate();
        inner.instruction = Some(inst.into());
        Self::with(inner)
    }

    /// Set a dynamic instruction source — any `Fn(&State) -> String`
    /// closure or a `TemplateInstruction` (feature `templates`), resolved
    /// against live session state at the start of every run. Wins over
    /// [`instruction`](Self::instruction) when both are set.
    pub fn instruction_provider(
        self,
        provider: impl gemini_adk_rs::instruction::InstructionProvider + 'static,
    ) -> Self {
        let mut inner = self.mutate();
        inner.instruction_provider = Some(Arc::new(provider));
        Self::with(inner)
    }

    /// Set a dynamic model source, resolved against session state at the
    /// start of every run — risk-based escalation to a stronger model, cost
    /// routing to a cheaper one, per-tenant model selection — without
    /// rebuilding the agent. Wins over the constructor's model when set.
    pub fn llm_provider(
        self,
        provider: impl Fn(&gemini_adk_rs::State) -> Arc<dyn BaseLlm> + Send + Sync + 'static,
    ) -> Self {
        let mut inner = self.mutate();
        inner.llm_provider = Some(Arc::new(provider));
        Self::with(inner)
    }

    /// Set the output voice.
    pub fn voice(self, voice: Voice) -> Self {
        let mut inner = self.mutate();
        inner.voice = Some(voice);
        Self::with(inner)
    }

    /// Set the temperature.
    pub fn temperature(self, t: f32) -> Self {
        let mut inner = self.mutate();
        inner.temperature = Some(t);
        Self::with(inner)
    }

    /// Set text-only mode (no audio output).
    pub fn text_only(self) -> Self {
        let mut inner = self.mutate();
        inner.response_modalities = Some(vec![Modality::Text]);
        Self::with(inner)
    }

    /// Set response modalities explicitly.
    pub fn response_modalities(self, modalities: Vec<Modality>) -> Self {
        let mut inner = self.mutate();
        inner.response_modalities = Some(modalities);
        Self::with(inner)
    }

    /// Enable thinking with a token budget.
    pub fn thinking(self, budget: u32) -> Self {
        let mut inner = self.mutate();
        inner.thinking_budget = Some(budget);
        Self::with(inner)
    }

    /// Add a built-in URL context tool.
    pub fn url_context(self) -> Self {
        let mut inner = self.mutate();
        inner.built_in_tools.push(Tool::url_context());
        Self::with(inner)
    }

    /// Add a built-in Google Search tool.
    pub fn google_search(self) -> Self {
        let mut inner = self.mutate();
        inner.built_in_tools.push(Tool::google_search());
        Self::with(inner)
    }

    /// Add a built-in code execution tool.
    pub fn code_execution(self) -> Self {
        let mut inner = self.mutate();
        inner.built_in_tools.push(Tool::code_execution());
        Self::with(inner)
    }

    /// Declare a state key this agent writes.
    pub fn writes(self, key: impl Into<String>) -> Self {
        let mut inner = self.mutate();
        inner.writes.push(key.into());
        Self::with(inner)
    }

    /// Declare a state key this agent reads.
    pub fn reads(self, key: impl Into<String>) -> Self {
        let mut inner = self.mutate();
        inner.reads.push(key.into());
        Self::with(inner)
    }

    /// Add a sub-agent for transfer.
    pub fn sub_agent(self, agent: AgentBuilder) -> Self {
        let mut inner = self.mutate();
        inner.sub_agents.push(agent);
        Self::with(inner)
    }

    /// Run this agent in isolated state (no shared state).
    pub fn isolate(self) -> Self {
        let mut inner = self.mutate();
        inner.isolate = true;
        Self::with(inner)
    }

    /// Keep this agent active after transfer (don't tear down).
    pub fn stay(self) -> Self {
        let mut inner = self.mutate();
        inner.stay = true;
        Self::with(inner)
    }

    /// Set top_p (nucleus sampling).
    pub fn top_p(self, p: f32) -> Self {
        let mut inner = self.mutate();
        inner.top_p = Some(p);
        Self::with(inner)
    }

    /// Set top_k (top-k sampling).
    pub fn top_k(self, k: u32) -> Self {
        let mut inner = self.mutate();
        inner.top_k = Some(k);
        Self::with(inner)
    }

    /// Set maximum output tokens.
    pub fn max_output_tokens(self, n: u32) -> Self {
        let mut inner = self.mutate();
        inner.max_output_tokens = Some(n);
        Self::with(inner)
    }

    /// Set stop sequences.
    pub fn stop_sequences(self, seqs: Vec<String>) -> Self {
        let mut inner = self.mutate();
        inner.stop_sequences = seqs;
        Self::with(inner)
    }

    /// Set a description for this agent (used in tool/agent metadata).
    pub fn description(self, desc: impl Into<String>) -> Self {
        let mut inner = self.mutate();
        inner.description = Some(desc.into());
        Self::with(inner)
    }

    /// Set a JSON schema for structured output.
    pub fn output_schema(self, schema: serde_json::Value) -> Self {
        let mut inner = self.mutate();
        inner.output_schema = Some(schema);
        Self::with(inner)
    }

    /// Set the output key — agent's final text response is auto-saved to this state key.
    pub fn output_key(self, key: impl Into<String>) -> Self {
        let mut inner = self.mutate();
        inner.output_key = Some(key.into());
        Self::with(inner)
    }

    /// Set a default transfer target agent.
    pub fn transfer_to(self, agent_name: impl Into<String>) -> Self {
        let mut inner = self.mutate();
        inner.transfer_to_agent = Some(agent_name.into());
        Self::with(inner)
    }

    // ── Upstream naming aliases ──

    /// Alias for [`instruction`](Self::instruction) — matches upstream Python `Agent.instruct()`.
    pub fn instruct(self, inst: impl Into<String>) -> Self {
        self.instruction(inst)
    }

    /// Alias for [`description`](Self::description) — matches upstream Python `Agent.describe()`.
    pub fn describe(self, desc: impl Into<String>) -> Self {
        self.description(desc)
    }

    /// Register one tool: anything that implements [`ToolFunction`] — a
    /// `SimpleTool`/`TypedTool`, the value a `#[tool]` function returns, or an
    /// `Arc<dyn ToolFunction>` you already hold.
    ///
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// # use std::sync::Arc;
    /// #[tool("Get the weather for a city")]
    /// async fn get_weather(city: String) -> Result<serde_json::Value, ToolError> {
    ///     Ok(serde_json::json!({"city": city, "temp": 22}))
    /// }
    /// let agent = AgentBuilder::new("assistant").tool(get_weather());
    /// ```
    pub fn tool(self, f: impl ToolFunction + 'static) -> Self {
        self.tools(ToolComposite::from_function(Arc::new(f)))
    }

    /// Register tools: a `|`-composed [`ToolComposite`] from the `T`
    /// namespace, or a single [`ToolFunction`].
    ///
    /// `T::mcp(..)` needs an async connection that this synchronous builder
    /// cannot perform; it is rejected by [`build`](Self::build) with a
    /// [`ConfigError`] — attach MCP toolsets to a `Live` session instead.
    ///
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// # use serde_json::json;
    /// let tools = T::simple("greet", "Greet", |_| async { Ok(json!({})) })
    ///     | T::google_search();
    /// AgentBuilder::new("assistant").tools(tools);
    /// ```
    pub fn tools(self, tools: impl Into<ToolComposite>) -> Self {
        use crate::compose::tools::{DeferredTool, ToolResolution};
        let mut inner = self.mutate();
        for entry in tools.into().entries {
            match entry.classify() {
                ToolResolution::Runtime(f) => {
                    inner
                        .tools
                        .push(ToolEntry::Runtime(Arc::new(ToolFunctionEntry(f))));
                }
                ToolResolution::BuiltIn(t) => {
                    inner.built_in_tools.push(t);
                }
                ToolResolution::Agent {
                    name,
                    description,
                    agent,
                } => {
                    // Expose the sub-agent as a callable tool over a fresh State.
                    let tool = gemini_adk_rs::TextAgentTool::from_arc(
                        name,
                        description,
                        agent,
                        gemini_adk_rs::State::new(),
                    );
                    inner
                        .tools
                        .push(ToolEntry::Runtime(Arc::new(ToolFunctionEntry(Arc::new(
                            tool,
                        )))));
                }
                ToolResolution::Deferred(DeferredTool::Mcp { params }) => {
                    // An MCP toolset needs an async handshake, which the
                    // synchronous text-agent `build()` cannot perform. It
                    // belongs on a `Live` session (resolved at connect); make
                    // `build` fail rather than drop the tool silently — the same
                    // outcome `Live::connect` gives an unreachable MCP server.
                    inner.config_errors.push(format!(
                        "T::mcp({params:?}) cannot be attached to a text AgentBuilder: MCP \
                         toolsets need an async connection, which only a Live session performs \
                         (`Live::builder().tools(T::mcp(..))`)"
                    ));
                }
            }
        }
        Self::with(inner)
    }

    /// Attach output guards. Each model response is validated against every
    /// guard; if any rejects the output the agent run fails with an
    /// [`AgentError`](gemini_adk_rs::error::AgentError) listing the violations.
    ///
    /// Accepts a single guard or a `|`-composed [`GuardComposite`]:
    ///
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// AgentBuilder::new("writer").guard(G::pii() | G::length(1, 2000));
    /// ```
    ///
    /// The guards are installed as an `after_model` middleware layer, so they
    /// accumulate with `.middleware(...)` and honor copy-on-write.
    pub fn guard(self, guard: impl Into<GuardComposite>) -> Self {
        let mut inner = self.mutate();
        inner.middleware_layers.push(guard.into().into_middleware());
        Self::with(inner)
    }

    /// Attach a context policy that rewrites conversation history before each
    /// model call (e.g. windowing, role filtering, tool-result exclusion).
    ///
    /// Accepts a single policy or a `+`-composed [`ContextComposite`]:
    ///
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// AgentBuilder::new("chat").context(C::window(10) + C::user_only());
    /// ```
    ///
    /// The policy is installed as a `transform_request` middleware layer.
    pub fn context(self, policy: impl Into<ContextComposite>) -> Self {
        let mut inner = self.mutate();
        inner
            .middleware_layers
            .push(policy.into().into_middleware());
        Self::with(inner)
    }

    /// Disallow transfer to peer agents.
    pub fn no_peers(self) -> Self {
        self.isolate()
    }

    /// Attach middleware — a `|`-composed [`MiddlewareComposite`] from the
    /// `M` namespace or a single `Arc<dyn Middleware>`. All layers are
    /// installed on the compiled `LlmTextAgent` in the order given.
    ///
    /// Multiple calls to `.middleware()` accumulate: the new layers are
    /// appended after any previously registered layers, preserving the
    /// copy-on-write contract.
    ///
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// let agent = AgentBuilder::new("analyst")
    ///     .instruction("Analyze topics")
    ///     .middleware(M::log() | M::latency());
    /// ```
    pub fn middleware(self, middleware: impl Into<MiddlewareComposite>) -> Self {
        let mut inner = self.mutate();
        inner.middleware_layers.extend(middleware.into().layers);
        Self::with(inner)
    }

    // ── Compilation ──

    /// Compile this builder into an executable `TextAgent`.
    ///
    /// The LLM is required because `TextAgent` makes `BaseLlm::generate()` calls.
    /// Builder configuration (instruction, temperature, tools) is transferred to
    /// the resulting agent.
    ///
    /// Fails with a [`ConfigError`] when the configuration cannot be realized
    /// by a text agent — today, an MCP toolset (`T::mcp`) in
    /// [`tools`](Self::tools), which needs the async connect only a `Live`
    /// session performs.
    ///
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// # use std::sync::Arc;
    /// # async fn run() -> Result<(), AgentError> {
    /// let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));
    /// let agent = AgentBuilder::new("analyst")
    ///     .instruction("Analyze the topic")
    ///     .temperature(0.3)
    ///     .build(llm)?;
    ///
    /// let state = State::new();
    /// let result = agent.run(&state).await?;
    /// # let _ = result; Ok(())
    /// # }
    /// ```
    pub fn build(self, llm: Arc<dyn BaseLlm>) -> Result<Arc<dyn TextAgent>, ConfigError> {
        if !self.inner.config_errors.is_empty() {
            return Err(ConfigError {
                issues: self.inner.config_errors.clone(),
            });
        }
        let mut agent = LlmTextAgent::new(&self.inner.name, llm);

        if let Some(inst) = &self.inner.instruction {
            agent = agent.instruction(inst);
        }
        if let Some(provider) = &self.inner.instruction_provider {
            agent = agent.instruction_provider(provider.clone());
        }
        if let Some(provider) = &self.inner.llm_provider {
            let provider_clone = provider.clone();
            agent = agent.llm_provider(move |state| provider_clone(state));
        }
        if let Some(t) = self.inner.temperature {
            agent = agent.temperature(t);
        }
        if let Some(n) = self.inner.max_output_tokens {
            agent = agent.max_output_tokens(n);
        }

        // Build ToolDispatcher from registered tools.
        if !self.inner.tools.is_empty() {
            let mut dispatcher = ToolDispatcher::new();
            for entry in &self.inner.tools {
                match entry {
                    ToolEntry::Runtime(t) => {
                        let kind = t.to_tool_kind();
                        match kind {
                            ToolKind::Function(f) => dispatcher.register_function(f),
                            ToolKind::Streaming(s) => dispatcher.register_streaming(s),
                            ToolKind::InputStream(i) => dispatcher.register_input_streaming(i),
                        }
                    }
                    ToolEntry::Declaration(_) => {
                        // Built-in tool declarations (google_search, etc.) are sent
                        // as-is; they don't have runtime handlers for text dispatch.
                    }
                }
            }
            if !dispatcher.is_empty() {
                agent = agent.tools(Arc::new(dispatcher));
            }
        }

        // Install middleware layers from the builder.
        for mw in &self.inner.middleware_layers {
            agent = agent.add_middleware(mw.clone());
        }

        Ok(Arc::new(agent))
    }
}

/// Adapter that wraps an `Arc<dyn ToolFunction>` as a `ToolEntryTrait`.
#[derive(Clone)]
struct ToolFunctionEntry(Arc<dyn ToolFunction>);

impl ToolEntryTrait for ToolFunctionEntry {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn to_tool_kind(&self) -> ToolKind {
        ToolKind::Function(self.0.clone())
    }
}

impl std::fmt::Debug for AgentBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentBuilder")
            .field("name", &self.inner.name)
            .field("model", &self.inner.model)
            .field("instruction", &self.inner.instruction)
            .field("temperature", &self.inner.temperature)
            .field("text_only", &self.is_text_only())
            .field("tool_count", &self.tool_count())
            .field("sub_agents", &self.inner.sub_agents.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use gemini_adk_rs::llm::{LlmError, LlmRequest, LlmResponse};
    use gemini_genai_rs::prelude::{Content, Part, Role};

    /// A mock LLM for build() tests.
    struct MockLlm(String);

    #[async_trait]
    impl BaseLlm for MockLlm {
        fn model_id(&self) -> &str {
            "mock"
        }
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                content: Content {
                    role: Some(Role::Model),
                    parts: vec![Part::Text {
                        text: self.0.clone(),
                    }],
                },
                finish_reason: Some("STOP".into()),
                usage: None,
            })
        }
    }

    #[test]
    fn builder_creates_with_name() {
        let b = AgentBuilder::new("test-agent");
        assert_eq!(b.name(), "test-agent");
    }

    #[test]
    fn fluent_chaining_works() {
        let b = AgentBuilder::new("agent")
            .instruction("Be helpful")
            .temperature(0.7)
            .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);

        assert_eq!(b.get_instruction(), Some("Be helpful"));
        assert_eq!(b.get_temperature(), Some(0.7));
        assert_eq!(b.get_model(), Some(&ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO));
    }

    #[test]
    fn copy_on_write_clone_independence() {
        let base = AgentBuilder::new("base").temperature(0.5);
        let variant = base.clone().temperature(0.9);

        // Original unchanged
        assert_eq!(base.get_temperature(), Some(0.5));
        // Variant has new value
        assert_eq!(variant.get_temperature(), Some(0.9));
    }

    #[test]
    fn text_only_sets_modalities() {
        let b = AgentBuilder::new("text").text_only();
        assert!(b.is_text_only());
    }

    #[test]
    fn url_context_adds_tool() {
        let b = AgentBuilder::new("search").url_context();
        assert_eq!(b.tool_count(), 1);
    }

    #[test]
    fn google_search_adds_tool() {
        let b = AgentBuilder::new("search").google_search();
        assert_eq!(b.tool_count(), 1);
    }

    #[test]
    fn code_execution_adds_tool() {
        let b = AgentBuilder::new("code").code_execution();
        assert_eq!(b.tool_count(), 1);
    }

    #[test]
    fn thinking_sets_budget() {
        let b = AgentBuilder::new("thinker").thinking(2048);
        assert_eq!(b.get_thinking_budget(), Some(2048));
    }

    #[test]
    fn writes_and_reads_keys() {
        let b = AgentBuilder::new("data").writes("output").reads("input");
        assert_eq!(b.get_writes(), &["output"]);
        assert_eq!(b.get_reads(), &["input"]);
    }

    #[test]
    fn sub_agent_registration() {
        let child = AgentBuilder::new("child");
        let parent = AgentBuilder::new("parent").sub_agent(child);
        assert_eq!(parent.get_sub_agents().len(), 1);
        assert_eq!(parent.get_sub_agents()[0].name(), "child");
    }

    #[test]
    fn isolate_and_stay() {
        let b = AgentBuilder::new("agent").isolate().stay();
        assert!(b.is_isolated());
        assert!(b.is_stay());
    }

    #[test]
    fn debug_display() {
        let b = AgentBuilder::new("debug-test");
        let debug = format!("{b:?}");
        assert!(debug.contains("debug-test"));
    }

    #[test]
    fn top_p_sets_value() {
        let b = AgentBuilder::new("agent").top_p(0.95);
        assert_eq!(b.get_top_p(), Some(0.95));
    }

    #[test]
    fn top_k_sets_value() {
        let b = AgentBuilder::new("agent").top_k(40);
        assert_eq!(b.get_top_k(), Some(40));
    }

    #[test]
    fn max_output_tokens_sets_value() {
        let b = AgentBuilder::new("agent").max_output_tokens(4096);
        assert_eq!(b.get_max_output_tokens(), Some(4096));
    }

    #[test]
    fn stop_sequences_sets_value() {
        let b =
            AgentBuilder::new("agent").stop_sequences(vec!["END".to_string(), "STOP".to_string()]);
        assert_eq!(b.get_stop_sequences().len(), 2);
    }

    #[test]
    fn description_sets_value() {
        let b = AgentBuilder::new("agent").description("A helpful agent");
        assert_eq!(b.get_description(), Some("A helpful agent"));
    }

    #[test]
    fn output_schema_sets_value() {
        let schema = serde_json::json!({"type": "object"});
        let b = AgentBuilder::new("agent").output_schema(schema.clone());
        assert_eq!(b.get_output_schema(), Some(&schema));
    }

    #[test]
    fn transfer_to_sets_value() {
        let b = AgentBuilder::new("agent").transfer_to("target-agent");
        assert_eq!(b.get_transfer_to(), Some("target-agent"));
    }

    #[test]
    fn full_fluent_chain() {
        let b = AgentBuilder::new("full-agent")
            .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
            .instruction("Be helpful")
            .temperature(0.7)
            .top_p(0.95)
            .top_k(40)
            .max_output_tokens(4096)
            .thinking(2048)
            .description("A fully configured agent")
            .google_search()
            .writes("output")
            .reads("input");

        assert_eq!(b.name(), "full-agent");
        assert_eq!(b.get_temperature(), Some(0.7));
        assert_eq!(b.get_top_p(), Some(0.95));
        assert_eq!(b.get_top_k(), Some(40));
        assert_eq!(b.get_max_output_tokens(), Some(4096));
        assert_eq!(b.get_thinking_budget(), Some(2048));
        assert_eq!(b.get_description(), Some("A fully configured agent"));
        assert_eq!(b.tool_count(), 1);
    }

    // ── build() tests ──

    #[tokio::test]
    async fn build_produces_executable_agent() {
        let llm: Arc<dyn BaseLlm> = Arc::new(MockLlm("built agent output".into()));
        let agent = AgentBuilder::new("test")
            .instruction("Be helpful")
            .temperature(0.5)
            .build(llm)
            .unwrap();

        assert_eq!(agent.name(), "test");
        let state = gemini_adk_rs::State::new();
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "built agent output");
    }

    #[tokio::test]
    async fn build_stores_output_in_state() {
        let llm: Arc<dyn BaseLlm> = Arc::new(MockLlm("state output".into()));
        let agent = AgentBuilder::new("test").build(llm).unwrap();
        let state = gemini_adk_rs::State::new();
        agent.run(&state).await.unwrap();
        assert_eq!(state.get::<String>("output"), Some("state output".into()));
    }

    #[tokio::test]
    async fn build_reads_input_from_state() {
        use gemini_adk_rs::llm::LlmRequest;

        // An LLM that echoes whatever it receives.
        struct EchoLlm;
        #[async_trait]
        impl BaseLlm for EchoLlm {
            fn model_id(&self) -> &str {
                "echo"
            }
            async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
                let text: String = req
                    .contents
                    .iter()
                    .flat_map(|c| &c.parts)
                    .filter_map(|p| match p {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text { text }],
                    },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            }
        }

        let agent = AgentBuilder::new("echo").build(Arc::new(EchoLlm)).unwrap();
        let state = gemini_adk_rs::State::new();
        let _ = state.set("input", "hello from state");
        let result = agent.run(&state).await.unwrap();
        assert!(result.contains("hello from state"));
    }

    // ── Middleware end-to-end tests ──

    /// A mock LLM that issues one tool call and then returns text.
    struct ToolCallingMockLlm {
        tool_name: &'static str,
        final_text: &'static str,
    }

    #[async_trait]
    impl BaseLlm for ToolCallingMockLlm {
        fn model_id(&self) -> &str {
            "tool-mock"
        }

        async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            use gemini_genai_rs::prelude::FunctionCall;

            // If any part is a FunctionResponse, we already dispatched — return text.
            let already_responded = req
                .contents
                .iter()
                .flat_map(|c| &c.parts)
                .any(|p| matches!(p, Part::FunctionResponse { .. }));

            if already_responded {
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text {
                            text: self.final_text.to_string(),
                        }],
                    },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            } else {
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::FunctionCall {
                            function_call: FunctionCall {
                                name: self.tool_name.to_string(),
                                args: serde_json::json!({"x": 1}),
                                id: Some("call-1".into()),
                            },
                        }],
                    },
                    finish_reason: None,
                    usage: None,
                })
            }
        }
    }

    /// Verify that `M::before_model` and `M::after_tool` hooks fire when the agent runs.
    #[tokio::test]
    async fn middleware_hooks_fire_end_to_end() {
        use crate::compose::middleware::M;
        use gemini_adk_rs::tool::SimpleTool;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let before_model_count = Arc::new(AtomicUsize::new(0));
        let after_tool_count = Arc::new(AtomicUsize::new(0));

        let bm = before_model_count.clone();
        let at = after_tool_count.clone();

        let mw = M::before_model(move |_req| {
            bm.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }) | M::after_tool(move |_call, _result| {
            at.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let llm: Arc<dyn BaseLlm> = Arc::new(ToolCallingMockLlm {
            tool_name: "echo_tool",
            final_text: "done",
        });

        let agent = AgentBuilder::new("mw-test")
            .middleware(mw)
            .tool(SimpleTool::new(
                "echo_tool",
                "Echo tool",
                None,
                |_args| async move { Ok(serde_json::json!({"echo": true})) },
            ))
            .build(llm)
            .unwrap();

        let state = gemini_adk_rs::State::new();
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "done");

        // before_model fires once per LLM call: first call (tool call) + second call (final text).
        assert_eq!(
            before_model_count.load(Ordering::SeqCst),
            2,
            "before_model should fire for each generate() call"
        );
        // after_tool fires once per successful tool dispatch.
        assert_eq!(
            after_tool_count.load(Ordering::SeqCst),
            1,
            "after_tool should fire once for the tool dispatch"
        );
    }

    /// Verify copy-on-write: adding middleware to a clone does not affect the original.
    #[test]
    fn middleware_copy_on_write() {
        use crate::compose::middleware::M;

        let base = AgentBuilder::new("base").instruction("base");
        let with_mw = base.clone().middleware(M::log() | M::latency());

        // Original should have no middleware layers.
        assert_eq!(base.middleware_layer_count(), 0);
        // Clone with middleware should have 2 layers.
        assert_eq!(with_mw.middleware_layer_count(), 2);
    }

    /// Verify `on_error` hook fires when the agent errors.
    #[tokio::test]
    async fn middleware_on_error_fires_on_failure() {
        use crate::compose::middleware::M;
        use gemini_adk_rs::llm::LlmError;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let error_count = Arc::new(AtomicUsize::new(0));
        let ec = error_count.clone();

        let mw = M::on_error(move |_err| {
            ec.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        struct FailLlm;
        #[async_trait]
        impl BaseLlm for FailLlm {
            fn model_id(&self) -> &str {
                "fail"
            }
            async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                Err(LlmError::RequestFailed("boom".into()))
            }
        }

        let agent = AgentBuilder::new("error-test")
            .middleware(mw)
            .build(Arc::new(FailLlm))
            .unwrap();

        let state = gemini_adk_rs::State::new();
        let result = agent.run(&state).await;
        assert!(result.is_err(), "agent should fail");
        assert_eq!(
            error_count.load(Ordering::SeqCst),
            1,
            "on_error should fire exactly once"
        );
    }

    // ── Guard / context wiring tests ──

    /// A mock LLM that echoes a fixed response and records the number of
    /// `contents` it was asked to generate from (to observe context rewriting).
    struct RecordingLlm {
        text: &'static str,
        seen_len: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl BaseLlm for RecordingLlm {
        fn model_id(&self) -> &str {
            "recording-mock"
        }

        async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            self.seen_len
                .store(req.contents.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(LlmResponse {
                content: Content {
                    role: Some(Role::Model),
                    parts: vec![Part::Text {
                        text: self.text.to_string(),
                    }],
                },
                finish_reason: Some("STOP".into()),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn guard_blocks_violating_output() {
        use crate::compose::guards::G;

        let llm: Arc<dyn BaseLlm> = Arc::new(RecordingLlm {
            text: "you can reach me at agent@example.com",
            seen_len: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });

        let agent = AgentBuilder::new("guarded")
            .guard(G::pii())
            .build(llm)
            .unwrap();

        let state = gemini_adk_rs::State::new();
        let err = agent.run(&state).await.unwrap_err();
        assert!(
            err.to_string().contains("guard violation"),
            "PII guard should veto the response, got: {err}"
        );
    }

    #[tokio::test]
    async fn guard_allows_clean_output() {
        use crate::compose::guards::G;

        let llm: Arc<dyn BaseLlm> = Arc::new(RecordingLlm {
            text: "all clean here",
            seen_len: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });

        let agent = AgentBuilder::new("guarded")
            .guard(G::pii() | G::length(1, 1000))
            .build(llm)
            .unwrap();

        let state = gemini_adk_rs::State::new();
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "all clean here");
    }

    #[tokio::test]
    async fn context_policy_rewrites_request_history() {
        use crate::compose::context::C;

        // The agent seeds one user turn; a prepend policy injects a second turn,
        // so the LLM should see 2 contents — proving transform_request ran.
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let llm: Arc<dyn BaseLlm> = Arc::new(RecordingLlm {
            text: "ok",
            seen_len: seen.clone(),
        });

        let agent = AgentBuilder::new("ctx")
            .context(C::prepend(Content::user("system preamble")))
            .build(llm)
            .unwrap();

        let state = gemini_adk_rs::State::new();
        let _ = state.set("input", "hello");
        let _ = agent.run(&state).await.unwrap();
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "context policy should have prepended a turn before the model call"
        );
    }

    #[tokio::test]
    async fn context_window_trims_history() {
        use crate::compose::context::C;

        // window(1) keeps only the last turn. We seed a single input turn and
        // prepend two extra turns, then window down to 1 — the model sees 1.
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let llm: Arc<dyn BaseLlm> = Arc::new(RecordingLlm {
            text: "ok",
            seen_len: seen.clone(),
        });

        let agent = AgentBuilder::new("ctx")
            .context(C::prepend(Content::user("a")) + C::prepend(Content::user("b")) + C::window(1))
            .build(llm)
            .unwrap();

        let state = gemini_adk_rs::State::new();
        let _ = state.set("input", "hello");
        let _ = agent.run(&state).await.unwrap();
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "window(1) should trim history to the last turn"
        );
    }
}

#[cfg(test)]
mod mcp_rejection_tests {
    use super::*;
    use gemini_adk_rs::llm::{LlmError, LlmRequest, LlmResponse};

    struct NeverLlm;
    #[async_trait::async_trait]
    impl BaseLlm for NeverLlm {
        fn model_id(&self) -> &str {
            "never"
        }
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Err(LlmError::RequestFailed("never".into()))
        }
    }

    /// `T::mcp` on a text agent is a build error naming the tool kind — the
    /// same way `Live::connect` fails on it — never a silent drop.
    #[test]
    fn mcp_toolset_is_a_build_error() {
        use crate::compose::tools::T;
        let err = AgentBuilder::new("text")
            .tools(T::mcp("node ./server.js"))
            .build(Arc::new(NeverLlm))
            .err()
            .expect("build must fail");
        assert!(err.to_string().contains("T::mcp"), "{err}");
        assert!(err.to_string().contains("Live"), "{err}");
    }

    /// `tool(..)` takes any `ToolFunction`, including an `Arc<dyn ToolFunction>`.
    #[test]
    fn tool_accepts_values_and_arcs() {
        use gemini_adk_rs::tool::SimpleTool;
        let arc: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new("a", "a", None, |_| async {
            Ok(serde_json::json!({}))
        }));
        let b = AgentBuilder::new("t")
            .tool(SimpleTool::new("b", "b", None, |_| async {
                Ok(serde_json::json!({}))
            }))
            .tool(arc.clone())
            .tools(arc);
        assert_eq!(b.tool_count(), 3);
    }
}
