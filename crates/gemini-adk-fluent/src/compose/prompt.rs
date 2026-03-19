//! P — Prompt composition.
//!
//! Compose prompt sections additively with `+`.

/// A section of a prompt.
#[derive(Clone)]
pub struct PromptSection {
    /// The semantic category of this section.
    pub kind: PromptSectionKind,
    /// The text content of this section.
    pub content: String,
    /// Optional name for this section (used for name-based filtering/reordering).
    pub name: Option<String>,
    /// Optional adapter function for adaptive prompts.
    pub adapter: Option<std::sync::Arc<dyn Fn(&str) -> String + Send + Sync>>,
}

impl std::fmt::Debug for PromptSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptSection")
            .field("kind", &self.kind)
            .field("content", &self.content)
            .field("name", &self.name)
            .field(
                "adapter",
                &self.adapter.as_ref().map(|_| "Fn(&str) -> String"),
            )
            .finish()
    }
}

/// The semantic category of a prompt section.
#[derive(Clone, Debug, PartialEq)]
pub enum PromptSectionKind {
    /// Agent role definition (e.g., "You are ...").
    Role,
    /// Task description (e.g., "Your task: ...").
    Task,
    /// Behavioral constraint (e.g., "Constraint: ...").
    Constraint,
    /// Output format specification.
    Format,
    /// Input/output example.
    Example,
    /// Free-form text.
    Text,
    /// Background context.
    Context,
    /// Personality or persona description.
    Persona,
    /// Bulleted guideline list.
    Guidelines,
    /// Step-by-step scaffolded prompt.
    Scaffolded,
    /// Versioned prompt section.
    Versioned,
    /// Marker indicating the prompt should be compressed.
    Compressed,
    /// Adaptive prompt that adjusts based on context.
    Adaptive,
}

impl PromptSection {
    /// Render this section as a formatted string.
    pub fn render(&self) -> String {
        match &self.kind {
            PromptSectionKind::Role => format!("You are {}.", self.content),
            PromptSectionKind::Task => format!("Your task: {}", self.content),
            PromptSectionKind::Constraint => format!("Constraint: {}", self.content),
            PromptSectionKind::Format => format!("Output format: {}", self.content),
            PromptSectionKind::Example => self.content.clone(),
            PromptSectionKind::Text => self.content.clone(),
            PromptSectionKind::Context => format!("Context: {}", self.content),
            PromptSectionKind::Persona => format!("Persona: {}", self.content),
            PromptSectionKind::Guidelines => self.content.clone(),
            PromptSectionKind::Scaffolded => self.content.clone(),
            PromptSectionKind::Versioned => self.content.clone(),
            PromptSectionKind::Compressed => format!("[compressed] {}", self.content),
            PromptSectionKind::Adaptive => self.content.clone(),
        }
    }

    /// Render an adaptive section with a context string.
    ///
    /// If this section has an adapter function, invokes it with the given context.
    /// Otherwise, falls back to the normal `render()`.
    pub fn render_with_context(&self, ctx: &str) -> String {
        if let Some(adapter) = &self.adapter {
            adapter(ctx)
        } else {
            self.render()
        }
    }

    /// Attach an adapter function to this section (builder pattern).
    pub fn with_adapter<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> String + Send + Sync + 'static,
    {
        self.adapter = Some(std::sync::Arc::new(f));
        self
    }
}

/// Compose two prompt sections with `+`.
impl std::ops::Add for PromptSection {
    type Output = PromptComposite;

    fn add(self, rhs: PromptSection) -> Self::Output {
        PromptComposite {
            sections: vec![self, rhs],
        }
    }
}

/// A composed prompt built from multiple sections.
#[derive(Clone, Debug)]
pub struct PromptComposite {
    /// The ordered list of prompt sections.
    pub sections: Vec<PromptSection>,
}

