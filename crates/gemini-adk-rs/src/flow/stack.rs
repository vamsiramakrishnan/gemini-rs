//! `FlowStack` — the runtime above the DAG: a main flow plus its digressions.
//!
//! A [`FlowMonitor`] governs one [`Flow`]. Real conversations
//! leave the main task temporarily — a side question, a cancel, a hand-off —
//! and expect to come back to it. A [`FlowStack`] holds the **main** monitor
//! plus a set of [`Overlay`]s (digressions): when an overlay's trigger holds,
//! the main flow is suspended untouched, the overlay's own monitor drives the
//! session until it completes, and the main flow then continues per the
//! overlay's [`Resume`] policy.
//!
//! This is the *only* governance object the Live control plane drives. A
//! session governed by a bare flow is a stack with no overlays, so every
//! execution path — the simulator, a live session, a replay — advances the same
//! type with the same semantics. The authoring layer lowers a conversation into
//! a stack; it does not implement one.
//!
//! Nesting depth is 1: a digression cannot itself be interrupted by another.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{
    CompiledFlow, Enforcement, Flow, FlowExplanation, FlowMonitor, Guard, Marking, Step, StepAction,
};
use crate::state::State;

/// The state key raised when a stage's repair policy escalates.
pub fn escalate_flag(stage: &str) -> String {
    format!("repair:{stage}:escalate")
}

/// The state key raised when a stage's repair policy asks for a reprompt.
pub fn reprompt_flag(stage: &str) -> String {
    format!("repair:{stage}:reprompt")
}

/// The state key that names the active digression (`null` when the main flow
/// is driving). Published by the control plane at every turn boundary.
pub const OVERLAY_STATE_KEY: &str = "flow:overlay";

/// How the main flow continues after a digression (overlay) completes.
///
/// `Restart` resets the main flow's *monitor* — its marking, fired `on_enter`
/// actions and reset edges — against the session's existing `State`. It does
/// not clear state, so slots the user already filled stay filled. It is a
/// fresh pass over the same conversation, not a fresh business task.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Resume {
    /// Resume the main flow exactly where it was suspended (history state).
    #[default]
    Previous,
    /// Re-enter the main flow from its start (see the type docs for what that
    /// does and does not reset).
    Restart,
    /// End the conversation (e.g. a cancel/handoff digression).
    Terminate,
}

fn default_reprompt_after() -> u32 {
    2
}
fn default_escalate_after() -> u32 {
    4
}

/// A step's repair policy for the weird paths (silence, no-match, the user
/// stalling). The stack sets `repair:{step}:reprompt` once the step has been
/// active `reprompt_after` turns without completing, and `repair:{step}:escalate`
/// after `escalate_after`. When `escalate_to` is set, the authoring layer lowers
/// an extra edge gated on the escalate signal — a deterministic "give up and
/// hand off".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepairPolicy {
    /// Turns the step may be active before a reprompt signal is raised.
    #[serde(default = "default_reprompt_after")]
    pub reprompt_after: u32,
    /// Turns the step may be active before an escalation signal is raised.
    #[serde(default = "default_escalate_after")]
    pub escalate_after: u32,
    /// Step to route to on escalation (also completes the current step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_to: Option<String>,
}

impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            reprompt_after: default_reprompt_after(),
            escalate_after: default_escalate_after(),
            escalate_to: None,
        }
    }
}

impl RepairPolicy {
    /// A policy with the given reprompt/escalate turn thresholds.
    pub fn new(reprompt_after: u32, escalate_after: u32) -> Self {
        Self {
            reprompt_after,
            escalate_after,
            escalate_to: None,
        }
    }

    /// Route to `step` on escalation (also completes the current step).
    pub fn escalate_to(mut self, step: impl Into<String>) -> Self {
        self.escalate_to = Some(step.into());
        self
    }
}

/// A digression the runtime can enter: its trigger, governed flow and resume
/// policy. Built from a compiled flow so it carries proof of compilation.
#[derive(Debug, Clone)]
pub struct Overlay {
    name: String,
    trigger: Guard,
    flow: Flow,
    resume: Resume,
}

impl Overlay {
    /// A digression over a compiled flow.
    pub fn new(
        name: impl Into<String>,
        trigger: Guard,
        flow: CompiledFlow,
        resume: Resume,
    ) -> Self {
        Self {
            name: name.into(),
            trigger,
            flow: flow.into_flow(),
            resume,
        }
    }

