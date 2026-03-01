//! Prompt engineering for Gemini Live sessions.
//!
//! Inspired by ADK's `P` (Prompt) namespace. System instructions in Gemini Live
//! are sent once at WebSocket setup time via the `systemInstruction` field.
//! This module provides structured composition, conditional sections, and
//! template interpolation for building high-quality system prompts.
//!
//! # Architecture
//!
//! Unlike request-response agents where prompts can change per request,
//! Gemini Live system instructions are set once per session. This module
//! makes that single prompt maximally effective by:
//!
//! 1. **Structured sections** with consistent ordering (Role → Context → Task → Constraint → Format → Example)
//! 2. **Conditional sections** included based on runtime state
//! 3. **Template interpolation** with `{key}` placeholders
//! 4. **Versioning** for A/B testing and prompt iteration
//!
//! # Example
//!
//! ```rust
//! use gemini_live_rs::prompt::*;
//!
//! let prompt = SystemPrompt::builder()
//!     .role("You are a customer service agent for TechCorp.")
//!     .task("Help customers with billing inquiries and technical support.")
//!     .constraint("Never share internal pricing formulas.")
//!     .constraint("Always verify customer identity before sharing account details.")
//!     .format("Respond conversationally. Use short sentences for speech clarity.")
//!     .example(
//!         "Customer: What's my balance?",
//!         "I'd be happy to check that for you. Could you verify your account number first?",
//!     )
//!     .build();
//!
//! let rendered = prompt.render_static();
//! assert!(rendered.contains("TechCorp"));
//! ```

use std::collections::HashMap;
use std::fmt;

// Type alias to reduce complexity in ConditionalPredicate
type PredicateFn = std::sync::Arc<dyn Fn(&HashMap<String, serde_json::Value>) -> bool + Send + Sync>;

// ---------------------------------------------------------------------------
// Section kinds
// ---------------------------------------------------------------------------

/// The kind of a prompt section, determining its render order.
///
/// Default ordering: Role → Context → Task → Constraint → Format → Example → Custom
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SectionKind {
    /// Agent persona and identity (rendered without a header).
    Role,
    /// Background information the agent should know.
    Context,
    /// The primary objective or task.
    Task,
    /// Rules and guardrails the agent must follow.
    Constraint,
    /// Output format specifications (especially important for speech).
    Format,
    /// Few-shot examples with input/output pairs.
    Example,
    /// Named custom section.
    Custom(String),
}

impl SectionKind {
    /// Default sort order for section kinds.
    fn order(&self) -> u32 {
        match self {
            Self::Role => 0,
            Self::Context => 1,
            Self::Task => 2,
            Self::Constraint => 3,
            Self::Format => 4,
            Self::Example => 5,
            Self::Custom(_) => 6,
        }
    }

