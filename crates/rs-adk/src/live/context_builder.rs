//! Declarative state-to-narrative context builder.
//!
//! A [`ContextBuilder`] renders session [`State`] into a natural-language summary
//! that gets appended to the phase instruction via [`InstructionModifier::CustomAppend`].
//! It replaces hand-written `fn app_context(s: &State) -> String` closures with a
//! declarative, composable API.
//!
//! # How it works
//!
//! 1. Declare **sections** — named groups of related state keys
//! 2. Each section has **fields** — state keys with display labels and render modes
//! 3. The builder checks each key in state, skips missing values, formats present ones
//! 4. If the current phase has [`needs`](super::Phase::needs) metadata, appends a
//!    "Gathering:" line for missing keys so the model knows what to focus on
//!
//! # Output format
//!
//! ```text
//! [Caller] Name: Bob. Organization: Google. Known contact.
//! [Call] Purpose: schedule meeting. Urgency: high (0.8).
//! [Gathering] caller_organization
//! ```
//!
//! Empty sections are omitted. When no state has been gathered, returns an empty
//! string (no noise in the instruction).
//!
//! # Example
//!
//! ```ignore
//! use rs_adk::live::context_builder::ContextBuilder;
//!
//! let ctx = ContextBuilder::new()
//!     .section("Caller")
//!         .field("caller_name", "Name")
//!         .field("caller_organization", "Organization")
//!         .flag("is_known_contact", "Known contact")
//!     .section("Call")
//!         .field("call_purpose", "Purpose")
//!         .sentiment("caller_sentiment")
//!     .build();
//!
//! // Use with phase_defaults:
//! // .phase_defaults(|d| d.with_context(ctx))
//! ```

use std::sync::Arc;

use serde_json::Value;

use crate::state::State;

// ── Field rendering modes ──────────────────────────────────────────────────

/// How a field renders its value from state.
#[derive(Clone)]
enum FieldKind {
    /// Render as "Label: {value}."
    Value,
    /// Bool flag — show label when true, omit when false.
    Flag,
    /// Sentiment — renders as "Caller seems {value}." when non-neutral.
    Sentiment,
    /// Custom formatter — receives the raw JSON value.
    Format(Arc<dyn Fn(&Value) -> String + Send + Sync>),
}

/// A single state key with its display label and render mode.
#[derive(Clone)]
struct Field {
    key: String,
    label: String,
    kind: FieldKind,
}

impl Field {
    fn render(&self, state: &State) -> Option<String> {
        let val: Option<Value> = state.get(&self.key);
        match &self.kind {
            FieldKind::Value => {
                let val = val?;
                match &val {
                    Value::String(s) if s.is_empty() => None,
                    Value::String(s) => Some(format!("{}: {s}.", self.label)),
                    Value::Number(n) => Some(format!("{}: {n}.", self.label)),
                    Value::Bool(b) => Some(format!("{}: {b}.", self.label)),
                    Value::Null => None,
                    other => Some(format!("{}: {other}.", self.label)),
                }
            }
            FieldKind::Flag => {
                let val = val?;
                if val.as_bool().unwrap_or(false) {
                    Some(format!("{}.", self.label))
                } else {
                    None
                }
            }
            FieldKind::Sentiment => {
                let val = val?;
                let s = val.as_str()?;
                if s.is_empty() || s == "neutral" || s == "unknown" {
                    None
                } else {
                    Some(format!("Caller seems {s}."))
                }
            }
            FieldKind::Format(f) => {
                let val = val?;
                let rendered = f(&val);
                if rendered.is_empty() {
                    None
                } else {
                    Some(rendered)
                }
            }
        }
    }
}

// ── Section ────────────────────────────────────────────────────────────────

/// A named group of related state fields.
#[derive(Clone)]
struct Section {
    label: String,
    fields: Vec<Field>,
}

impl Section {
    fn render(&self, state: &State) -> Option<String> {
        let parts: Vec<String> = self.fields.iter().filter_map(|f| f.render(state)).collect();
        if parts.is_empty() {
            None
        } else {
            Some(format!("[{}] {}", self.label, parts.join(" ")))
        }
    }
}

// ── SectionBuilder ─────────────────────────────────────────────────────────

/// Fluent builder for a single section.
pub struct SectionBuilder {
    label: String,
    fields: Vec<Field>,
    parent_sections: Vec<Section>,
}

impl SectionBuilder {
    /// Add a value field — renders as "Label: {value}." when the key exists.
    pub fn field(mut self, key: &str, label: &str) -> Self {
        self.fields.push(Field {
            key: key.into(),
            label: label.into(),
            kind: FieldKind::Value,
        });
        self
    }

