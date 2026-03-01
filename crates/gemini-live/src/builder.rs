//! AgentBuilder — copy-on-write immutable builder for fluent agent construction.
//!
//! Every mutation returns a new builder (original unchanged), so builders
//! are safely shareable as templates.

use std::sync::Arc;

use gemini_live_wire::prelude::{
    GeminiModel, Modality, Tool, Voice,
};
use gemini_live_runtime::tool::ToolKind;

/// Inner state of an AgentBuilder — shared via Arc for copy-on-write.
#[derive(Clone)]
struct AgentBuilderInner {
    name: String,
    model: Option<GeminiModel>,
    instruction: Option<String>,
    voice: Option<Voice>,
    temperature: Option<f32>,
    response_modalities: Option<Vec<Modality>>,
    thinking_budget: Option<u32>,
    tools: Vec<ToolEntry>,
    built_in_tools: Vec<Tool>,
    writes: Vec<String>,
    reads: Vec<String>,
    sub_agents: Vec<AgentBuilder>,
    isolate: bool,
    stay: bool,
}

/// An entry in the builder's tool list — either a runtime ToolKind or a declaration.
#[derive(Clone)]
pub enum ToolEntry {
    Runtime(Arc<dyn ToolEntryTrait>),
    Declaration(Tool),
}

/// Trait for tool entries that can provide a name (for dedup/inspection).
pub trait ToolEntryTrait: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn to_tool_kind(&self) -> ToolKind;
}

/// Copy-on-write immutable builder for agent construction.
///
/// Every setter returns a new `AgentBuilder`, leaving the original unchanged.
/// This makes builders safe to share as templates.
///
/// ```rust,no_run
/// use gemini_live::builder::AgentBuilder;
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
                response_modalities: None,
                thinking_budget: None,
                tools: Vec::new(),
                built_in_tools: Vec::new(),
                writes: Vec::new(),
                reads: Vec::new(),
                sub_agents: Vec::new(),
                isolate: false,
                stay: false,
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
}
