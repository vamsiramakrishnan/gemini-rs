//! P — Prompt composition.
//!
//! Compose prompt sections additively with `+`.

/// A section of a prompt.
#[derive(Clone, Debug)]
pub struct PromptSection {
    /// The semantic category of this section.
    pub kind: PromptSectionKind,
    /// The text content of this section.
    pub content: String,
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
        }
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
        }
    }

    /// Define the agent's task.
    pub fn task(task: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Task,
            content: task.to_string(),
        }
    }

    /// Add a constraint.
    pub fn constraint(c: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Constraint,
            content: c.to_string(),
        }
    }

    /// Specify output format.
    pub fn format(f: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Format,
            content: f.to_string(),
        }
    }

    /// Add an input/output example.
    pub fn example(input: &str, output: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Example,
            content: format!("Example:\nInput: {input}\nOutput: {output}"),
        }
    }

    /// Add free-form text.
    pub fn text(t: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Text,
            content: t.to_string(),
        }
    }

    /// Add background context.
    pub fn context(ctx: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Context,
            content: ctx.to_string(),
        }
    }

    /// Define a personality/persona.
    pub fn persona(desc: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Persona,
            content: desc.to_string(),
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
        }
    }

    /// Add a named section (flexible section kind).
    pub fn section(name: &str, text: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Text,
            content: format!("## {}\n{}", name, text),
        }
    }

    /// Template with `{key}` placeholders — rendered with state values at runtime.
    pub fn template(tpl: &str) -> PromptSection {
        PromptSection {
            kind: PromptSectionKind::Text,
            content: tpl.to_string(),
        }
    }

    // ── Instruction modifier factories ──────────────────────────────────────
    // Bridge P-module composition to the InstructionModifier system.

    /// Create a state-append modifier that renders selected state keys into the instruction.
    ///
    /// ```ignore
    /// let modifiers = P::with_state(&["emotional_state", "willingness_to_pay"]);
    /// ```
    pub fn with_state(keys: &[&str]) -> rs_adk::live::InstructionModifier {
        rs_adk::live::InstructionModifier::StateAppend(keys.iter().map(|k| k.to_string()).collect())
    }

    /// Create a conditional modifier that appends text when the predicate is true.
    ///
    /// ```ignore
    /// let risk_mod = P::when(risk_is_elevated, "IMPORTANT: Show extra empathy.");
    /// ```
    pub fn when(
        predicate: impl Fn(&rs_adk::State) -> bool + Send + Sync + 'static,
        text: impl Into<String>,
    ) -> rs_adk::live::InstructionModifier {
        rs_adk::live::InstructionModifier::Conditional {
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
        f: impl Fn(&rs_adk::State) -> String + Send + Sync + 'static,
    ) -> rs_adk::live::InstructionModifier {
        rs_adk::live::InstructionModifier::CustomAppend(std::sync::Arc::new(f))
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
}