    /// Add a bool flag — renders "Label." when true, omitted when false/missing.
    pub fn flag(mut self, key: &str, label: &str) -> Self {
        self.fields.push(Field {
            key: key.into(),
            label: label.into(),
            kind: FieldKind::Flag,
        });
        self
    }

    /// Add a sentiment field — renders "Caller seems {value}." when non-neutral.
    pub fn sentiment(mut self, key: &str) -> Self {
        self.fields.push(Field {
            key: key.into(),
            label: "sentiment".into(),
            kind: FieldKind::Sentiment,
        });
        self
    }

    /// Add a field with a custom formatter.
    ///
    /// The formatter receives the raw JSON value and returns a string.
    /// Return an empty string to skip the field.
    pub fn format(
        mut self,
        key: &str,
        label: &str,
        f: impl Fn(&Value) -> String + Send + Sync + 'static,
    ) -> Self {
        self.fields.push(Field {
            key: key.into(),
            label: label.into(),
            kind: FieldKind::Format(Arc::new(f)),
        });
        self
    }

    /// Start a new section (finalizes the current one).
    pub fn section(mut self, label: &str) -> SectionBuilder {
        // Finalize current section
        if !self.fields.is_empty() {
            self.parent_sections.push(Section {
                label: self.label,
                fields: self.fields,
            });
        }
        SectionBuilder {
            label: label.into(),
            fields: Vec::new(),
            parent_sections: self.parent_sections,
        }
    }

    /// Build the final [`ContextBuilder`].
    pub fn build(mut self) -> ContextBuilder {
        // Finalize last section
        if !self.fields.is_empty() {
            self.parent_sections.push(Section {
                label: self.label,
                fields: self.fields,
            });
        }
        ContextBuilder {
            sections: self.parent_sections,
        }
    }
}

// ── ContextBuilder ─────────────────────────────────────────────────────────

/// Declarative state-to-narrative renderer.
///
/// Renders session state into a natural-language summary for the model's
/// instruction. Sections group related keys; missing values are auto-skipped.
///
/// When the current phase has `needs` metadata (set via `.needs()` on the
/// phase builder), appends a "Gathering:" line for keys that are still missing,
/// so the model knows what to focus on in the current phase.
///
/// Use directly with `with_context()` on phase defaults or individual phases.
///
/// # Example
///
/// ```ignore
/// .phase_defaults(|d| d.with_context(
///     ContextBuilder::new()
///         .section("Caller")
///         .field("name", "Name")
///         .build()
/// ))
/// ```
#[derive(Clone)]
pub struct ContextBuilder {
    sections: Vec<Section>,
}

impl ContextBuilder {
    /// Start building a new context with the first section.
    pub fn new() -> SectionBuilder {
        SectionBuilder {
            label: String::new(),
            fields: Vec::new(),
            parent_sections: Vec::new(),
        }
    }

    /// Render the context from the given state.
    ///
    /// Returns an empty string when no state has been gathered (no noise).
    pub fn render(&self, state: &State) -> String {
        let mut lines: Vec<String> = self
            .sections
            .iter()
            .filter_map(|s| s.render(state))
            .collect();

        // Phase-aware "Gathering" context from phase needs metadata.
        // The processor stores the current phase's needs in "session:phase_needs"
        // as a JSON array of strings.
        if let Some(needs) = state.get::<Vec<String>>("session:phase_needs") {
            let missing: Vec<&str> = needs
                .iter()
                .filter(|key| !state.contains(key))
                .map(|s| s.as_str())
                .collect();
            if !missing.is_empty() {
                lines.push(format!("[Gathering] {}", missing.join(", ")));
            }
        }

        lines.join("\n")
    }

    /// Convert into an [`InstructionModifier`] for use with phase modifiers.
    pub fn into_modifier(self) -> super::InstructionModifier {
        super::InstructionModifier::CustomAppend(Arc::new(move |state: &State| {
            self.render(state)
        }))
    }
}

impl Default for ContextBuilder {
    fn default() -> Self {
        Self {
            sections: Vec::new(),
        }
    }
}

// ── Compose with + ─────────────────────────────────────────────────────────

impl std::ops::Add for ContextBuilder {
    type Output = ContextBuilder;

