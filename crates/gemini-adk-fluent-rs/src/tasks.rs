//! Skill authoring and the shared task runtime.

pub use crate::spec::{
    SkillSpec, SkillToolSpec, TaskExpectation, TaskObservationKind, TaskScenario, TaskStep,
    TaskTraceSnapshot,
};
pub use gemini_adk_rs::tasks::*;

/// Small fluent authoring surface over the serializable skill definition.
#[derive(Debug, Clone)]
pub struct Skill {
    spec: SkillSpec,
}

impl Skill {
    /// Start a versioned local capability.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            spec: SkillSpec {
                name: name.into(),
                version: version.into(),
                ..Default::default()
            },
        }
    }
    /// Set the short catalog description.
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.spec.description = text.into();
        self
    }
    /// Set instructions loaded when this capability is foreground.
    pub fn instruction(mut self, text: impl Into<String>) -> Self {
        self.spec.instruction = text.into();
        self
    }
    /// Declare a typed input field.
    pub fn input(mut self, name: impl Into<String>, field: crate::spec::StateFieldSpec) -> Self {
        self.spec.inputs.insert(name.into(), field);
        self
    }
    /// Declare a state field exported on completion.
    pub fn output(mut self, name: impl Into<String>, field: crate::spec::StateFieldSpec) -> Self {
        self.spec.outputs.insert(name.into(), field);
        self
    }
    /// Add a declared operation with an explicit external-effect policy.
    pub fn tool(mut self, tool: crate::spec::ToolSpec, effect: TaskToolEffect) -> Self {
        self.spec.tools.push(SkillToolSpec { tool, effect });
        self
    }
    /// Attach optional user-facing stages and digressions.
    pub fn conversation(mut self, conversation: crate::conversation::ConversationSpec) -> Self {
        self.spec.conversation = Some(conversation);
        self
    }
    /// Declare task-private state and its optional default.
    pub fn state(mut self, name: impl Into<String>, field: crate::spec::StateFieldSpec) -> Self {
        self.spec.state.insert(name.into(), field);
        self
    }
    /// Extract structured fields from this activation's owned transcript.
    pub fn extract(mut self, extractor: crate::spec::ExtractSpec) -> Self {
        self.spec.extract.push(extractor);
        self
    }
    /// Add a dependency-ordered derived state value.
    pub fn computed(mut self, computed: crate::spec::ComputedSpec) -> Self {
        self.spec.computed.push(computed);
        self
    }
    /// React to an accepted task-local state change.
    pub fn watch(mut self, watch: crate::spec::WatchSpec) -> Self {
        self.spec.watch.push(watch);
        self
    }
    /// Detect a condition over task-local turns or active time.
    pub fn pattern(mut self, pattern: crate::spec::PatternSpec) -> Self {
        self.spec.patterns.push(pattern);
        self
    }
    /// Project declared memory slots through the existing memory binding.
    pub fn memory(mut self, memory: crate::spec::MemorySpec) -> Self {
        self.spec.memory = Some(memory);
        self
    }
    /// Compile tools to be rebound against each private invocation snapshot.
    pub fn compile(self, resources: &crate::spec::SpecResources) -> Result<CompiledSkill, String> {
        self.spec.compile(resources)
    }
    /// Return the data document for editors or publication through existing bundles.
    pub fn into_spec(self) -> SkillSpec {
        self.spec
    }
}