    /// Section header for rendering. Role has no header (renders inline).
    fn header(&self) -> Option<&str> {
        match self {
            Self::Role => None,
            Self::Context => Some("Context"),
            Self::Task => Some("Task"),
            Self::Constraint => Some("Constraints"),
            Self::Format => Some("Format"),
            Self::Example => Some("Examples"),
            Self::Custom(name) => Some(name.as_str()),
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt section
// ---------------------------------------------------------------------------

/// A single section of a system prompt.
#[derive(Clone)]
pub struct PromptSection {
    /// What kind of section this is.
    pub kind: SectionKind,
    /// The section content.
    pub content: SectionContent,
    /// Custom sort order override (lower = earlier).
    pub order_override: Option<u32>,
}

/// Content of a prompt section.
#[derive(Clone)]
pub enum SectionContent {
    /// Static text content.
    Static(String),
    /// Template with `{key}` placeholders resolved from state at render time.
    Template(String),
    /// Conditional section — included only if predicate returns true.
    Conditional {
        /// The predicate function.
        predicate: ConditionalPredicate,
        /// The content to include if predicate is true.
        content: Box<SectionContent>,
    },
}

/// A predicate for conditional prompt sections.
#[derive(Clone)]
pub enum ConditionalPredicate {
    /// Include if a state key is truthy (exists and is not false/null/empty).
    StateKeyTruthy(String),
    /// Include if a state key equals a specific value.
    StateKeyEquals(String, serde_json::Value),
    /// Custom predicate function.
    Custom(PredicateFn),
}

impl ConditionalPredicate {
    /// Evaluate the predicate against state.
    pub fn evaluate(&self, state: &HashMap<String, serde_json::Value>) -> bool {
        match self {
            Self::StateKeyTruthy(key) => {
                state.get(key).is_some_and(|v| {
                    !v.is_null()
                        && v != &serde_json::Value::Bool(false)
                        && v != &serde_json::Value::String(String::new())
                })
            }
            Self::StateKeyEquals(key, expected) => {
                state.get(key).is_some_and(|v| v == expected)
            }
            Self::Custom(f) => f(state),
        }
    }
}

impl fmt::Debug for PromptSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptSection")
            .field("kind", &self.kind)
            .field("order_override", &self.order_override)
            .finish()
    }
}

impl SectionContent {
    /// Resolve this content to a string, interpolating templates and evaluating conditions.
    pub fn resolve(&self, state: &HashMap<String, serde_json::Value>) -> Option<String> {
        match self {
            Self::Static(text) => Some(text.clone()),
            Self::Template(template) => {
                let mut result = template.clone();
                for (k, v) in state {
                    let placeholder = format!("{{{k}}}");
                    let replacement = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    result = result.replace(&placeholder, &replacement);
                }
                // Handle optional placeholders: {key?} → empty string if missing
                loop {
                    if let Some(start) = result.find('{') {
                        if let Some(end) = result[start..].find('}') {
                            let key = &result[start + 1..start + end];
                            if key.ends_with('?') {
                                // Optional placeholder — remove it
                                result = format!(
                                    "{}{}",
                                    &result[..start],
                                    &result[start + end + 1..]
                                );
                                continue;
                            }
                        }
                    }
                    break;
                }
                Some(result)
            }
            Self::Conditional {
                predicate,
                content,
            } => {
                if predicate.evaluate(state) {
                    content.resolve(state)
                } else {
                    None
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// System prompt
// ---------------------------------------------------------------------------

/// A structured system prompt composed of ordered sections.
///
/// Sections are rendered in a consistent order (Role → Context → Task →
/// Constraint → Format → Example → Custom), with each section separated
/// by a blank line. Constraints are merged into a single bulleted list.
///
/// # Speech-optimized formatting
///
/// For Gemini Live speech-to-speech, the prompt format matters:
/// - Short, clear sentences work better for voice
/// - Explicit turn-taking instructions improve conversation flow
/// - Format sections should specify "conversational" style
#[derive(Clone)]
pub struct SystemPrompt {
    sections: Vec<PromptSection>,
    /// Optional version tag for A/B testing.
    pub version: Option<String>,
}

impl fmt::Debug for SystemPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemPrompt")
            .field("sections", &self.sections.len())
            .field("version", &self.version)
            .finish()
    }
}

impl SystemPrompt {
    /// Create a new builder.
    pub fn builder() -> SystemPromptBuilder {
        SystemPromptBuilder::new()
    }

    /// Render the prompt to a string without any state interpolation.
    pub fn render_static(&self) -> String {
        self.render(&HashMap::new())
    }

    /// Render the prompt to a string, evaluating conditions and templates.
    pub fn render(&self, state: &HashMap<String, serde_json::Value>) -> String {
        let mut sorted = self.sections.clone();
        sorted.sort_by_key(|s| s.order_override.unwrap_or(s.kind.order()));

        let mut output = String::new();
        let mut constraints: Vec<String> = Vec::new();

        for section in &sorted {
            let resolved = match section.content.resolve(state) {
                Some(text) if !text.is_empty() => text,
                _ => continue,
            };

            // Merge constraints into a single bulleted list
            if section.kind == SectionKind::Constraint {
                constraints.push(resolved);
                continue;
            }

            if !output.is_empty() {
                output.push_str("\n\n");
            }

            match section.kind.header() {
                Some(header) => {
                    // Check if we need to flush constraints before a later section
                    if section.kind.order() > SectionKind::Constraint.order()
                        && !constraints.is_empty()
                    {
                        output.push_str("## Constraints\n");
                        for c in &constraints {
                            output.push_str(&format!("- {c}\n"));
                        }
                        constraints.clear();
                        output.push('\n');
                    }
                    output.push_str(&format!("## {header}\n{resolved}"));
                }
                None => {
                    // Role: no header
                    output.push_str(&resolved);
                }
            }
        }

        // Flush remaining constraints
        if !constraints.is_empty() {
            if !output.is_empty() {
                output.push_str("\n\n");
            }
            output.push_str("## Constraints\n");
            for c in &constraints {
                output.push_str(&format!("- {c}\n"));
            }
        }

        output.trim_end().to_string()
    }

    /// Convert to a `Content` for use as `system_instruction` in [`SessionConfig`].
    pub fn to_content(&self) -> crate::protocol::Content {
        crate::protocol::Content {
            role: None,
            parts: vec![crate::protocol::Part::Text {
                text: self.render_static(),
            }],
        }
    }

    /// Convert to a `Content` with state interpolation.
    pub fn to_content_with_state(
        &self,
        state: &HashMap<String, serde_json::Value>,
    ) -> crate::protocol::Content {
        crate::protocol::Content {
            role: None,
            parts: vec![crate::protocol::Part::Text {
                text: self.render(state),
            }],
        }
    }
}

// ---------------------------------------------------------------------------
// System prompt builder
// ---------------------------------------------------------------------------

/// Builder for constructing a [`SystemPrompt`] with fluent method chaining.
pub struct SystemPromptBuilder {
    sections: Vec<PromptSection>,
    version: Option<String>,
}

impl SystemPromptBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            version: None,
        }
    }