impl PromptComposite {
    /// Render the full prompt by joining all sections.
    pub fn render(&self) -> String {
        self.sections
            .iter()
            .map(|s| s.render())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

impl PromptComposite {
    /// Keep only sections of specified kinds.
    pub fn only(self, kinds: &[PromptSectionKind]) -> Self {
        Self {
            sections: self
                .sections
                .into_iter()
                .filter(|s| kinds.contains(&s.kind))
                .collect(),
        }
    }

    /// Remove sections of specified kinds.
    pub fn without(self, kinds: &[PromptSectionKind]) -> Self {
        Self {
            sections: self
                .sections
                .into_iter()
                .filter(|s| !kinds.contains(&s.kind))
                .collect(),
        }
    }

    /// Reorder sections by kind priority.
    pub fn reorder(mut self, order: &[PromptSectionKind]) -> Self {
        self.sections.sort_by_key(|s| {
            order
                .iter()
                .position(|k| k == &s.kind)
                .unwrap_or(usize::MAX)
        });
        self
    }

    /// Reorder sections by name. Sections matching the given names come first
    /// (in the specified order); unmatched sections are appended at the end
    /// in their original order.
    pub fn reorder_by_name(self, order: &[&str]) -> Self {
        let order: Vec<&str> = order.to_vec();
        let mut ordered = Vec::with_capacity(self.sections.len());
        let mut remaining = self.sections;

        for name in &order {
            let mut i = 0;
            while i < remaining.len() {
                if remaining[i].name.as_deref() == Some(name) {
                    ordered.push(remaining.remove(i));
                } else {
                    i += 1;
                }
            }
        }
        ordered.extend(remaining);
        Self { sections: ordered }
    }

    /// Keep only sections whose names match the given list.
    pub fn only_by_name(self, names: &[&str]) -> Self {
        Self {
            sections: self
                .sections
                .into_iter()
                .filter(|s| {
                    s.name
                        .as_deref()
                        .map(|n| names.contains(&n))
                        .unwrap_or(false)
                })
                .collect(),
        }
    }

    /// Remove sections whose names match the given list.
    pub fn without_by_name(self, names: &[&str]) -> Self {
        Self {
            sections: self
                .sections
                .into_iter()
                .filter(|s| {
                    s.name
                        .as_deref()
                        .map(|n| !names.contains(&n))
                        .unwrap_or(true)
                })
                .collect(),
        }
    }

    /// Apply a `PromptTransform` to this composite.
    pub fn apply(self, transform: PromptTransform) -> Self {
        match transform {
            PromptTransform::Reorder(order) => {
                let refs: Vec<&str> = order.iter().map(|s| s.as_str()).collect();
                self.reorder_by_name(&refs)
            }
            PromptTransform::Only(names) => {
                let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                self.only_by_name(&refs)
            }
            PromptTransform::Without(names) => {
                let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                self.without_by_name(&refs)
            }
        }
    }
}

/// A declarative transform that can be applied to a `PromptComposite`.
///
/// Created by `P::reorder()`, `P::only()`, and `P::without()`.
#[derive(Clone, Debug)]
pub enum PromptTransform {
    /// Reorder sections by name.
    Reorder(Vec<String>),
    /// Keep only sections with these names.
    Only(Vec<String>),
    /// Remove sections with these names.
    Without(Vec<String>),
}

impl From<PromptComposite> for String {
    fn from(p: PromptComposite) -> String {
        p.render()
    }
}

impl From<PromptSection> for String {
    fn from(s: PromptSection) -> String {
        s.render()
    }
}

impl std::ops::Add<PromptSection> for PromptComposite {
    type Output = PromptComposite;

    fn add(mut self, rhs: PromptSection) -> Self::Output {
        self.sections.push(rhs);
        self
    }
}

/// The `P` namespace — static factory methods for prompt sections.
pub struct P;

impl P {
    /// Define the agent's role.
    pub fn role(role: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Role,
            content: role.to_string(),
            name: Some("role".to_string()),
            adapter: None,
        }
    }

    /// Define the agent's task.
    pub fn task(task: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Task,
            content: task.to_string(),
            name: Some("task".to_string()),
            adapter: None,
        }
    }

