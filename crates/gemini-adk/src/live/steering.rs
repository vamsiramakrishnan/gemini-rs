//! Context injection steering — an alternative to instruction replacement.
//!
//! Instead of replacing the entire system instruction on every turn/phase,
//! steering injects model-role context turns via `send_client_content`.
//! This works *with* the model's conversational intelligence rather than
//! overriding it.

use serde_json::Value;

use crate::state::State;

/// How the phase machine steers the model's behavior.
///
/// Controls two things:
/// 1. **Phase instruction delivery** — whether the phase instruction is sent as
///    a system instruction update (`update_instruction`) or as a model-role
///    context turn (`send_client_content`).
/// 2. **Per-turn modifier delivery** — whether `with_state`, `when`, and
///    `with_context` modifiers are baked into the system instruction or
///    injected as model-role context turns.
///
/// # Choosing a mode
///
/// | Mode | System instruction | Modifiers | Best for |
/// |------|--------------------|-----------|----------|
/// | `InstructionUpdate` | Replaced on every phase transition | Baked into instruction | Agents with radically different personas per phase |
/// | `ContextInjection` | Set once at connect, never touched | Model-role context turns | Multi-phase apps with stable persona (recommended) |
/// | `Hybrid` | Replaced on phase transition | Model-role context turns | Persona shifts + lightweight per-turn context |
///
/// # Example
///
/// ```rust,ignore
/// use gemini_adk_fluent::prelude::*;
///
/// // Recommended: base instruction at connect, phase context via injection
/// let handle = Live::builder()
///     .instruction("You are a helpful restaurant reservation assistant.")
///     .steering_mode(SteeringMode::ContextInjection)
///     .phase("greeting")
///         .instruction("Welcome the guest and ask how you can help.")
///         .done()
///     .phase("booking")
///         .instruction("Help the guest find an available time slot.")
///         .done()
///     .initial_phase("greeting")
///     .connect_google_ai(api_key)
///     .await?;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SteeringMode {
    /// Replace system instruction on phase transition.
    ///
    /// The model re-processes its full context on every phase change.
    /// Gives the clearest persona shift but causes a latency spike as the
    /// model ingests the new instruction.
    ///
    /// Per-turn modifiers (`with_state`, `when`, `with_context`) are baked
    /// into the system instruction text.
    #[default]
    InstructionUpdate,

    /// Inject all steering via `send_client_content` (model-role turns).
    ///
    /// The system instruction set at connect time is **never updated**.
    /// Phase instructions and per-turn modifiers are delivered as model-role
    /// context turns, working *with* the model's conversational intelligence
    /// rather than overriding it.
    ///
    /// Lighter weight, lower latency, avoids instruction re-processing.
    /// Best for multi-phase apps where the base persona is stable.
    ContextInjection,

    /// Hybrid: instruction update on phase transition + context injection per turn.
    ///
    /// Phase transitions trigger a system instruction replacement (like
    /// `InstructionUpdate`), but per-turn modifiers are delivered as
    /// model-role context turns (like `ContextInjection`).
    ///
    /// Use when phases represent genuinely different personas but you also
    /// want lightweight per-turn steering within each phase.
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

/// When to deliver model-role context turns to the wire.
///
/// Controls timing of context injection (tool advisory, repair nudge,
/// steering modifiers, phase instructions, on_enter_context).
///
/// | Mode | Behavior | Best for |
/// |------|----------|----------|
/// | `Immediate` | Send as single batched frame during TurnComplete | Low-latency apps, text-only |
/// | `Deferred` | Queue until next user send (audio/text/video) | Voice apps where mid-silence sends cause glitches |
///
/// # Example
///
/// ```rust,ignore
/// Live::builder()
///     .steering_mode(SteeringMode::ContextInjection)
///     .context_delivery(ContextDelivery::Deferred)  // flush with next user audio
///     .phase("greeting")
///         .instruction("Welcome the guest")
///         .done()
///     .initial_phase("greeting")
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextDelivery {
    /// Send batched context immediately during TurnComplete processing.
    ///
    /// All context turns are accumulated into a single `send_client_content`
    /// call and sent as one WebSocket frame.  The model receives the context
    /// as soon as the turn completes, before the next user interaction.
    #[default]
    Immediate,

    /// Queue context and flush before the next user send.
    ///
    /// Context turns are pushed into a [`PendingContext`](super::context_writer::PendingContext)
    /// buffer.  The [`DeferredWriter`](super::context_writer::DeferredWriter)
    /// drains this buffer before forwarding `send_audio`, `send_text`, or
    /// `send_video` — ensuring context arrives in the same burst as user content.
    ///
    /// This eliminates the "extraneous message" problem where isolated context
    /// frames sent during silence can cause the model to interrupt or produce
    /// unexpected responses.
    Deferred,
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
