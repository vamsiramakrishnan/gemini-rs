//! Conversation repair protocol — tracks need fulfillment and nudges.
//!
//! When a phase declares `needs` (state keys that must be gathered), this
//! module tracks whether the conversation is making progress. After N turns
//! without progress, it nudges the model via context injection. After M turns,
//! it sets an escalation flag for phase guards to pick up.

use std::collections::HashMap;

use crate::state::State;

/// Default turns before first nudge.
pub const DEFAULT_NUDGE_AFTER: u32 = 3;
/// Default turns before escalation.
pub const DEFAULT_ESCALATE_AFTER: u32 = 6;

/// Configuration for the conversation repair system.
#[derive(Debug, Clone)]
pub struct RepairConfig {
    /// Turns without progress before first nudge.
    pub nudge_after: u32,
    /// Turns without progress before escalation flag is set.
    pub escalate_after: u32,
}

impl Default for RepairConfig {
    fn default() -> Self {
        Self {
            nudge_after: DEFAULT_NUDGE_AFTER,
            escalate_after: DEFAULT_ESCALATE_AFTER,
        }
    }
}

impl RepairConfig {
    /// Create a new config with custom thresholds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the nudge threshold.
    pub fn nudge_after(mut self, n: u32) -> Self {
        self.nudge_after = n;
        self
    }

    /// Set the escalation threshold.
    pub fn escalate_after(mut self, n: u32) -> Self {
        self.escalate_after = n;
        self
    }
}

/// What action the repair system recommends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairAction {
    /// No intervention needed.
    None,
    /// Nudge the model to collect missing information.
    Nudge {
        /// Keys that still need values.
        unfulfilled: Vec<String>,
        /// Which nudge attempt this is (1-based).
        attempt: u32,
    },
    /// Escalation — set state flag for phase guards.
    Escalate {
        /// Keys that still need values.
        unfulfilled: Vec<String>,
    },
}

/// Tracks need fulfillment per phase and recommends repair actions.
pub struct NeedsFulfillment {
    /// Phase name → consecutive turns without progress.
    stall_count: HashMap<String, u32>,
    /// Configuration thresholds.
    config: RepairConfig,
}

impl NeedsFulfillment {
    /// Create with the given configuration.
    pub fn new(config: RepairConfig) -> Self {
        Self {
            stall_count: HashMap::new(),
            config,
        }
    }

    /// Evaluate whether repair action is needed for the current phase.
    ///
    /// Call after extractors run. Returns the recommended action.
    pub fn evaluate(&mut self, phase: &str, needs: &[String], state: &State) -> RepairAction {
        let unfulfilled: Vec<String> = needs
            .iter()
            .filter(|key| state.get_raw(key).is_none())
            .cloned()
            .collect();

        if unfulfilled.is_empty() {
            self.stall_count.remove(phase);
            return RepairAction::None;
        }

        let count = self.stall_count.entry(phase.to_string()).or_insert(0);
        *count += 1;

        if *count >= self.config.escalate_after {
            RepairAction::Escalate { unfulfilled }
        } else if *count >= self.config.nudge_after {
            RepairAction::Nudge {
                unfulfilled,
                attempt: *count - self.config.nudge_after + 1,
            }
        } else {
            RepairAction::None
        }
    }

    /// Reset tracking for a phase (call on phase transition).
    pub fn reset(&mut self, phase: &str) {
        self.stall_count.remove(phase);
    }

    /// Reset all tracking.
    pub fn reset_all(&mut self) {
        self.stall_count.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_action_when_needs_fulfilled() {
        let state = State::new();
        state.set("customer_id", "C123");
        state.set("account_number", "A456");

        let mut nf = NeedsFulfillment::new(RepairConfig::default());
        let action = nf.evaluate(
            "gather",
            &["customer_id".into(), "account_number".into()],
            &state,
        );
        assert_eq!(action, RepairAction::None);
    }

    #[test]
    fn no_action_before_threshold() {
        let state = State::new();
        let mut nf = NeedsFulfillment::new(RepairConfig::default());

        // First 2 turns: no action (threshold is 3)
        for _ in 0..2 {
            let action = nf.evaluate("gather", &["customer_id".into()], &state);
            assert_eq!(action, RepairAction::None);
        }
    }

    #[test]
    fn nudge_at_threshold() {
        let state = State::new();
        let mut nf = NeedsFulfillment::new(RepairConfig::default());

        // Turns 1-2: no action
        for _ in 0..2 {
            nf.evaluate("gather", &["customer_id".into()], &state);
        }

        // Turn 3: nudge
        let action = nf.evaluate("gather", &["customer_id".into()], &state);
        assert!(matches!(action, RepairAction::Nudge { attempt: 1, .. }));
    }

    #[test]
    fn escalation_at_threshold() {
        let state = State::new();
        let mut nf = NeedsFulfillment::new(RepairConfig::default());

        for _ in 0..5 {
            nf.evaluate("gather", &["customer_id".into()], &state);
        }

        // Turn 6: escalate
        let action = nf.evaluate("gather", &["customer_id".into()], &state);
        assert!(matches!(action, RepairAction::Escalate { .. }));
    }

    #[test]
    fn fulfilling_need_resets_counter() {
        let state = State::new();
        let mut nf = NeedsFulfillment::new(RepairConfig::default());

        // Stall for 2 turns
        for _ in 0..2 {
            nf.evaluate("gather", &["customer_id".into()], &state);
        }

        // Fulfill the need
        state.set("customer_id", "C123");
        let action = nf.evaluate("gather", &["customer_id".into()], &state);
        assert_eq!(action, RepairAction::None);

        // Counter should be reset — unfulfill again
        state.remove("customer_id");
        let action = nf.evaluate("gather", &["customer_id".into()], &state);
        assert_eq!(action, RepairAction::None); // Turn 1 of new stall
    }

    #[test]
    fn custom_thresholds() {
        let state = State::new();
        let mut nf = NeedsFulfillment::new(RepairConfig::new().nudge_after(1).escalate_after(2));

        // Turn 1: nudge immediately
        let action = nf.evaluate("gather", &["x".into()], &state);
        assert!(matches!(action, RepairAction::Nudge { attempt: 1, .. }));

        // Turn 2: escalate
        let action = nf.evaluate("gather", &["x".into()], &state);
        assert!(matches!(action, RepairAction::Escalate { .. }));
    }
}