    /// Add a role section (agent persona, rendered without a header).
    pub fn role(mut self, text: impl Into<String>) -> Self {
        self.sections.push(PromptSection {
            kind: SectionKind::Role,
            content: SectionContent::Static(text.into()),
            order_override: None,
        });
        self
    }

    /// Add a context section (background information).
    pub fn context(mut self, text: impl Into<String>) -> Self {
        self.sections.push(PromptSection {
            kind: SectionKind::Context,
            content: SectionContent::Static(text.into()),
            order_override: None,
        });
        self
    }

    /// Add a task section (primary objective).
    pub fn task(mut self, text: impl Into<String>) -> Self {
        self.sections.push(PromptSection {
            kind: SectionKind::Task,
            content: SectionContent::Static(text.into()),
            order_override: None,
        });
        self
    }

    /// Add a constraint (rule/guardrail). Multiple constraints are merged.
    pub fn constraint(mut self, text: impl Into<String>) -> Self {
        self.sections.push(PromptSection {
            kind: SectionKind::Constraint,
            content: SectionContent::Static(text.into()),
            order_override: None,
        });
        self
    }

    /// Add a format section (output style, especially important for voice).
    pub fn format(mut self, text: impl Into<String>) -> Self {
        self.sections.push(PromptSection {
            kind: SectionKind::Format,
            content: SectionContent::Static(text.into()),
            order_override: None,
        });
        self
    }

    /// Add a few-shot example with input/output pair.
    pub fn example(mut self, input: impl Into<String>, output: impl Into<String>) -> Self {
        let text = format!(
            "Input: {}\nOutput: {}",
            input.into(),
            output.into()
        );
        self.sections.push(PromptSection {
            kind: SectionKind::Example,
            content: SectionContent::Static(text),
            order_override: None,
        });
        self
    }

    /// Add a named custom section.
    pub fn section(mut self, name: impl Into<String>, content: impl Into<String>) -> Self {
        self.sections.push(PromptSection {
            kind: SectionKind::Custom(name.into()),
            content: SectionContent::Static(content.into()),
            order_override: None,
        });
        self
    }

    /// Add a template section with `{key}` placeholders resolved from state.
    pub fn template(mut self, kind: SectionKind, template: impl Into<String>) -> Self {
        self.sections.push(PromptSection {
            kind,
            content: SectionContent::Template(template.into()),
            order_override: None,
        });
        self
    }

    /// Add a conditional section included only when a state key is truthy.
    pub fn when_state(
        mut self,
        key: impl Into<String>,
        kind: SectionKind,
        content: impl Into<String>,
    ) -> Self {
        self.sections.push(PromptSection {
            kind,
            content: SectionContent::Conditional {
                predicate: ConditionalPredicate::StateKeyTruthy(key.into()),
                content: Box::new(SectionContent::Static(content.into())),
            },
            order_override: None,
        });
        self
    }

    /// Add a conditional section included only when a state key equals a value.
    pub fn when_state_eq(
        mut self,
        key: impl Into<String>,
        value: serde_json::Value,
        kind: SectionKind,
        content: impl Into<String>,
    ) -> Self {
        self.sections.push(PromptSection {
            kind,
            content: SectionContent::Conditional {
                predicate: ConditionalPredicate::StateKeyEquals(key.into(), value),
                content: Box::new(SectionContent::Static(content.into())),
            },
            order_override: None,
        });
        self
    }

    /// Add a section with a custom predicate.
    pub fn when<F>(mut self, predicate: F, kind: SectionKind, content: impl Into<String>) -> Self
    where
        F: Fn(&HashMap<String, serde_json::Value>) -> bool + Send + Sync + 'static,
    {
        self.sections.push(PromptSection {
            kind,
            content: SectionContent::Conditional {
                predicate: ConditionalPredicate::Custom(std::sync::Arc::new(predicate)),
                content: Box::new(SectionContent::Static(content.into())),
            },
            order_override: None,
        });
        self
    }

