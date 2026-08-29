//! Instruction templating — inject state values into instruction strings.
//!
//! Replaces `{key}` placeholders with values from the state container.
//! Supports optional `{key?}` syntax that resolves to empty string if missing.

use regex::Regex;
use std::sync::LazyLock;

use crate::state::State;

static PLACEHOLDER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([a-zA-Z_][a-zA-Z0-9_:]*)\??\}").unwrap());

/// Replace `{key}` placeholders in `template` with values from `state`.
///
/// - `{key}` — required: if present in state, replaced with the string representation;
///   if missing, left as-is (e.g., `{unknown}` stays `{unknown}`)
/// - `{key?}` — optional: if present in state, replaced; if missing, replaced with `""`
/// - Prefix keys are supported: `{app:flag}`, `{user:name}`, etc.
pub fn inject_session_state(template: &str, state: &State) -> String {
    PLACEHOLDER_RE
        .replace_all(template, |caps: &regex::Captures| {
            let full_match = &caps[0];
            let key = &caps[1];
            let optional = full_match.ends_with("?}");

            match state.get_raw(key) {
                Some(value) => value_to_string(&value),
                None => {
                    if optional {
                        String::new()
                    } else {
                        full_match.to_string()
                    }
                }
            }
        })
        .into_owned()
}

/// A dynamic instruction source — the ADK "instruction provider" pattern:
/// instead of a string fixed at build time, the instruction is produced
/// from live session state on every model request (persona switching,
/// risk-driven guardrails, multi-tenant instructions without rebuilding
/// the agent). Any `Fn(&State) -> String` closure is a provider.
pub trait InstructionProvider: Send + Sync {
    /// Produce the system instruction for the current request.
    fn provide(&self, state: &State) -> String;
}

impl<F> InstructionProvider for F
where
    F: Fn(&State) -> String + Send + Sync,
{
    fn provide(&self, state: &State) -> String {
        self(state)
    }
}

impl<T: InstructionProvider + ?Sized> InstructionProvider for std::sync::Arc<T> {
    fn provide(&self, state: &State) -> String {
        (**self).provide(state)
    }
}

/// A full template-engine instruction *(feature `templates`)* — minijinja
/// (Jinja2 syntax: conditionals, loops, filters) over the session state,
/// mirroring ADK's `use_jinja2` instructions. The whole state is exposed
/// as `state` (subscript prefixed keys: `{{ state["session:turn_count"] }}`),
/// and every key that is a bare identifier is also available at top level
/// (`{{ name }}`). Falls back to the empty string for missing values under
/// Jinja's default undefined semantics.
///
/// ```ignore
/// let inst = TemplateInstruction::new(
///     "You are a support agent.\n\
///      {% if state[\"derived:risk\"] and state[\"derived:risk\"] > 0.8 %}\
///      Escalate carefully and show extra empathy.{% endif %}",
/// )?;
/// LlmTextAgent::new("support", llm).instruction_provider(inst);
/// ```
#[cfg(feature = "templates")]
pub struct TemplateInstruction {
    source: String,
}

#[cfg(feature = "templates")]
impl TemplateInstruction {
    /// Compile-check the template now so errors surface at build time, not
    /// on the first model request.
    pub fn new(source: impl Into<String>) -> Result<Self, String> {
        let source = source.into();
        let env = minijinja::Environment::new();
        env.template_from_str(&source)
            .map_err(|e| format!("template error: {e}"))?;
        Ok(Self { source })
    }

    fn context(state: &State) -> minijinja::Value {
        let mut all = serde_json::Map::new();
        let mut top = serde_json::Map::new();
        for key in state.keys() {
            if let Some(value) = state.get_raw(&key) {
                let bare_identifier = !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !key.starts_with(|c: char| c.is_ascii_digit());
                if bare_identifier {
                    top.insert(key.clone(), value.clone());
                }
                all.insert(key, value);
            }
        }
        top.insert("state".into(), serde_json::Value::Object(all));
        minijinja::Value::from_serialize(&serde_json::Value::Object(top))
    }
}

#[cfg(feature = "templates")]
impl InstructionProvider for TemplateInstruction {
    fn provide(&self, state: &State) -> String {
        let env = minijinja::Environment::new();
        env.render_str(&self.source, Self::context(state))
            .unwrap_or_else(|e| format!("[template render error: {e}]"))
    }
}

fn value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_is_an_instruction_provider() {
        let state = State::new();
        let _ = state.set("persona", "pirate");
        let provider = |s: &State| {
            format!(
                "Speak like a {}.",
                s.get::<String>("persona").unwrap_or_default()
            )
        };
        assert_eq!(
            InstructionProvider::provide(&provider, &state),
            "Speak like a pirate."
        );
    }

    #[cfg(feature = "templates")]
    #[test]
    fn template_instruction_renders_conditionals_and_prefixed_keys() {
        let state = State::new();
        let _ = state.set("name", "Alice");
        let _ = state.set("derived:risk", 0.9);
        let inst = TemplateInstruction::new(
            "Hello {{ name }}.{% if state[\"derived:risk\"] > 0.8 %} Escalate.{% endif %}",
        )
        .unwrap();
        assert_eq!(inst.provide(&state), "Hello Alice. Escalate.");
    }

    #[cfg(feature = "templates")]
    #[test]
    fn template_instruction_rejects_bad_syntax_at_build() {
        assert!(TemplateInstruction::new("{% if x %}unclosed").is_err());
    }

    #[test]
    fn simple_substitution() {
        let state = State::new();
        let _ = state.set("name", "Alice");
        let result = inject_session_state("Hello, {name}!", &state);
        assert_eq!(result, "Hello, Alice!");
    }

    #[test]
    fn optional_key_present() {
        let state = State::new();
        let _ = state.set("title", "Dr.");
        let result = inject_session_state("Hello, {title?} Smith!", &state);
        assert_eq!(result, "Hello, Dr. Smith!");
    }

    #[test]
    fn optional_key_missing() {
        let state = State::new();
        let result = inject_session_state("Hello, {title?}Smith!", &state);
        assert_eq!(result, "Hello, Smith!");
    }

    #[test]
    fn missing_required_key_left_as_is() {
        let state = State::new();
        let result = inject_session_state("Hello, {unknown}!", &state);
        assert_eq!(result, "Hello, {unknown}!");
    }

    #[test]
    fn multiple_keys() {
        let state = State::new();
        let _ = state.set("first", "Alice");
        let _ = state.set("last", "Smith");
        let result = inject_session_state("{first} {last}", &state);
        assert_eq!(result, "Alice Smith");
    }

    #[test]
    fn prefix_key() {
        let state = State::new();
        let _ = state.app().set("flag", true);
        let result = inject_session_state("Flag is {app:flag}", &state);
        assert_eq!(result, "Flag is true");
    }

    #[test]
    fn no_placeholders_passthrough() {
        let state = State::new();
        let template = "No placeholders here.";
        assert_eq!(inject_session_state(template, &state), template);
    }

    #[test]
    fn numeric_value() {
        let state = State::new();
        let _ = state.set("count", 42);
        let result = inject_session_state("Count: {count}", &state);
        assert_eq!(result, "Count: 42");
    }

    #[test]
    fn empty_template() {
        let state = State::new();
        assert_eq!(inject_session_state("", &state), "");
    }
}