    /// Add a constraint.
    pub fn constraint(c: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Constraint,
            content: c.to_string(),
            name: Some("constraint".to_string()),
            adapter: None,
        }
    }

    /// Specify output format.
    pub fn format(f: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Format,
            content: f.to_string(),
            name: Some("format".to_string()),
            adapter: None,
        }
    }

    /// Add an input/output example.
    pub fn example(input: &str, output: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Example,
            content: format!("Example:\nInput: {input}\nOutput: {output}"),
            name: Some("example".to_string()),
            adapter: None,
        }
    }

    /// Add free-form text.
    pub fn text(t: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Text,
            content: t.to_string(),
            name: None,
            adapter: None,
        }
    }

    /// Add background context.
    pub fn context(ctx: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Context,
            content: ctx.to_string(),
            name: Some("context".to_string()),
            adapter: None,
        }
    }

    /// Define a personality/persona.
    pub fn persona(desc: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Persona,
            content: desc.to_string(),
            name: Some("persona".to_string()),
            adapter: None,
        }
    }

    /// Add multiple guidelines as a bulleted list.
    pub fn guidelines(items: &[&str]) -> PromptSection {
        let content = items
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n");
        PromptSection {
            kind: PromptSectionKind::Guidelines,
            content: format!("Guidelines:\n{content}"),
            name: Some("guidelines".to_string()),
            adapter: None,
        }
    }

    /// Add a named section (flexible section kind).
    pub fn section(name: &str, text: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Text,
            content: format!("## {}\n{}", name, text),
            name: Some(name.to_string()),
            adapter: None,
        }
    }

    /// Template with `{key}` placeholders — rendered with state values at runtime.
    pub fn template(tpl: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Text,
            content: tpl.to_string(),
            name: Some("template".to_string()),
            adapter: None,
        }
    }

    /// Reorder sections in a composite by name.
    ///
    /// Sections whose names match the given order come first (in order);
    /// unmatched sections are appended at the end in their original order.
    ///
    /// ```ignore
    /// let prompt = (P::role("analyst") + P::task("analyze") + P::format("JSON"))
    ///     .reorder_by_name(&["format", "role", "task"]);
    /// ```
    pub fn reorder(order: &[&str]) -> PromptTransform {
        let order: Vec<String> = order.iter().map(|s| s.to_string()).collect();
        PromptTransform::Reorder(order)
    }

    /// Keep only sections whose names match the given list.
    ///
    /// ```ignore
    /// let prompt = (P::role("analyst") + P::task("analyze") + P::format("JSON"))
    ///     .only_by_name(&["role", "task"]);
    /// ```
    pub fn only(names: &[&str]) -> PromptTransform {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        PromptTransform::Only(names)
    }

    /// Remove sections whose names match the given list.
    ///
    /// ```ignore
    /// let prompt = (P::role("analyst") + P::task("analyze") + P::format("JSON"))
    ///     .without_by_name(&["format"]);
    /// ```
    pub fn without(names: &[&str]) -> PromptTransform {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        PromptTransform::Without(names)
    }

    /// Mark prompt for compression. This is a placeholder/marker indicating
    /// the prompt content should be compressed before sending to the model.
    pub fn compress() -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Compressed,
            content: String::new(),
            name: Some("compress".to_string()),
            adapter: None,
        }
    }

    /// Create an adaptive prompt that adjusts based on a context function.
    ///
    /// The function receives context (e.g., token budget, turn count) and returns
    /// the adapted prompt text.
    ///
    /// ```ignore
    /// let prompt = P::adapt(|ctx| {
    ///     if ctx.contains("detailed") {
    ///         "Provide a thorough analysis with citations.".to_string()
    ///     } else {
    ///         "Be concise.".to_string()
    ///     }
    /// });
    /// ```
    pub fn adapt<F>(f: F) -> PromptSection
    where
        F: Fn(&str) -> String + Send + Sync + 'static,
    {
        PromptSection {
            kind: PromptSectionKind::Adaptive,
            content: String::new(),
            name: Some("adapt".to_string()),
            adapter: None,
        }
        .with_adapter(f)
    }

    /// Create a step-by-step scaffolded prompt from ordered steps.
    ///
    /// ```ignore
    /// let prompt = P::scaffolded(&["Identify the problem", "Gather data", "Analyze", "Conclude"]);
    /// ```
    pub fn scaffolded(steps: &[&str]) -> PromptSection {
        let content = steps
            .iter()
            .enumerate()
            .map(|(i, step)| format!("Step {}: {step}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        PromptSection {
            kind: PromptSectionKind::Scaffolded,
            content: format!("Follow these steps:\n{content}"),
            name: Some("scaffolded".to_string()),
            adapter: None,
        }
    }

    /// Create a versioned prompt section with a version tag.
    ///
    /// ```ignore
    /// let prompt = P::versioned("v2.1", "Analyze the data using the new methodology");
    /// ```
    pub fn versioned(version: &str, text: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Versioned,
            content: format!("[{version}] {text}"),
            name: Some(format!("versioned:{version}")),
            adapter: None,
        }
    }

    // ── Instruction modifier factories ──────────────────────────────────────
    // Bridge P-module composition to the InstructionModifier system.

    /// Create a state-append modifier that renders selected state keys into the instruction.
    ///
    /// ```ignore
    /// let modifiers = P::with_state(&["emotional_state", "willingness_to_pay"]);
    /// ```
    pub fn with_state(keys: &[&str]) -> gemini_adk::live::InstructionModifier {
        gemini_adk::live::InstructionModifier::StateAppend(
            keys.iter().map(|k| k.to_string()).collect(),
        )
    }

    /// Create a conditional modifier that appends text when the predicate is true.
    ///
    /// ```ignore
    /// let risk_mod = P::when(risk_is_elevated, "IMPORTANT: Show extra empathy.");
    /// ```
    pub fn when(
        predicate: impl Fn(&gemini_adk::State) -> bool + Send + Sync + 'static,
        text: impl Into<String>,
    ) -> gemini_adk::live::InstructionModifier {
        gemini_adk::live::InstructionModifier::Conditional {
            predicate: std::sync::Arc::new(predicate),
            text: text.into(),
        }
    }

    /// Create a custom-append modifier from a formatting function.
    ///
    /// ```ignore
    /// let ctx = P::context_fn(|s| format!("Customer: {}", s.get::<String>("name").unwrap_or_default()));
    /// ```
    pub fn context_fn(
        f: impl Fn(&gemini_adk::State) -> String + Send + Sync + 'static,
    ) -> gemini_adk::live::InstructionModifier {
        gemini_adk::live::InstructionModifier::CustomAppend(std::sync::Arc::new(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_renders() {
        let s = P::role("analyst");
        assert_eq!(s.render(), "You are analyst.");
    }

    #[test]
    fn task_renders() {
        let s = P::task("analyze data");
        assert_eq!(s.render(), "Your task: analyze data");
    }

    #[test]
    fn constraint_renders() {
        let s = P::constraint("be concise");
        assert_eq!(s.render(), "Constraint: be concise");
    }

    #[test]
    fn format_renders() {
        let s = P::format("JSON");
        assert_eq!(s.render(), "Output format: JSON");
    }

    #[test]
    fn example_renders() {
        let s = P::example("hello", "world");
        assert!(s.render().contains("Input: hello"));
        assert!(s.render().contains("Output: world"));
    }

    #[test]
    fn compose_with_add() {
        let prompt = P::role("analyst") + P::task("analyze data") + P::format("JSON");
        assert_eq!(prompt.sections.len(), 3);
    }

    #[test]
    fn composite_renders_all() {
        let prompt = P::role("analyst") + P::task("analyze data");
        let rendered = prompt.render();
        assert!(rendered.contains("You are analyst."));
        assert!(rendered.contains("Your task: analyze data"));
    }

    #[test]
    fn context_renders() {
        let s = P::context("user is a developer");
        assert_eq!(s.render(), "Context: user is a developer");
        assert_eq!(s.kind, PromptSectionKind::Context);
    }

    #[test]
    fn persona_renders() {
        let s = P::persona("friendly and concise");
        assert_eq!(s.render(), "Persona: friendly and concise");
        assert_eq!(s.kind, PromptSectionKind::Persona);
    }

    #[test]
    fn guidelines_renders() {
        let s = P::guidelines(&["be concise", "use examples", "cite sources"]);
        assert!(s.render().contains("Guidelines:"));
        assert!(s.render().contains("- be concise"));
        assert!(s.render().contains("- use examples"));
        assert!(s.render().contains("- cite sources"));
        assert_eq!(s.kind, PromptSectionKind::Guidelines);
    }

    #[test]
    fn section_kinds() {
        assert_eq!(P::role("x").kind, PromptSectionKind::Role);
        assert_eq!(P::task("x").kind, PromptSectionKind::Task);
        assert_eq!(P::text("x").kind, PromptSectionKind::Text);
    }

    #[test]
    fn section_into_string() {
        let s: String = P::role("analyst").into();
        assert_eq!(s, "You are analyst.");
    }

    #[test]
    fn composite_into_string() {
        let s: String = (P::role("analyst") + P::task("analyze data")).into();
        assert!(s.contains("You are analyst."));
        assert!(s.contains("Your task: analyze data"));
    }

    #[test]
    fn reorder_by_name() {
        let prompt = P::role("analyst") + P::task("analyze") + P::format("JSON");
        let reordered = prompt.reorder_by_name(&["format", "task", "role"]);
        assert_eq!(reordered.sections[0].name.as_deref(), Some("format"));
        assert_eq!(reordered.sections[1].name.as_deref(), Some("task"));
        assert_eq!(reordered.sections[2].name.as_deref(), Some("role"));
    }

    #[test]
    fn only_by_name() {
        let prompt = P::role("analyst") + P::task("analyze") + P::format("JSON");
        let filtered = prompt.only_by_name(&["role", "task"]);
        assert_eq!(filtered.sections.len(), 2);
        assert_eq!(filtered.sections[0].name.as_deref(), Some("role"));
        assert_eq!(filtered.sections[1].name.as_deref(), Some("task"));
    }

    #[test]
    fn without_by_name() {
        let prompt = P::role("analyst") + P::task("analyze") + P::format("JSON");
        let filtered = prompt.without_by_name(&["format"]);
        assert_eq!(filtered.sections.len(), 2);
        assert!(filtered
            .sections
            .iter()
            .all(|s| s.name.as_deref() != Some("format")));
    }

    #[test]
    fn reorder_transform_via_apply() {
        let prompt = P::role("analyst") + P::task("analyze") + P::format("JSON");
        let transform = P::reorder(&["format", "role"]);
        let reordered = prompt.apply(transform);
        assert_eq!(reordered.sections[0].name.as_deref(), Some("format"));
        assert_eq!(reordered.sections[1].name.as_deref(), Some("role"));
    }

    #[test]
    fn only_transform_via_apply() {
        let prompt = P::role("analyst") + P::task("analyze") + P::format("JSON");
        let transform = P::only(&["task"]);
        let filtered = prompt.apply(transform);
        assert_eq!(filtered.sections.len(), 1);
        assert_eq!(filtered.sections[0].name.as_deref(), Some("task"));
    }

    #[test]
    fn without_transform_via_apply() {
        let prompt = P::role("analyst") + P::task("analyze") + P::format("JSON");
        let transform = P::without(&["role", "format"]);
        let filtered = prompt.apply(transform);
        assert_eq!(filtered.sections.len(), 1);
        assert_eq!(filtered.sections[0].name.as_deref(), Some("task"));
    }

    #[test]
    fn compress_renders() {
        let s = P::compress();
        assert_eq!(s.kind, PromptSectionKind::Compressed);
        assert_eq!(s.render(), "[compressed] ");
    }

    #[test]
    fn adapt_renders_with_context() {
        let s = P::adapt(|ctx| {
            if ctx.contains("detailed") {
                "Be thorough.".to_string()
            } else {
                "Be concise.".to_string()
            }
        });
        assert_eq!(s.kind, PromptSectionKind::Adaptive);
        assert_eq!(s.render_with_context("detailed"), "Be thorough.");
        assert_eq!(s.render_with_context("brief"), "Be concise.");
    }

    #[test]
    fn adapt_fallback_render() {
        let s = P::adapt(|_| "adapted".to_string());
        // render() without context returns the empty content
        assert_eq!(s.render(), "");
    }

    #[test]
    fn scaffolded_renders() {
        let s = P::scaffolded(&["Identify", "Analyze", "Conclude"]);
        assert_eq!(s.kind, PromptSectionKind::Scaffolded);
        let rendered = s.render();
        assert!(rendered.contains("Follow these steps:"));
        assert!(rendered.contains("Step 1: Identify"));
        assert!(rendered.contains("Step 2: Analyze"));
        assert!(rendered.contains("Step 3: Conclude"));
    }

    #[test]
    fn versioned_renders() {
        let s = P::versioned("v2.1", "Use the new methodology");
        assert_eq!(s.kind, PromptSectionKind::Versioned);
        assert_eq!(s.render(), "[v2.1] Use the new methodology");
        assert_eq!(s.name.as_deref(), Some("versioned:v2.1"));
    }

    #[test]
    fn sections_have_names() {
        assert_eq!(P::role("x").name.as_deref(), Some("role"));
        assert_eq!(P::task("x").name.as_deref(), Some("task"));
        assert_eq!(P::constraint("x").name.as_deref(), Some("constraint"));
        assert_eq!(P::format("x").name.as_deref(), Some("format"));
        assert_eq!(P::example("x", "y").name.as_deref(), Some("example"));
        assert_eq!(P::text("x").name, None);
        assert_eq!(P::context("x").name.as_deref(), Some("context"));
        assert_eq!(P::persona("x").name.as_deref(), Some("persona"));
        assert_eq!(P::guidelines(&["x"]).name.as_deref(), Some("guidelines"));
        assert_eq!(P::section("foo", "bar").name.as_deref(), Some("foo"));
        assert_eq!(P::scaffolded(&["x"]).name.as_deref(), Some("scaffolded"));
        assert_eq!(P::compress().name.as_deref(), Some("compress"));
    }
}
