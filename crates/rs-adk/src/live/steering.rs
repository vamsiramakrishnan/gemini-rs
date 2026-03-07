//! Context injection steering — an alternative to instruction replacement.
//!
//! Instead of replacing the entire system instruction on every turn/phase,
//! steering injects model-role context turns via `send_client_content`.
//! This works *with* the model's conversational intelligence rather than
//! overriding it.

use serde_json::Value;

use crate::state::State;

/// How the phase machine steers the model's behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SteeringMode {
    /// Replace system instruction on phase transition.
    /// Use for major persona/goal changes.
    #[default]
    InstructionUpdate,
    /// Inject steering context via `send_client_content`.
    /// Lighter weight, works WITH the model's conversational intelligence.
    ContextInjection,
    /// Hybrid: instruction update on phase transition, context injection per turn.
    Hybrid,
}

/// Build steering context from instruction modifiers.
///
/// Converts `InstructionModifier`s into conversational text suitable for
/// injection as a model-role context turn.
pub fn build_steering_context(
    state: &State,
    modifiers: &[super::phase::InstructionModifier],
) -> Vec<String> {
    let mut parts = Vec::new();
    for modifier in modifiers {
        match modifier {
            super::phase::InstructionModifier::StateAppend(keys) => {
                let pairs: Vec<String> = keys
                    .iter()
                    .filter_map(|key| {
                        let display_key = key
                            .strip_prefix("derived:")
                            .or_else(|| key.strip_prefix("session:"))
                            .or_else(|| key.strip_prefix("app:"))
                            .or_else(|| key.strip_prefix("user:"))
                            .unwrap_or(key);
                        state
                            .get::<Value>(key)
                            .map(|v| format!("{}={}", display_key, v))
                    })
                    .collect();
                if !pairs.is_empty() {
                    parts.push(format!("Current context: {}", pairs.join(", ")));
                }
            }
            super::phase::InstructionModifier::Conditional { predicate, text } => {
                if predicate(state) {
                    parts.push(text.clone());
                }
            }
            super::phase::InstructionModifier::CustomAppend(f) => {
                let text = f(state);
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::phase::InstructionModifier;
    use std::sync::Arc;

    #[test]
    fn state_append_builds_context() {
        let state = State::new();
        state.set("mood", "calm");
        state.set("app:score", 0.85f64);

        let modifiers = vec![InstructionModifier::StateAppend(vec![
            "mood".into(),
            "app:score".into(),
        ])];

        let parts = build_steering_context(&state, &modifiers);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("mood="));
        assert!(parts[0].contains("score="));
    }

    #[test]
    fn conditional_appends_when_true() {
        let state = State::new();
        state.set("urgent", true);

        let modifiers = vec![InstructionModifier::Conditional {
            predicate: Arc::new(|s: &State| s.get::<bool>("urgent").unwrap_or(false)),
            text: "Handle with urgency.".into(),
        }];

        let parts = build_steering_context(&state, &modifiers);
        assert_eq!(parts, vec!["Handle with urgency."]);
    }

    #[test]
    fn conditional_skips_when_false() {
        let state = State::new();

        let modifiers = vec![InstructionModifier::Conditional {
            predicate: Arc::new(|s: &State| s.get::<bool>("urgent").unwrap_or(false)),
            text: "Handle with urgency.".into(),
        }];

        let parts = build_steering_context(&state, &modifiers);
        assert!(parts.is_empty());
    }
}