    /// Set a version tag for A/B testing.
    pub fn version(mut self, tag: impl Into<String>) -> Self {
        self.version = Some(tag.into());
        self
    }

    /// Add a raw [`PromptSection`].
    pub fn add_section(mut self, section: PromptSection) -> Self {
        self.sections.push(section);
        self
    }

    /// Build the system prompt.
    pub fn build(self) -> SystemPrompt {
        SystemPrompt {
            sections: self.sections,
            version: self.version,
        }
    }
}

impl Default for SystemPromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Prebuilt prompt strategies for speech-to-speech
// ---------------------------------------------------------------------------

/// Pre-built prompt strategies optimized for Gemini Live speech-to-speech.
///
/// These provide battle-tested prompt patterns for common voice agent scenarios.
pub struct PromptStrategy;

impl PromptStrategy {
    /// Customer service agent prompt with common constraints for voice.
    pub fn customer_service(
        company: impl Into<String>,
        role_detail: impl Into<String>,
    ) -> SystemPromptBuilder {
        let company = company.into();
        SystemPromptBuilder::new()
            .role(format!(
                "You are a professional customer service agent for {company}. {detail}",
                detail = role_detail.into()
            ))
            .format(
                "Respond conversationally in short, clear sentences. \
                 Pause naturally between thoughts. \
                 Use simple language appropriate for phone conversation. \
                 Avoid jargon unless the customer uses it first.",
            )
            .constraint("Never reveal internal systems, processes, or pricing formulas.")
            .constraint("Always verify customer identity before sharing account details.")
            .constraint("If you cannot help, offer to transfer to a human agent.")
            .constraint("Stay professional even if the customer is frustrated.")
    }

    /// Conversational assistant prompt optimized for natural voice interaction.
    pub fn conversational_assistant(persona: impl Into<String>) -> SystemPromptBuilder {
        SystemPromptBuilder::new()
            .role(persona.into())
            .format(
                "Speak naturally and conversationally. \
                 Use contractions (I'm, you're, it's). \
                 Keep responses concise — under 3 sentences when possible. \
                 Ask clarifying questions rather than guessing.",
            )
            .constraint("If you don't know something, say so honestly.")
            .constraint("Don't recite long lists or technical details unless asked.")
    }