    /// The digression's name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The guard that activates it, evaluated against the main flow's context.
    pub fn trigger(&self) -> &Guard {
        &self.trigger
    }
    /// The digression's governed flow.
    pub fn flow(&self) -> &Flow {
        &self.flow
    }
    /// Mutable access to the flow, for connect-time merges (ambient tools).
    pub fn flow_mut(&mut self) -> &mut Flow {
        &mut self.flow
    }
    /// What the main flow does once this digression completes.
    pub fn resume(&self) -> Resume {
        self.resume
    }
}

/// A digression currently suspending the main flow.
struct ActiveOverlay {
    name: String,
    monitor: FlowMonitor,
    resume: Resume,
}

/// A shared, lock-protected [`FlowStack`] — the form in which the Live
/// control plane owns governance, so runtime surfaces (e.g.
/// [`LiveHandle::explain`](crate::live::LiveHandle::explain)) can snapshot it
/// concurrently. All methods are synchronous: lock briefly and never hold the
/// guard across an `await`.
pub type SharedFlowStack = Arc<parking_lot::Mutex<FlowStack>>;

/// The main flow plus its digressions, with push-on-trigger and
/// resume-on-completion.
///
/// While a digression is active, governance — tool admission, postures and
/// grounds, `explain()` — delegates to the **active** layer, and the main
/// flow's marking is untouched, so [`Resume::Previous`] resumes exactly where
/// it left off. Driven by `State` and guards: model-free and deterministic.
pub struct FlowStack {
    main: FlowMonitor,
    mode: Enforcement,
    overlays: Vec<Overlay>,
    active: Option<ActiveOverlay>,
    terminated: bool,
    /// Per-main-step repair policies.
    repair: BTreeMap<String, RepairPolicy>,
    /// Consecutive turns each main step has been active without completing.
    active_turns: BTreeMap<String, u32>,
}

impl std::fmt::Debug for FlowStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowStack")
            .field("mode", &self.mode)
            .field("overlays", &self.overlays.len())
            .field("active", &self.active.as_ref().map(|a| &a.name))
            .field("terminated", &self.terminated)
            .field("repair", &self.repair)
            .finish()
    }
}

impl FlowStack {
    /// A stack over a compiled main flow with no digressions yet.
    pub fn new(main: CompiledFlow, mode: Enforcement) -> Self {
        Self::from_monitor(FlowMonitor::compiled(main, mode))
    }

    /// A stack whose main layer is an existing monitor (keeps its `on_enter`
    /// actions and mode). This is how a bare governed flow becomes the one
    /// governance object the control plane drives.
    pub fn from_monitor(main: FlowMonitor) -> Self {
        Self {
            mode: main.mode(),
            main,
            overlays: Vec::new(),
            active: None,
            terminated: false,
            repair: BTreeMap::new(),
            active_turns: BTreeMap::new(),
        }
    }

    /// Add a digression.
    pub fn with_overlay(mut self, overlay: Overlay) -> Self {
        self.overlays.push(overlay);
        self
    }

    /// Add several digressions, in trigger-priority order.
    pub fn with_overlays(mut self, overlays: impl IntoIterator<Item = Overlay>) -> Self {
        self.overlays.extend(overlays);
        self
    }

    /// Attach a repair policy to a main-flow step.
    pub fn with_repair(mut self, step: impl Into<String>, policy: RepairPolicy) -> Self {
        self.repair.insert(step.into(), policy);
        self
    }

    /// Attach repair policies keyed by main-flow step.
    pub fn with_repairs(
        mut self,
        policies: impl IntoIterator<Item = (String, RepairPolicy)>,
    ) -> Self {
        self.repair.extend(policies);
        self
    }

    /// Wrap in a [`SharedFlowStack`] for shared ownership between the control
    /// lane (which advances it) and runtime accessors (which snapshot it).
    pub fn into_shared(self) -> SharedFlowStack {
        Arc::new(parking_lot::Mutex::new(self))
    }

    /// The enforcement mode (shared by every layer).
    pub fn mode(&self) -> Enforcement {
        self.mode
    }

    /// The digressions this stack can enter, in trigger-priority order.
    pub fn overlays(&self) -> &[Overlay] {
        &self.overlays
    }

    /// Mutable access to the digressions, for connect-time merges.
    pub fn overlays_mut(&mut self) -> &mut [Overlay] {
        &mut self.overlays
    }

    /// The repair policies keyed by main-flow step.
    pub fn repair_policies(&self) -> &BTreeMap<String, RepairPolicy> {
        &self.repair
    }

    /// The main flow's monitor, whether or not it is currently driving.
    pub fn main(&self) -> &FlowMonitor {
        &self.main
    }

