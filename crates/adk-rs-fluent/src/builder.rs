//! AgentBuilder — copy-on-write immutable builder for fluent agent construction.
//!
//! Every mutation returns a new builder (original unchanged), so builders
//! are safely shareable as templates.

use std::sync::Arc;

use rs_genai::prelude::{
    GeminiModel, Modality, Tool, Voice,
};
use rs_adk::llm::BaseLlm;
use rs_adk::text::{LlmTextAgent, TextAgent};
use rs_adk::tool::{ToolDispatcher, ToolKind};

/// Inner state of an AgentBuilder — shared via Arc for copy-on-write.
#[derive(Clone)]
struct AgentBuilderInner {
    name: String,
    model: Option<GeminiModel>,
    instruction: Option<String>,
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
    transfer_to_agent: Option<String>,
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
/// ```rust,no_run
/// use adk_rs_fluent::builder::AgentBuilder;
///
/// let base = AgentBuilder::new("researcher")
///     .instruction("You are a research assistant.");
///
/// // Clone-on-write: base is unchanged
/// let variant_a = base.clone().temperature(0.3);
/// let variant_b = base.clone().temperature(0.9);
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
                transfer_to_agent: None,
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
    pub fn get_model(&self) -> Option<&GeminiModel> {
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

    /// Configured transfer target agent, if any.
    pub fn get_transfer_to(&self) -> Option<&str> {
        self.inner.transfer_to_agent.as_deref()
    }

    // ── Fluent Setters (copy-on-write) ──

    /// Set the Gemini model.
    pub fn model(self, model: GeminiModel) -> Self {
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

    /// Set a default transfer target agent.
    pub fn transfer_to(self, agent_name: impl Into<String>) -> Self {
        let mut inner = self.mutate();
        inner.transfer_to_agent = Some(agent_name.into());
        Self::with(inner)
    }

    // ── Compilation ──

    /// Compile this builder into an executable `TextAgent`.
    ///
    /// The LLM is required because `TextAgent` makes `BaseLlm::generate()` calls.
    /// Builder configuration (instruction, temperature, tools) is transferred to
    /// the resulting agent.
    ///
    /// ```rust,ignore
    /// let agent = AgentBuilder::new("analyst")
    ///     .instruction("Analyze the topic")
    ///     .temperature(0.3)
    ///     .build(llm);
    ///
    /// let result = agent.run(&state).await?;
    /// ```
    pub fn build(self, llm: Arc<dyn BaseLlm>) -> Arc<dyn TextAgent> {
        let mut agent = LlmTextAgent::new(&self.inner.name, llm);

        if let Some(inst) = &self.inner.instruction {
            agent = agent.instruction(inst);
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

        Arc::new(agent)
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
    use rs_adk::llm::{LlmError, LlmRequest, LlmResponse};
    use rs_genai::prelude::{Content, Part, Role};

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
                    parts: vec![Part::Text { text: self.0.clone() }],
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
            .model(GeminiModel::Gemini2_0FlashLive);

        assert_eq!(b.get_instruction(), Some("Be helpful"));
        assert_eq!(b.get_temperature(), Some(0.7));
        assert_eq!(b.get_model(), Some(&GeminiModel::Gemini2_0FlashLive));
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
        let b = AgentBuilder::new("data")
            .writes("output")
            .reads("input");
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
        let debug = format!("{:?}", b);
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
        let b = AgentBuilder::new("agent")
            .stop_sequences(vec!["END".to_string(), "STOP".to_string()]);
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
            .model(GeminiModel::Gemini2_0FlashLive)
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
            .build(llm);

        assert_eq!(agent.name(), "test");
        let state = rs_adk::State::new();
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "built agent output");
    }

    #[tokio::test]
    async fn build_stores_output_in_state() {
        let llm: Arc<dyn BaseLlm> = Arc::new(MockLlm("state output".into()));
        let agent = AgentBuilder::new("test").build(llm);
        let state = rs_adk::State::new();
        agent.run(&state).await.unwrap();
        assert_eq!(
            state.get::<String>("output"),
            Some("state output".into())
        );
    }

    #[tokio::test]
    async fn build_reads_input_from_state() {
        use rs_adk::llm::LlmRequest;

        // An LLM that echoes whatever it receives.
        struct EchoLlm;
        #[async_trait]
        impl BaseLlm for EchoLlm {
            fn model_id(&self) -> &str { "echo" }
            async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
                let text: String = req.contents.iter()
                    .flat_map(|c| &c.parts)
                    .filter_map(|p| match p { Part::Text { text } => Some(text.as_str()), _ => None })
                    .collect::<Vec<_>>()
                    .join("");
                Ok(LlmResponse {
                    content: Content { role: Some(Role::Model), parts: vec![Part::Text { text }] },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            }
        }

        let agent = AgentBuilder::new("echo").build(Arc::new(EchoLlm));
        let state = rs_adk::State::new();
        state.set("input", "hello from state");
        let result = agent.run(&state).await.unwrap();
        assert!(result.contains("hello from state"));
    }
}