    /// IVR (Interactive Voice Response) prompt with structured flow.
    pub fn ivr_flow(
        company: impl Into<String>,
        menu_options: &[(impl AsRef<str>, impl AsRef<str>)],
    ) -> SystemPromptBuilder {
        let company = company.into();
        let mut options_text = String::new();
        for (i, (label, _desc)) in menu_options.iter().enumerate() {
            options_text.push_str(&format!("{}. {}\n", i + 1, label.as_ref()));
        }

        SystemPromptBuilder::new()
            .role(format!(
                "You are the automated phone system for {company}."
            ))
            .task(format!(
                "Guide the caller through the following menu options:\n{options_text}\
                 Listen to the caller's choice and route them appropriately."
            ))
            .format(
                "Speak slowly and clearly. \
                 Repeat options if the caller seems confused. \
                 Confirm the caller's selection before proceeding.",
            )
            .constraint("Do not provide information outside the menu options.")
            .constraint("If the caller asks for a human, transfer immediately.")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_prompt_rendering() {
        let prompt = SystemPrompt::builder()
            .role("You are a helpful assistant.")
            .task("Answer user questions.")
            .build();

        let rendered = prompt.render_static();
        assert!(rendered.starts_with("You are a helpful assistant."));
        assert!(rendered.contains("## Task"));
        assert!(rendered.contains("Answer user questions."));
    }

    #[test]
    fn constraints_merged() {
        let prompt = SystemPrompt::builder()
            .role("Agent.")
            .constraint("Be concise.")
            .constraint("Be accurate.")
            .constraint("Be kind.")
            .build();

        let rendered = prompt.render_static();
        assert!(rendered.contains("## Constraints"));
        assert!(rendered.contains("- Be concise."));
        assert!(rendered.contains("- Be accurate."));
        assert!(rendered.contains("- Be kind."));
    }

    #[test]
    fn section_ordering() {
        let prompt = SystemPrompt::builder()
            .constraint("Rule 1.")
            .task("Do the thing.")
            .role("I am the agent.")
            .format("Use JSON.")
            .build();

        let rendered = prompt.render_static();
        let role_pos = rendered.find("I am the agent.").unwrap();
        let task_pos = rendered.find("## Task").unwrap();
        let format_pos = rendered.find("## Format").unwrap();

        assert!(role_pos < task_pos, "Role should come before Task");
        assert!(task_pos < format_pos, "Task should come before Format");
    }

    #[test]
    fn conditional_section_included() {
        let prompt = SystemPrompt::builder()
            .role("Agent.")
            .when_state(
                "is_vip",
                SectionKind::Context,
                "This customer is a VIP. Provide premium service.",
            )
            .build();

        // With state: is_vip = true
        let mut state = HashMap::new();
        state.insert("is_vip".to_string(), serde_json::json!(true));
        let rendered = prompt.render(&state);
        assert!(rendered.contains("VIP"));

        // Without state: is_vip not set
        let rendered = prompt.render(&HashMap::new());
        assert!(!rendered.contains("VIP"));
    }

    #[test]
    fn conditional_state_eq() {
        let prompt = SystemPrompt::builder()
            .role("Agent.")
            .when_state_eq(
                "tier",
                serde_json::json!("gold"),
                SectionKind::Context,
                "Gold tier: offer 20% discount.",
            )
            .build();

        let mut state = HashMap::new();
        state.insert("tier".to_string(), serde_json::json!("gold"));
        assert!(prompt.render(&state).contains("Gold tier"));

        state.insert("tier".to_string(), serde_json::json!("silver"));
        assert!(!prompt.render(&state).contains("Gold tier"));
    }

    #[test]
    fn template_interpolation() {
        let prompt = SystemPrompt::builder()
            .role("Agent for {company}.")
            .template(SectionKind::Role, "Serving {company} customers.")
            .build();

        let mut state = HashMap::new();
        state.insert("company".to_string(), serde_json::json!("Acme"));
        let rendered = prompt.render(&state);
        assert!(rendered.contains("Serving Acme customers."));
    }

    #[test]
    fn examples_rendered() {
        let prompt = SystemPrompt::builder()
            .role("Agent.")
            .example("Hi", "Hello! How can I help?")
            .example("What's the price?", "Let me check that for you.")
            .build();

        let rendered = prompt.render_static();
        assert!(rendered.contains("## Examples"));
        assert!(rendered.contains("Input: Hi"));
        assert!(rendered.contains("Output: Hello! How can I help?"));
    }

    #[test]
    fn custom_section() {
        let prompt = SystemPrompt::builder()
            .role("Agent.")
            .section("Safety Guidelines", "Always prioritize user safety.")
            .build();

        let rendered = prompt.render_static();
        assert!(rendered.contains("## Safety Guidelines"));
        assert!(rendered.contains("Always prioritize user safety."));
    }

    #[test]
    fn version_tag() {
        let prompt = SystemPrompt::builder()
            .role("Agent.")
            .version("v2.1-beta")
            .build();

        assert_eq!(prompt.version, Some("v2.1-beta".to_string()));
    }

    #[test]
    fn to_content_conversion() {
        let prompt = SystemPrompt::builder()
            .role("You are a helpful assistant.")
            .task("Help with questions.")
            .build();

        let content = prompt.to_content();
        assert!(content.role.is_none());
        assert_eq!(content.parts.len(), 1);
        match &content.parts[0] {
            crate::protocol::Part::Text { text } => {
                assert!(text.contains("helpful assistant"));
            }
            _ => panic!("Expected text part"),
        }
    }

    #[test]
    fn customer_service_strategy() {
        let builder = PromptStrategy::customer_service("TechCorp", "Handle billing inquiries.");
        let prompt = builder.build();
        let rendered = prompt.render_static();
        assert!(rendered.contains("TechCorp"));
        assert!(rendered.contains("billing"));
        assert!(rendered.contains("## Constraints"));
    }

    #[test]
    fn conversational_assistant_strategy() {
        let builder = PromptStrategy::conversational_assistant("You are Max, a friendly AI.");
        let prompt = builder.build();
        let rendered = prompt.render_static();
        assert!(rendered.contains("Max"));
        assert!(rendered.contains("conversationally"));
    }

    #[test]
    fn empty_conditional_excluded() {
        let prompt = SystemPrompt::builder()
            .role("Agent.")
            .when_state("nonexistent", SectionKind::Task, "This should not appear.")
            .build();

        let rendered = prompt.render_static();
        assert!(!rendered.contains("This should not appear."));
        assert!(!rendered.contains("## Task"));
    }

    #[test]
    fn optional_template_placeholders() {
        let content = SectionContent::Template("Hello {name?}, welcome!".to_string());
        let result = content.resolve(&HashMap::new());
        assert_eq!(result, Some("Hello , welcome!".to_string()));
    }
}