    /// The monitor currently driving — the active digression if any, else the
    /// main flow.
    pub fn current(&self) -> &FlowMonitor {
        self.active.as_ref().map_or(&self.main, |a| &a.monitor)
    }

    fn current_mut(&mut self) -> &mut FlowMonitor {
        match &mut self.active {
            Some(a) => &mut a.monitor,
            None => &mut self.main,
        }
    }

    /// The name of the active digression, if one is suspending the main flow.
    pub fn active_overlay(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.name.as_str())
    }

    /// Whether the conversation is finished (main complete, or a `Terminate`
    /// digression ran).
    pub fn is_complete(&self) -> bool {
        self.terminated || (self.active.is_none() && self.main.is_complete())
    }

    /// Whether a `Terminate` digression ended the conversation.
    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    /// Index of the first overlay whose trigger holds against the main context.
    fn triggered(&self, state: &State) -> Option<usize> {
        self.overlays
            .iter()
            .position(|ov| self.main.eval(&ov.trigger, state))
    }

    /// Bump per-step active-turn counters for the main flow and raise repair
    /// signals when thresholds are hit. Clears signals for steps that are no
    /// longer active.
    fn apply_repair(&mut self, state: &State) {
        if self.repair.is_empty() {
            return;
        }
        let active: BTreeSet<String> = self
            .main
            .active_steps(state)
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let left: Vec<String> = self
            .active_turns
            .keys()
            .filter(|k| !active.contains(*k))
            .cloned()
            .collect();
        for step in left {
            self.active_turns.remove(&step);
            let _ = state.set(reprompt_flag(&step), false);
            // A step that completed *by escalating* must keep its escalate
            // signal: the authoring layer lowers `escalate_to` into an edge
            // gated on it, and gates are evaluated every turn, so clearing the
            // flag here would drop the hand-off target out of the active set
            // one turn after it was entered. Clear it only when the step left
            // the active set without completing (a reset).
            if !self.main.marking().done.contains(&step) {
                let _ = state.set(escalate_flag(&step), false);
            }
        }
        for step in &active {
            let count = self.active_turns.entry(step.clone()).or_insert(0);
            *count += 1;
            if let Some(rp) = self.repair.get(step) {
                if *count >= rp.reprompt_after {
                    let _ = state.set(reprompt_flag(step), true);
                }
                if *count >= rp.escalate_after {
                    let _ = state.set(escalate_flag(step), true);
                }
            }
        }
    }

    fn apply_resume(&mut self, resume: Resume) {
        match resume {
            // Main marking was untouched while suspended — nothing to do.
            Resume::Previous => {}
            Resume::Restart => self.main.restart(),
            Resume::Terminate => self.terminated = true,
        }
    }

    /// Advance one turn. Enters a triggered digression (suspending the main
    /// flow), advances an active digression and resumes when it completes, or
    /// advances the main flow.
    pub fn on_turn(&mut self, state: &State) {
        if self.terminated {
            return;
        }
        match &mut self.active {
            Some(active) => {
                active.monitor.on_turn(state);
                if active.monitor.is_complete() {
                    let resume = active.resume;
                    self.active = None;
                    self.apply_resume(resume);
                }
            }
            None => {
                if let Some(idx) = self.triggered(state) {
                    let ov = &self.overlays[idx];
                    let mut monitor = FlowMonitor::new(ov.flow.clone(), self.mode);
                    // Drive the digression's first turn so single-step overlays can latch.
                    monitor.on_turn(state);
                    if monitor.is_complete() {
                        let resume = ov.resume;
                        self.apply_resume(resume);
                    } else {
                        self.active = Some(ActiveOverlay {
                            name: ov.name.clone(),
                            monitor,
                            resume: ov.resume,
                        });
                    }
                } else {
                    // Repair bookkeeping is based on the pre-turn active set so
                    // an escalation signal can take effect this turn.
                    self.apply_repair(state);
                    self.main.on_turn(state);
                }
            }
        }
    }

    /// Record a successful tool call against the active layer.
    pub fn on_tool_ok(&mut self, tool: &str, state: &State) {
        self.current_mut().on_tool_ok(tool, state);
    }

    /// Observe a tool call for conformance against the active layer (see
    /// [`FlowMonitor::observe_tool`]).
    pub fn observe_tool(&mut self, tool: &str, ok: bool, state: &State) {
        self.current_mut().observe_tool(tool, ok, state);
    }

    /// Whether `tool` is admitted right now (delegates to the active layer).
    pub fn admits_tool(&self, tool: &str, state: &State) -> Result<(), String> {
        self.current().admits_tool(tool, state)
    }

    /// Explain the active layer's control-plane state.
    pub fn explain(&self, state: &State) -> FlowExplanation {
        self.current().explain(state)
    }

    /// The active layer's marking.
    pub fn marking(&self) -> &Marking {
        self.current().marking()
    }

    /// Steps of the active layer that are eligible but not yet done.
    pub fn active_steps(&self, state: &State) -> Vec<&Step> {
        self.current().active_steps(state)
    }

    /// Postures of the active layer's active steps.
    pub fn active_postures(&self, state: &State) -> Vec<String> {
        self.current().active_postures(state)
    }

    /// Grounding lines of the active layer's active steps.
    pub fn active_grounds(&self, state: &State) -> Vec<String> {
        self.current().active_grounds(state)
    }

    /// The active layer's unmet requirements.
    pub fn unmet_requirements(&self) -> Vec<String> {
        self.current().unmet_requirements()
    }

    /// Steps of the active layer that became active since the last call.
    pub fn take_newly_active(&mut self, state: &State) -> Vec<String> {
        self.current_mut().take_newly_active(state)
    }

    /// The `on_enter` action registered for a step of the active layer.
    pub fn enter_action(&self, step: &str) -> Option<&StepAction> {
        self.current().enter_action(step)
    }

    /// Replace a main-flow step's posture. Returns `true` when the step exists.
    pub fn set_posture(&mut self, step_id: &str, posture: Option<String>) -> bool {
        self.main.set_posture(step_id, posture)
    }

    /// Replace a main-flow step's grounding template. Returns `true` when the
    /// step exists.
    pub fn set_ground(&mut self, step_id: &str, ground: Option<String>) -> bool {
        self.main.set_ground(step_id, ground)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main_flow() -> CompiledFlow {
        Flow::new()
            .step("a")
            .done(Guard::is_true("a_done"))
            .step("b")
            .after("a")
            .terminal()
            .build()
            .expect("valid")
            .compile()
            .expect("compiles")
    }

    fn faq_overlay() -> Overlay {
        let flow = Flow::new()
            .step("answer")
            .done(Guard::is_true("faq_answered"))
            .step("faq_end")
            .after("answer")
            .terminal()
            .require(["faq_end"])
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        Overlay::new("faq", Guard::is_true("intent:faq"), flow, Resume::Previous)
    }

    #[test]
    fn bare_stack_behaves_like_its_monitor() {
        let state = State::new();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce);
        let mut mon = FlowMonitor::compiled(main_flow(), Enforcement::Enforce);
        for turn in 0..3 {
            if turn == 1 {
                let _ = state.set("a_done", true);
            }
            stack.on_turn(&state);
            mon.on_turn(&state);
            assert_eq!(stack.explain(&state).active, mon.explain(&state).active);
            assert_eq!(stack.marking().done, mon.marking().done);
        }
        assert!(stack.is_complete());
    }

    #[test]
    fn overlay_suspends_main_then_resumes_previous() {
        let state = State::new();
        let mut stack =
            FlowStack::new(main_flow(), Enforcement::Enforce).with_overlay(faq_overlay());

        assert!(stack.explain(&state).active.contains(&"a".to_string()));
        assert!(stack.active_overlay().is_none());

        let _ = state.set("intent:faq", true);
        stack.on_turn(&state);
        assert_eq!(stack.active_overlay(), Some("faq"));
        assert!(stack.explain(&state).active.contains(&"answer".to_string()));

        let _ = state.set("faq_answered", true);
        let _ = state.set("intent:faq", false);
        stack.on_turn(&state);
        assert!(stack.active_overlay().is_none());
        assert!(stack.explain(&state).active.contains(&"a".to_string()));

        let _ = state.set("a_done", true);
        stack.on_turn(&state);
        assert!(stack.main().marking().done.contains("a"));
    }

    #[test]
    fn restart_resets_marking_but_not_state() {
        let state = State::new();
        let cancel = {
            let flow = Flow::new()
                .step("confirm")
                .done(Guard::is_true("confirmed"))
                .step("cancel_end")
                .after("confirm")
                .terminal()
                .require(["cancel_end"])
                .build()
                .expect("valid")
                .compile()
                .expect("compiles");
            Overlay::new(
                "cancel",
                Guard::is_true("intent:cancel"),
                flow,
                Resume::Restart,
            )
        };
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce).with_overlay(cancel);

        let _ = state.set("a_done", true);
        stack.on_turn(&state);
        assert!(stack.main().marking().done.contains("a"));

        let _ = state.set("intent:cancel", true);
        stack.on_turn(&state);
        assert_eq!(stack.active_overlay(), Some("cancel"));
        let _ = state.set("confirmed", true);
        let _ = state.set("intent:cancel", false);
        stack.on_turn(&state);
        assert!(stack.active_overlay().is_none());

        // Monitor restarted: nothing done until re-latched…
        assert!(stack.main().marking().done.is_empty());
        // …but state is untouched, so the next turn re-latches from the same facts.
        assert_eq!(state.get::<bool>("a_done"), Some(true));
        stack.on_turn(&state);
        assert!(stack.main().marking().done.contains("a"));
    }

    #[test]
    fn terminate_ends_the_conversation() {
        let state = State::new();
        let bye = {
            let flow = Flow::new()
                .step("bye")
                .terminal()
                .build()
                .expect("valid")
                .compile()
                .expect("compiles");
            Overlay::new("bye", Guard::is_true("intent:bye"), flow, Resume::Terminate)
        };
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce).with_overlay(bye);
        let _ = state.set("intent:bye", true);
        stack.on_turn(&state);
        assert!(stack.is_terminated());
        assert!(stack.is_complete());
    }

    #[test]
    fn repair_raises_then_clears_signals() {
        let state = State::new();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce)
            .with_repair("a", RepairPolicy::new(1, 3));
        stack.on_turn(&state);
        assert_eq!(state.get::<bool>("repair:a:reprompt"), Some(true));
        assert_eq!(state.get::<bool>("repair:a:escalate"), None);
        let _ = state.set("a_done", true);
        stack.on_turn(&state);
        stack.on_turn(&state);
        assert_eq!(state.get::<bool>("repair:a:reprompt"), Some(false));
    }

    /// `escalate_to` lowers to an edge gated on the escalate signal. Gates are
    /// evaluated every turn, so the signal must stay latched once the step
    /// has completed by escalating, or the hand-off target is active for one
    /// turn and then vanishes.
    #[test]
    fn escalation_edge_stays_open_after_the_step_completes() {
        let state = State::new();
        // The same shape the conversation compiler lowers: `collect` completes
        // on `info` or on its escalate flag; `handoff` follows, gated on the flag.
        let main = Flow::new()
            .step("collect")
            .done(Guard::any(vec![
                Guard::is_true("info"),
                Guard::is_true(escalate_flag("collect")),
            ]))
            .step("handoff")
            .after("collect")
            .gate(Guard::is_true(escalate_flag("collect")))
            .done(Guard::is_true("handoff_complete"))
            .step("done")
            .after_when("collect", Guard::is_true("info"))
            .terminal()
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        let mut stack = FlowStack::new(main, Enforcement::Enforce)
            .with_repair("collect", RepairPolicy::new(1, 2).escalate_to("handoff"));

        stack.on_turn(&state); // active 1 turn: reprompt
        stack.on_turn(&state); // active 2 turns: escalate -> collect done -> handoff
        assert!(
            stack
                .explain(&state)
                .active
                .contains(&"handoff".to_string())
        );
        // The turn after, collect has left the active set; handoff must stay.
        stack.on_turn(&state);
        assert!(
            stack
                .explain(&state)
                .active
                .contains(&"handoff".to_string())
        );
        assert_eq!(state.get::<bool>(&reprompt_flag("collect")), Some(false));
        assert_eq!(state.get::<bool>(&escalate_flag("collect")), Some(true));
    }

    #[test]
    fn tool_admission_follows_the_active_layer() {
        let state = State::new();
        let main = Flow::new()
            .step("a")
            .allow(["main_tool"])
            .done(Guard::is_true("a_done"))
            .step("b")
            .after("a")
            .terminal()
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        let ov = {
            let flow = Flow::new()
                .step("answer")
                .allow(["faq_tool"])
                .done(Guard::is_true("faq_answered"))
                .step("faq_end")
                .after("answer")
                .terminal()
                .require(["faq_end"])
                .build()
                .expect("valid")
                .compile()
                .expect("compiles");
            Overlay::new("faq", Guard::is_true("intent:faq"), flow, Resume::Previous)
        };
        let mut stack = FlowStack::new(main, Enforcement::Enforce).with_overlay(ov);
        assert!(stack.admits_tool("main_tool", &state).is_ok());
        assert!(stack.admits_tool("faq_tool", &state).is_err());
        let _ = state.set("intent:faq", true);
        stack.on_turn(&state);
        assert!(stack.admits_tool("faq_tool", &state).is_ok());
        assert!(stack.admits_tool("main_tool", &state).is_err());
    }
}
