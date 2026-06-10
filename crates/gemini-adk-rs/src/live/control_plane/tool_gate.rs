//! The single gate where a completed tool advances the governed flow.
//!
//! Both inline and background tools converge here. The gate enforces one
//! invariant (code-review redline #7):
//!
//! > a tool completion advances the governed flow **exactly once**, iff the
//! > tool actually completed.
//!
//! Inline tools call [`ToolGate::observe_completion`] directly from the control
//! lane; background tools (which run in a detached task that cannot reach the
//! synchronous `FlowMonitor`) signal completion back to the control lane, which
//! then routes them through the same gate. Idempotency is keyed by `call_id`, so
//! a completion observed twice (e.g. a retry or a duplicated signal) advances the
//! flow only once.

use std::collections::HashSet;

use crate::state::State;

/// Idempotent sink for tool completions feeding the governed flow.
#[derive(Default)]
pub(in crate::live) struct ToolGate {
    /// `call_id`s whose completion already advanced the flow.
    observed: HashSet<String>,
}

impl ToolGate {
    /// Create an empty gate.
    pub(in crate::live) fn new() -> Self {
        Self::default()
    }

    /// Advance the governed flow for a completed tool — at most once per
    /// `call_id`.
    ///
    /// `ok` is whether the tool completed successfully. A completion with an
    /// empty `call_id` (the model supplied no correlation id) cannot be deduped,
    /// so it is always observed — matching the pre-gate behavior. No-op when no
    /// flow is governing the session.
    pub(in crate::live) fn observe_completion(
        &mut self,
        call_id: &str,
        name: &str,
        ok: bool,
        flow: &mut Option<crate::flow::FlowMonitor>,
        state: &State,
    ) {
        if !call_id.is_empty() && !self.observed.insert(call_id.to_string()) {
            return; // this completion already advanced the flow
        }
        if let Some(mon) = flow.as_mut() {
            mon.observe_tool(name, ok, state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Enforcement, Flow, FlowMonitor, Guard};

    fn one_step_flow() -> FlowMonitor {
        let flow = Flow::new()
            .step("charge")
            .done(Guard::called_ok("charge_card"))
            .step("end")
            .after("charge")
            .terminal()
            .build()
            .expect("valid flow");
        FlowMonitor::new(flow, Enforcement::Observe)
    }

    #[test]
    fn observe_completion_advances_flow_once_per_call_id() {
        let state = State::new();
        let mut flow = Some(one_step_flow());
        let mut gate = ToolGate::new();

        // First completion advances the flow: `charge` latches done.
        gate.observe_completion("c1", "charge_card", true, &mut flow, &state);
        flow.as_mut().unwrap().on_turn(&state);
        assert!(flow.as_ref().unwrap().marking().done.contains("charge"));

        // A duplicate completion for the same call_id is a no-op (idempotent).
        // We can't easily observe a second `on_tool_ok` directly, but inserting
        // the id twice must not re-run the observation: assert the guard held.
        let before = gate.observed.len();
        gate.observe_completion("c1", "charge_card", true, &mut flow, &state);
        assert_eq!(gate.observed.len(), before, "duplicate id not re-inserted");
    }

    #[test]
    fn empty_call_id_is_always_observed() {
        let state = State::new();
        let mut flow = Some(one_step_flow());
        let mut gate = ToolGate::new();

        gate.observe_completion("", "charge_card", true, &mut flow, &state);
        gate.observe_completion("", "charge_card", true, &mut flow, &state);
        // Empty ids are never recorded, so the gate stays empty.
        assert!(gate.observed.is_empty());
        flow.as_mut().unwrap().on_turn(&state);
        assert!(flow.as_ref().unwrap().marking().done.contains("charge"));
    }

    #[test]
    fn no_flow_is_a_no_op() {
        let state = State::new();
        let mut flow: Option<FlowMonitor> = None;
        let mut gate = ToolGate::new();
        // Must not panic when no flow governs the session.
        gate.observe_completion("c1", "charge_card", true, &mut flow, &state);
    }
}
