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
    fn simple_substitution() {
        let state = State::new();
        state.set("name", "Alice");
        let result = inject_session_state("Hello, {name}!", &state);
        assert_eq!(result, "Hello, Alice!");
    }

    #[test]
    fn optional_key_present() {
        let state = State::new();
        state.set("title", "Dr.");
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
        state.set("first", "Alice");
        state.set("last", "Smith");
        let result = inject_session_state("{first} {last}", &state);
        assert_eq!(result, "Alice Smith");
    }

    #[test]
    fn prefix_key() {
        let state = State::new();
        state.app().set("flag", true);
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
        state.set("count", 42);
        let result = inject_session_state("Count: {count}", &state);
        assert_eq!(result, "Count: 42");
    }

    #[test]
    fn empty_template() {
        let state = State::new();
        assert_eq!(inject_session_state("", &state), "");
    }
}