    fn add(mut self, rhs: ContextBuilder) -> Self::Output {
        self.sections.extend(rhs.sections);
        self
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    #[test]
    fn empty_state_returns_empty_string() {
        let ctx = ContextBuilder::new()
            .section("Caller")
            .field("name", "Name")
            .build();

        let state = State::new();
        assert_eq!(ctx.render(&state), "");
    }

    #[test]
    fn renders_populated_fields() {
        let ctx = ContextBuilder::new()
            .section("Caller")
            .field("name", "Name")
            .field("org", "Organization")
            .build();

        let state = State::new();
        state.set("name", "Bob");
        state.set("org", "Google");

        assert_eq!(
            ctx.render(&state),
            "[Caller] Name: Bob. Organization: Google."
        );
    }

    #[test]
    fn skips_missing_fields() {
        let ctx = ContextBuilder::new()
            .section("Caller")
            .field("name", "Name")
            .field("org", "Organization")
            .build();

        let state = State::new();
        state.set("name", "Bob");

        assert_eq!(ctx.render(&state), "[Caller] Name: Bob.");
    }

    #[test]
    fn flag_renders_when_true() {
        let ctx = ContextBuilder::new()
            .section("Status")
            .flag("verified", "Identity verified")
            .build();

        let state = State::new();
        state.set("verified", true);

        assert_eq!(ctx.render(&state), "[Status] Identity verified.");
    }

    #[test]
    fn flag_omitted_when_false() {
        let ctx = ContextBuilder::new()
            .section("Status")
            .flag("verified", "Identity verified")
            .build();

        let state = State::new();
        state.set("verified", false);

        assert_eq!(ctx.render(&state), "");
    }

    #[test]
    fn sentiment_renders_non_neutral() {
        let ctx = ContextBuilder::new()
            .section("Mood")
            .sentiment("sentiment")
            .build();

        let state = State::new();
        state.set("sentiment", "impatient");

        assert_eq!(ctx.render(&state), "[Mood] Caller seems impatient.");
    }

    #[test]
    fn sentiment_skips_neutral() {
        let ctx = ContextBuilder::new()
            .section("Mood")
            .sentiment("sentiment")
            .build();

        let state = State::new();
        state.set("sentiment", "neutral");

        assert_eq!(ctx.render(&state), "");
    }

    #[test]
    fn custom_format() {
        let ctx = ContextBuilder::new()
            .section("Call")
            .format("urgency", "Urgency", |v| {
                let u = v.as_f64().unwrap_or(0.0);
                if u > 0.7 {
                    format!("high ({u:.1})")
                } else {
                    String::new()
                }
            })
            .build();

        let state = State::new();
        state.set("urgency", 0.9_f64);

        assert_eq!(ctx.render(&state), "[Call] high (0.9)");
    }

    #[test]
    fn multiple_sections() {
        let ctx = ContextBuilder::new()
            .section("A")
            .field("x", "X")
            .section("B")
            .field("y", "Y")
            .build();

        let state = State::new();
        state.set("x", "1");
        state.set("y", "2");

        assert_eq!(ctx.render(&state), "[A] X: 1.\n[B] Y: 2.");
    }

    #[test]
    fn empty_section_omitted() {
        let ctx = ContextBuilder::new()
            .section("Empty")
            .field("missing", "Missing")
            .section("Present")
            .field("exists", "Exists")
            .build();

        let state = State::new();
        state.set("exists", "yes");

        assert_eq!(ctx.render(&state), "[Present] Exists: yes.");
    }

    #[test]
    fn compose_with_add() {
        let a = ContextBuilder::new()
            .section("A")
            .field("x", "X")
            .build();

        let b = ContextBuilder::new()
            .section("B")
            .field("y", "Y")
            .build();

        let combined = a + b;

        let state = State::new();
        state.set("x", "1");
        state.set("y", "2");

        assert_eq!(combined.render(&state), "[A] X: 1.\n[B] Y: 2.");
    }

    #[test]
    fn phase_needs_shows_gathering() {
        let ctx = ContextBuilder::new()
            .section("Caller")
            .field("name", "Name")
            .build();

        let state = State::new();
        state.set("name", "Bob");
        state.set(
            "session:phase_needs",
            vec!["name".to_string(), "org".to_string()],
        );

        let rendered = ctx.render(&state);
        assert!(rendered.contains("[Caller] Name: Bob."));
        assert!(rendered.contains("[Gathering] org"));
    }

    #[test]
    fn phase_needs_disappears_when_all_gathered() {
        let ctx = ContextBuilder::new()
            .section("Caller")
            .field("name", "Name")
            .field("org", "Org")
            .build();

        let state = State::new();
        state.set("name", "Bob");
        state.set("org", "Google");
        state.set(
            "session:phase_needs",
            vec!["name".to_string(), "org".to_string()],
        );

        let rendered = ctx.render(&state);
        assert!(!rendered.contains("[Gathering]"));
    }
}
