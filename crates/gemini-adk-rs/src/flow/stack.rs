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
//! Digressions nest: a digression can itself be interrupted by another (one
//! not already on the active path), which drives until it completes and then
//! resumes the one beneath it per its own [`Resume`] policy.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::timing::{VOICE_TIMING_KEY, VoiceTiming};
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

/// The state key raised for one turn when the user corrects `slot`: its
/// value changed from one captured value to another. See
/// [`FlowStack::with_correction`].
pub fn correction_flag(slot: &str) -> String {
    format!("correction:{slot}")
}

/// The state key that names the active digression (`null` when the main flow
/// is driving). Published by the control plane at every turn boundary.
pub const OVERLAY_STATE_KEY: &str = "flow:overlay";

/// The state key raised (`true`) once a [`Resume::Terminate`] digression has
/// ended the conversation. Governance is inert from then on: no postures, no
/// admitted tools. The runtime does not hang up by itself — the application
/// decides how a call ends — so watch this key (or
/// [`FlowStack::is_terminated`]) and close the session. Published by the
/// control plane at every turn boundary.
pub const TERMINATED_STATE_KEY: &str = "flow:terminated";

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
    /// does and does not reset). Repair signals and counters are cleared with
    /// the marking: a fresh pass starts with no step already escalated.
    Restart,
    /// End the conversation (e.g. a cancel/handoff digression). From the next
    /// turn boundary the stack governs nothing: see
    /// [`FlowStack::is_terminated`] and [`TERMINATED_STATE_KEY`].
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
    /// Escalate once the user has barged in this many times while the step
    /// is active (they keep cutting the model off: it is not working).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_after_interruptions: Option<u32>,
    /// Escalate once this many tool calls have failed or timed out while
    /// the step is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_after_tool_failures: Option<u32>,
}

impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            reprompt_after: default_reprompt_after(),
            escalate_after: default_escalate_after(),
            escalate_to: None,
            escalate_after_interruptions: None,
            escalate_after_tool_failures: None,
        }
    }
}

impl RepairPolicy {
    /// A policy with the given reprompt/escalate turn thresholds.
    pub fn new(reprompt_after: u32, escalate_after: u32) -> Self {
        Self {
            reprompt_after,
            escalate_after,
            ..Self::default()
        }
    }

    /// Route to `step` on escalation (also completes the current step).
    pub fn escalate_to(mut self, step: impl Into<String>) -> Self {
        self.escalate_to = Some(step.into());
        self
    }

    /// Also escalate after `n` barge-ins while the step is active.
    pub fn escalate_after_interruptions(mut self, n: u32) -> Self {
        self.escalate_after_interruptions = Some(n);
        self
    }

    /// Also escalate after `n` failed or timed-out tool calls while the step
    /// is active.
    pub fn escalate_after_tool_failures(mut self, n: u32) -> Self {
        self.escalate_after_tool_failures = Some(n);
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
///
/// A digression stays the active layer through the turn on which it
/// completes, so that turn's projection carries its closing instruction (the
/// terminal stage's posture — "hand off to a human now"), and its
/// [`Resume`] policy applies at the *next* turn boundary. Without that, a
/// digression that completes on its entry turn would never be seen at all.
pub struct FlowStack {
    main: FlowMonitor,
    mode: Enforcement,
    overlays: Vec<Overlay>,
    /// The digressions suspending the main flow, outermost first. The last
    /// one drives; each suspends the one beneath it.
    active: Vec<ActiveOverlay>,
    /// The digression that ended the conversation, once one has.
    terminated: Option<String>,
    /// Per-main-step repair policies.
    repair: BTreeMap<String, RepairPolicy>,
    /// Consecutive turns each main step has been active without completing.
    active_turns: BTreeMap<String, u32>,
    /// Per-step voice timing, keyed by step id in whichever layer is active.
    timing: BTreeMap<String, VoiceTiming>,
    /// Slots whose correction re-opens later stages, with the state keys to
    /// clear when that happens (e.g. the confirmation of a commit stage).
    corrections: BTreeMap<String, Vec<String>>,
    /// The last value seen for each watched slot.
    slot_values: BTreeMap<String, serde_json::Value>,
    /// Barge-ins each main step has seen while active.
    interruptions: BTreeMap<String, u32>,
    /// Failed tool calls each main step has seen while active.
    tool_failures: BTreeMap<String, u32>,
}

impl std::fmt::Debug for FlowStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowStack")
            .field("mode", &self.mode)
            .field("overlays", &self.overlays.len())
            .field(
                "active",
                &self.active.iter().map(|a| &a.name).collect::<Vec<_>>(),
            )
            .field("terminated", &self.terminated)
            .field("repair", &self.repair)
            .field("timing", &self.timing)
            .field("corrections", &self.corrections)
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
            active: Vec::new(),
            terminated: None,
            repair: BTreeMap::new(),
            active_turns: BTreeMap::new(),
            timing: BTreeMap::new(),
            corrections: BTreeMap::new(),
            slot_values: BTreeMap::new(),
            interruptions: BTreeMap::new(),
            tool_failures: BTreeMap::new(),
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

    /// Watch `slot` for corrections: when its value changes from one captured
    /// value to another, [`correction_flag`]`(slot)` is raised for that turn
    /// and `clear` keys are removed from state.
    ///
    /// Raising the flag does nothing by itself. The main flow reacts through
    /// a [`Constraint::Reset`](super::Constraint::Reset) gated on it, which
    /// un-latches the stages downstream of the slot so they run again with
    /// the corrected value. A `Conversation` lowers exactly that for every
    /// collected slot, clearing the confirmation of any commit stage it
    /// re-opens.
    pub fn with_correction(
        mut self,
        slot: impl Into<String>,
        clear: impl IntoIterator<Item = String>,
    ) -> Self {
        self.corrections
            .insert(slot.into(), clear.into_iter().collect());
        self
    }

    /// Watch several slots; see [`with_correction`](Self::with_correction).
    pub fn with_corrections(
        mut self,
        corrections: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        self.corrections.extend(corrections);
        self
    }

    /// The watched slots and the keys each correction clears.
    pub fn correction_policies(&self) -> &BTreeMap<String, Vec<String>> {
        &self.corrections
    }

    /// Attach voice timing to a step (main flow or digression).
    pub fn with_timing(mut self, step: impl Into<String>, timing: VoiceTiming) -> Self {
        self.timing.insert(step.into(), timing);
        self
    }

    /// Attach voice timing keyed by step.
    pub fn with_timings(
        mut self,
        timings: impl IntoIterator<Item = (String, VoiceTiming)>,
    ) -> Self {
        self.timing.extend(timings);
        self
    }

    /// The voice timing keyed by step.
    pub fn timing_policies(&self) -> &BTreeMap<String, VoiceTiming> {
        &self.timing
    }

    /// The merged timing of the steps active right now in the driving layer
    /// (empty when none of them has timing, or the conversation is over).
    pub fn active_timing(&self, state: &State) -> VoiceTiming {
        if self.timing.is_empty() || self.terminated.is_some() {
            return VoiceTiming::default();
        }
        self.current()
            .active_steps(state)
            .iter()
            .filter_map(|step| self.timing.get(&step.id))
            .fold(VoiceTiming::default(), |acc, t| acc.merge(t))
    }

    /// Publish [`active_timing`](Self::active_timing) to
    /// [`VOICE_TIMING_KEY`] in state, where the runtime's audio path, timers
    /// and turn lifecycle read it. Writes only on change; removes the key
    /// when no timing applies. The stack calls this itself after every turn
    /// and tool call; call it once when installing the stack.
    pub fn publish_timing(&self, state: &State) {
        if self.timing.is_empty() {
            return;
        }
        let timing = self.active_timing(state);
        let current = state.get::<VoiceTiming>(VOICE_TIMING_KEY);
        if timing.is_empty() {
            if current.is_some() {
                state.remove(VOICE_TIMING_KEY);
            }
        } else if current.as_ref() != Some(&timing) {
            let _ = state.set(VOICE_TIMING_KEY, &timing);
        }
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
        self.active.last().map_or(&self.main, |a| &a.monitor)
    }

    fn current_mut(&mut self) -> &mut FlowMonitor {
        match self.active.last_mut() {
            Some(a) => &mut a.monitor,
            None => &mut self.main,
        }
    }

    /// The name of the driving digression, if one is suspending the main flow.
    pub fn active_overlay(&self) -> Option<&str> {
        self.active.last().map(|a| a.name.as_str())
    }

    /// Every active digression, outermost first: a digression can itself be
    /// interrupted by another, which then drives until it completes.
    pub fn overlay_path(&self) -> Vec<&str> {
        self.active.iter().map(|a| a.name.as_str()).collect()
    }

    /// Whether the conversation is finished (main complete, or a `Terminate`
    /// digression ran).
    pub fn is_complete(&self) -> bool {
        self.terminated.is_some() || (self.active.is_empty() && self.main.is_complete())
    }

    /// Whether a `Terminate` digression ended the conversation. From then on
    /// the stack governs nothing: no active steps or postures, every tool
    /// denied. The runtime does not close the session by itself; the
    /// application reads this (or [`TERMINATED_STATE_KEY`]) and hangs up.
    pub fn is_terminated(&self) -> bool {
        self.terminated.is_some()
    }

    /// Why a tool is denied once the conversation has ended.
    fn termination_denial(&self) -> Option<String> {
        self.terminated
            .as_ref()
            .map(|by| format!("the conversation has ended: digression `{by}` terminated it"))
    }

    /// Whether the active digression has run to completion and is being
    /// projected for its closing turn.
    fn active_is_closing(&self) -> bool {
        self.active.last().is_some_and(|a| a.monitor.is_complete())
    }

    /// Index of the first overlay whose trigger holds against the main
    /// context and that is not already on the active path (a digression
    /// cannot interrupt itself).
    fn triggered(&self, state: &State) -> Option<usize> {
        self.overlays.iter().position(|ov| {
            !self.active.iter().any(|a| a.name == ov.name) && self.main.eval(&ov.trigger, state)
        })
    }

    /// Enter overlay `idx` on top of the active path, driving its first turn
    /// so single-step overlays latch. If that completes it, it still stays
    /// active for this turn's projection (see the type docs).
    fn enter(&mut self, idx: usize, state: &State) {
        let ov = &self.overlays[idx];
        let mut monitor = FlowMonitor::new(ov.flow.clone(), self.mode);
        monitor.on_turn(state);
        self.active.push(ActiveOverlay {
            name: ov.name.clone(),
            monitor,
            resume: ov.resume,
        });
    }

    /// Raise the correction flag of every watched slot whose value changed
    /// from one captured value to another since the last turn, clearing the
    /// keys its rule names. The main flow lowers the flags once it has
    /// advanced past them.
    fn detect_corrections(&mut self, state: &State) {
        if self.corrections.is_empty() {
            return;
        }
        for (slot, clear) in &self.corrections {
            let current = state.get_raw(slot);
            let corrected = matches!(
                (self.slot_values.get(slot), &current),
                (Some(before), Some(now)) if before != now
            );
            if corrected {
                let _ = state.set(correction_flag(slot), true);
                for key in clear {
                    state.remove(key);
                }
            }
            match current {
                Some(v) => {
                    self.slot_values.insert(slot.clone(), v);
                }
                None => {
                    self.slot_values.remove(slot);
                }
            }
        }
    }

    /// Count a barge-in against every active main step and escalate those
    /// whose policy's `escalate_after_interruptions` is reached. Called by
    /// the control plane when the user interrupts the model.
    pub fn on_interrupted(&mut self, state: &State) {
        self.count_against_active(
            state,
            |p| p.escalate_after_interruptions,
            |s| &mut s.interruptions,
        );
    }

    /// Count a failed (or timed-out) tool call against every active main step
    /// and escalate those whose policy's `escalate_after_tool_failures` is
    /// reached.
    pub fn on_tool_failed(&mut self, state: &State) {
        self.count_against_active(
            state,
            |p| p.escalate_after_tool_failures,
            |s| &mut s.tool_failures,
        );
    }

    fn count_against_active(
        &mut self,
        state: &State,
        threshold: impl Fn(&RepairPolicy) -> Option<u32>,
        counters: impl Fn(&mut Self) -> &mut BTreeMap<String, u32>,
    ) {
        if self.terminated.is_some() || !self.active.is_empty() || self.repair.is_empty() {
            return;
        }
        let steps: Vec<(String, u32)> = self
            .main
            .active_steps(state)
            .iter()
            .filter_map(|s| {
                self.repair
                    .get(&s.id)
                    .and_then(&threshold)
                    .map(|limit| (s.id.clone(), limit))
            })
            .collect();
        for (step, limit) in steps {
            let count = counters(self).entry(step.clone()).or_insert(0);
            *count += 1;
            if *count >= limit {
                let _ = state.set(escalate_flag(&step), true);
            }
        }
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
            self.interruptions.remove(&step);
            self.tool_failures.remove(&step);
            let _ = state.set(reprompt_flag(&step), false);
            // A step that completed *by escalating* must keep its escalate
            // signal: the authoring layer lowers `escalate_to` into an edge
            // gated on it, and gates are evaluated every turn, so clearing the
            // flag here would drop the hand-off target out of the active set
            // one turn after it was entered. Clear it only when the step left
            // the active set without completing (its gate closed). Steps a
            // `Constraint::Reset` un-latches are cleared in `advance_main`,
            // before the re-latch can see the stale signal.
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

    /// Forget everything the repair bookkeeping knows about `step`: its
    /// active-turn count and both signals. Used when a step is un-latched (a
    /// reset, or a restart of the whole main flow), because the lowered
    /// `escalate_to` completion guard references the escalate signal and would
    /// otherwise re-complete the step the moment it is re-latched.
    fn clear_repair(&mut self, step: &str, state: &State) {
        self.active_turns.remove(step);
        self.interruptions.remove(step);
        self.tool_failures.remove(step);
        let _ = state.set(reprompt_flag(step), false);
        let _ = state.set(escalate_flag(step), false);
    }

    /// Apply a closed digression's resume policy to the layer beneath it:
    /// the next digression on the path, or the main flow.
    fn apply_resume(&mut self, by: String, resume: Resume, state: &State) {
        match resume {
            // The suspended layer's marking was untouched — nothing to do.
            Resume::Previous => {}
            Resume::Restart => match self.active.last_mut() {
                Some(beneath) => beneath.monitor.restart(),
                None => {
                    self.main.restart();
                    // A fresh pass starts with no step already escalated.
                    let steps: Vec<String> = self.repair.keys().cloned().collect();
                    for step in steps {
                        self.clear_repair(&step, state);
                    }
                }
            },
            Resume::Terminate => {
                self.active.clear();
                self.terminated = Some(by);
            }
        }
    }

    /// Advance the main flow one turn: resets first (shedding the repair
    /// signals of any step they un-latch), then repair bookkeeping over the
    /// pre-turn active set, then the re-latch.
    fn advance_main(&mut self, state: &State) {
        for step in self.main.begin_turn(state) {
            self.clear_repair(&step, state);
        }
        // The main flow has now seen any correction raised since it last
        // advanced (its reset edges fired above): lower the flags so the
        // next correction is a fresh rising edge.
        for slot in self.corrections.keys() {
            let flag = correction_flag(slot);
            if state.get::<bool>(&flag) == Some(true) {
                let _ = state.set(&flag, false);
            }
        }
        // Repair bookkeeping is based on the pre-turn active set so an
        // escalation signal can take effect this turn.
        self.apply_repair(state);
        self.main.relatch(state);
    }

    /// Advance one turn.
    ///
    /// A digression that completed on the previous turn has had its closing
    /// turn projected; its resume policy applies now, and the turn then
    /// proceeds as if the main flow had been driving all along (a new
    /// digression may trigger, or the main flow advances). Otherwise: advance
    /// the active digression, enter a triggered one (suspending the main
    /// flow), or advance the main flow.
    pub fn on_turn(&mut self, state: &State) {
        self.advance_turn(state);
        self.publish_timing(state);
    }

    fn advance_turn(&mut self, state: &State) {
        if self.terminated.is_some() {
            return;
        }
        // A correction can come while a digression drives; its flag stays
        // raised until the main flow advances and sees it.
        self.detect_corrections(state);
        if self.active_is_closing() {
            let closed = self.active.pop().expect("checked above");
            self.apply_resume(closed.name, closed.resume, state);
            if self.terminated.is_some() {
                return;
            }
        }
        // A triggered digression suspends whichever layer is driving, the
        // main flow or another digression.
        if let Some(idx) = self.triggered(state) {
            self.enter(idx, state);
            return;
        }
        match self.active.last_mut() {
            Some(active) => active.monitor.on_turn(state),
            None => self.advance_main(state),
        }
    }

    /// Record a successful tool call against the active layer. A no-op once the
    /// conversation has been terminated: nothing is governing, so there is no
    /// marking for the call to advance. (In `Enforce` the call is denied before
    /// it runs; in `Observe` it runs but must not move a flow that has ended.)
    ///
    /// A tool can itself fire a reset (`reset(..).when(called_ok(..))`), so the
    /// main layer sheds the repair signals of whatever that un-latches, exactly
    /// as the main layer does at a turn boundary. Repair
    /// is tracked for the main flow only, so a digression just delegates.
    pub fn on_tool_ok(&mut self, tool: &str, state: &State) {
        self.advance_tool_ok(tool, state);
        self.publish_timing(state);
    }

    fn advance_tool_ok(&mut self, tool: &str, state: &State) {
        if self.terminated.is_some() {
            return;
        }
        match self.active.last_mut() {
            Some(active) => active.monitor.on_tool_ok(tool, state),
            None => {
                for step in self.main.begin_tool_ok(tool, state) {
                    self.clear_repair(&step, state);
                }
                // No `apply_repair` here: repair counters advance per turn, not
                // per tool call.
                self.main.relatch(state);
            }
        }
    }

    /// Observe a tool call for conformance against the active layer (see
    /// [`FlowMonitor::observe_tool`]).
    ///
    /// After termination nothing is advanced, but in `Observe` the call is
    /// still recorded as a deviation: a tool used after the conversation ended
    /// is exactly what that mode exists to catch, and the monitor cannot see it
    /// for itself because the denial is the stack's, not the flow's.
    pub fn observe_tool(&mut self, tool: &str, ok: bool, state: &State) {
        if let Some(denial) = self.termination_denial() {
            if self.mode == Enforcement::Observe {
                self.main.record_violation(tool, denial);
            }
            return;
        }
        // The conformance check is the *stack's*, not the active monitor's, and
        // the call is recorded through `Self::on_tool_ok` so a tool-fired reset
        // still sheds its repair signals.
        if self.mode == Enforcement::Observe
            && let Err(reason) = self.admits_tool(tool, state)
        {
            self.current_mut().record_violation(tool, reason);
        }
        if ok {
            self.on_tool_ok(tool, state);
        } else {
            self.on_tool_failed(state);
        }
    }

    /// Whether `tool` is admitted right now (delegates to the active layer).
    /// Every tool is denied once the conversation has been terminated.
    pub fn admits_tool(&self, tool: &str, state: &State) -> Result<(), String> {
        if let Some(denial) = self.termination_denial() {
            return Err(denial);
        }
        self.current().admits_tool(tool, state)
    }

    /// Explain the active layer's control-plane state. After termination:
    /// nothing active, nothing admitted, every tool blocked with the reason —
    /// the conversation is over, so it is waiting for nothing. To ask instead
    /// what it *never finished*, read [`main()`](Self::main): its marking and
    /// `unmet_requirements()` are kept intact for exactly that audit.
    pub fn explain(&self, state: &State) -> FlowExplanation {
        let mut ex = self.current().explain(state);
        if let Some(denial) = self.termination_denial() {
            ex.active.clear();
            ex.active_progress.clear();
            ex.missing_requirements.clear();
            for tool in std::mem::take(&mut ex.allowed_tools) {
                ex.blocked_tools.insert(tool, denial.clone());
            }
            for reason in ex.blocked_tools.values_mut() {
                *reason = denial.clone();
            }
        }
        ex
    }

    /// The active layer's marking (the last driving layer's, after
    /// termination — kept for audit).
    pub fn marking(&self) -> &Marking {
        self.current().marking()
    }

    /// Steps of the active layer that are eligible but not yet done. Empty
    /// after termination.
    pub fn active_steps(&self, state: &State) -> Vec<&Step> {
        if self.terminated.is_some() {
            return Vec::new();
        }
        self.current().active_steps(state)
    }

    /// Postures to project this turn: the active layer's active steps', or —
    /// on the turn a digression completes — its closing steps' (see
    /// [`FlowMonitor::closing_postures`]). Empty after termination.
    pub fn active_postures(&self, state: &State) -> Vec<String> {
        if self.terminated.is_some() {
            return Vec::new();
        }
        if self.active_is_closing() {
            return self.current().closing_postures();
        }
        self.current().active_postures(state)
    }

    /// Grounding lines to project this turn, chosen like
    /// [`active_postures`](Self::active_postures).
    pub fn active_grounds(&self, state: &State) -> Vec<String> {
        if self.terminated.is_some() {
            return Vec::new();
        }
        if self.active_is_closing() {
            return self.current().closing_grounds(state);
        }
        self.current().active_grounds(state)
    }

    /// The active layer's unmet requirements. Empty after termination.
    pub fn unmet_requirements(&self) -> Vec<String> {
        if self.terminated.is_some() {
            return Vec::new();
        }
        self.current().unmet_requirements()
    }

    /// Steps of the active layer that became active since the last call.
    pub fn take_newly_active(&mut self, state: &State) -> Vec<String> {
        if self.terminated.is_some() {
            return Vec::new();
        }
        self.current_mut().take_newly_active(state)
    }

    /// The `on_enter` action registered for a step of the active layer.
    pub fn enter_action(&self, step: &str) -> Option<&StepAction> {
        if self.terminated.is_some() {
            return None;
        }
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

    fn overlay(name: &str, trigger: &str, done_key: &str, resume: Resume) -> Overlay {
        let end = format!("{name}_end");
        let flow = Flow::new()
            .step(format!("{name}_step"))
            .done(Guard::is_true(done_key))
            .step(&end)
            .after(format!("{name}_step"))
            .terminal()
            .require([end.clone()])
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        Overlay::new(name, Guard::is_true(trigger), flow, resume)
    }

    #[test]
    fn a_digression_can_be_interrupted_by_another() {
        let state = State::new();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce)
            .with_overlay(overlay("faq", "intent:faq", "faq_done", Resume::Previous))
            .with_overlay(overlay(
                "clarify",
                "intent:clarify",
                "clarified",
                Resume::Previous,
            ));

        let _ = state.set("intent:faq", true);
        stack.on_turn(&state);
        assert_eq!(stack.overlay_path(), ["faq"]);

        // Mid-FAQ the user needs a clarification: it nests on top.
        let _ = state.set("intent:clarify", true);
        stack.on_turn(&state);
        assert_eq!(stack.overlay_path(), ["faq", "clarify"]);
        assert_eq!(stack.active_overlay(), Some("clarify"));

        // The clarification completes (closing turn), then FAQ drives again.
        let _ = state.set("intent:clarify", false);
        let _ = state.set("clarified", true);
        stack.on_turn(&state);
        assert_eq!(stack.overlay_path(), ["faq", "clarify"], "closing turn");
        stack.on_turn(&state);
        assert_eq!(stack.overlay_path(), ["faq"]);

        // FAQ completes; the main flow resumes where it was.
        let _ = state.set("intent:faq", false);
        let _ = state.set("faq_done", true);
        stack.on_turn(&state);
        stack.on_turn(&state);
        assert!(stack.overlay_path().is_empty());
        assert_eq!(stack.explain(&state).active, ["a"]);
    }

    #[test]
    fn a_nested_terminate_ends_the_whole_conversation() {
        let state = State::new();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce)
            .with_overlay(overlay("faq", "intent:faq", "faq_done", Resume::Previous))
            .with_overlay(overlay(
                "cancel",
                "intent:cancel",
                "cancelled",
                Resume::Terminate,
            ));
        let _ = state.set("intent:faq", true);
        stack.on_turn(&state);
        let _ = state.set("intent:cancel", true);
        let _ = state.set("cancelled", true);
        stack.on_turn(&state);
        assert_eq!(stack.overlay_path(), ["faq", "cancel"]);
        stack.on_turn(&state);
        assert!(stack.is_terminated());
        assert!(stack.overlay_path().is_empty());
    }

    #[test]
    fn a_digression_does_not_re_enter_itself() {
        let state = State::new();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce).with_overlay(overlay(
            "faq",
            "intent:faq",
            "faq_done",
            Resume::Previous,
        ));
        let _ = state.set("intent:faq", true);
        stack.on_turn(&state);
        stack.on_turn(&state);
        assert_eq!(
            stack.overlay_path(),
            ["faq"],
            "the trigger still holding does not stack it twice"
        );
    }

    #[test]
    fn barge_ins_and_tool_failures_escalate_the_active_step() {
        let state = State::new();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce).with_repair(
            "a",
            RepairPolicy::new(10, 10)
                .escalate_after_interruptions(2)
                .escalate_after_tool_failures(3),
        );
        stack.on_turn(&state);
        stack.on_interrupted(&state);
        assert_eq!(state.get::<bool>(&escalate_flag("a")), None);
        stack.on_interrupted(&state);
        assert_eq!(state.get::<bool>(&escalate_flag("a")), Some(true));

        let state = State::new();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce).with_repair(
            "a",
            RepairPolicy::new(10, 10).escalate_after_tool_failures(2),
        );
        stack.on_turn(&state);
        stack.observe_tool("lookup", false, &state);
        stack.observe_tool("lookup", false, &state);
        assert_eq!(state.get::<bool>(&escalate_flag("a")), Some(true));
    }

    #[test]
    fn a_corrected_slot_raises_its_flag_once_and_clears_its_keys() {
        let state = State::new();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce)
            .with_correction("party_size", ["confirmed".to_string()]);
        let _ = state.set("party_size", 4);
        let _ = state.set("confirmed", true);
        stack.on_turn(&state);
        assert_eq!(
            state.get::<bool>(&correction_flag("party_size")),
            None,
            "first value is not a correction"
        );

        let _ = state.set("party_size", 5);
        stack.detect_corrections(&state);
        assert_eq!(
            state.get::<bool>(&correction_flag("party_size")),
            Some(true)
        );
        assert_eq!(
            state.get::<bool>("confirmed"),
            None,
            "the confirmation is cleared"
        );

        // The main flow advances past it: the flag drops for the next edge.
        stack.on_turn(&state);
        assert_eq!(
            state.get::<bool>(&correction_flag("party_size")),
            Some(false)
        );
    }

    #[test]
    fn timing_follows_the_active_step_and_digression() {
        use std::time::Duration;
        let state = State::new();
        let a = VoiceTiming::new().reprompt_after(Duration::from_secs(6));
        let answer = VoiceTiming::new().uninterruptible();
        let mut stack = FlowStack::new(main_flow(), Enforcement::Enforce)
            .with_overlay(faq_overlay())
            .with_timing("a", a.clone())
            .with_timing("answer", answer.clone());

        stack.publish_timing(&state);
        assert_eq!(state.get::<VoiceTiming>(VOICE_TIMING_KEY), Some(a.clone()));

        // A digression takes over: its step's timing applies.
        let _ = state.set("intent:faq", true);
        stack.on_turn(&state);
        assert_eq!(state.get::<VoiceTiming>(VOICE_TIMING_KEY), Some(answer));

        // Back in the main flow, past `a`: `b` has no timing, so none applies.
        let _ = state.set("intent:faq", false);
        let _ = state.set("faq_answered", true);
        stack.on_turn(&state);
        stack.on_turn(&state);
        assert_eq!(state.get::<VoiceTiming>(VOICE_TIMING_KEY), Some(a));
        let _ = state.set("a_done", true);
        stack.on_turn(&state);
        assert_eq!(state.get::<VoiceTiming>(VOICE_TIMING_KEY), None);
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

        // The turn the digression completes it is still the projected layer
        // (its closing turn); the main flow is untouched underneath.
        let _ = state.set("faq_answered", true);
        let _ = state.set("intent:faq", false);
        stack.on_turn(&state);
        assert_eq!(stack.active_overlay(), Some("faq"));
        assert!(stack.explain(&state).active.is_empty());
        assert!(stack.main().marking().done.is_empty());

        // Next boundary: resumed exactly where it was, and that same turn
        // advances the main flow.
        let _ = state.set("a_done", true);
        stack.on_turn(&state);
        assert!(stack.active_overlay().is_none());
        assert!(stack.main().marking().done.contains("a"));
        assert!(stack.explain(&state).active.is_empty());
        assert!(stack.is_complete());
    }

    /// A digression that completes on its entry turn (the shape the safety
    /// hand-off policy lowers to: one terminal stage) must still be seen: it
    /// is the projected layer for that turn, its closing instruction is what
    /// the model is told, and the main flow's tools are not admitted.
    #[test]
    fn entry_complete_digression_is_projected_before_it_resumes() {
        let state = State::new();
        let main = Flow::new()
            .step("a")
            .allow(["main_tool"])
            .posture("MAIN")
            .done(Guard::is_true("a_done"))
            .step("b")
            .after("a")
            .terminal()
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        // The digression governs its closing turn, so what it says about tools
        // is what holds: this one locks the main flow's tool outright.
        let handoff = Flow::new()
            .step("handoff")
            .posture("HAND OFF NOW")
            .terminal()
            .require(["handoff"])
            .never("main_tool")
            .until(Guard::is_true("human_joined"))
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        let mut stack = FlowStack::new(main, Enforcement::Enforce).with_overlay(Overlay::new(
            "safety",
            Guard::is_true("intent:abuse"),
            handoff,
            Resume::Previous,
        ));
        assert_eq!(stack.active_postures(&state), ["MAIN"]);

        let _ = state.set("intent:abuse", true);
        stack.on_turn(&state);
        assert_eq!(stack.active_overlay(), Some("safety"));
        assert_eq!(stack.active_postures(&state), ["HAND OFF NOW"]);
        // Complete, so nothing is *active* — but it is still the governing layer.
        assert!(stack.active_steps(&state).is_empty());
        assert!(stack.admits_tool("main_tool", &state).is_err());
        assert!(!stack.is_complete());

        let _ = state.set("intent:abuse", false);
        stack.on_turn(&state);
        assert!(stack.active_overlay().is_none());
        assert_eq!(stack.active_postures(&state), ["MAIN"]);
        assert!(stack.admits_tool("main_tool", &state).is_ok());
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
        stack.on_turn(&state); // closing turn
        assert_eq!(stack.active_overlay(), Some("cancel"));
        assert!(stack.main().marking().done.contains("a"));

        // Next boundary: the monitor restarts and re-latches in the same turn.
        // State is untouched, so `a` completes again from the same facts.
        stack.on_turn(&state);
        assert!(stack.active_overlay().is_none());
        assert_eq!(state.get::<bool>("a_done"), Some(true));
        assert!(stack.main().marking().done.contains("a"));
        assert_eq!(stack.main().marking().turns, 1);
    }

    /// A restart is a fresh pass: a step that had escalated must not start
    /// the new pass already escalated. The lowered completion guard references
    /// the escalate signal, so a stale signal would complete it on re-latch.
    #[test]
    fn restart_clears_repair_signals() {
        let state = State::new();
        let main = escalating_flow();
        let cancel = {
            let flow = Flow::new()
                .step("bye")
                .terminal()
                .require(["bye"])
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
        let mut stack = FlowStack::new(main, Enforcement::Enforce)
            .with_overlay(cancel)
            .with_repair("collect", RepairPolicy::new(1, 2).escalate_to("handoff"));
        stack.on_turn(&state);
        stack.on_turn(&state); // escalated: collect done, handoff active
        assert_eq!(stack.explain(&state).active, ["handoff"]);

        let _ = state.set("intent:cancel", true);
        stack.on_turn(&state); // enters and completes the digression
        let _ = state.set("intent:cancel", false);
        stack.on_turn(&state); // restart + re-latch
        assert!(stack.active_overlay().is_none());
        assert_eq!(stack.explain(&state).active, ["collect"]);
        assert_eq!(state.get::<bool>(&escalate_flag("collect")), Some(false));
        // And it can escalate again, from a fresh count.
        stack.on_turn(&state);
        assert_eq!(stack.explain(&state).active, ["handoff"]);
    }

    /// A `Terminate` digression is projected for its closing turn (the model is
    /// told to say goodbye), and from the next boundary the stack governs
    /// nothing: no steps, no postures, every tool denied, further turns no-ops.
    #[test]
    fn terminate_ends_the_conversation() {
        let state = State::new();
        let main = Flow::new()
            .step("a")
            .allow(["main_tool"])
            .posture("MAIN")
            .done(Guard::is_true("a_done"))
            .step("b")
            .after("a")
            .terminal()
            .require(["b"])
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        let bye = {
            let flow = Flow::new()
                .step("bye")
                .posture("SAY GOODBYE")
                .terminal()
                .require(["bye"])
                .build()
                .expect("valid")
                .compile()
                .expect("compiles");
            Overlay::new("bye", Guard::is_true("intent:bye"), flow, Resume::Terminate)
        };
        let mut stack = FlowStack::new(main, Enforcement::Enforce).with_overlay(bye);
        let _ = state.set("intent:bye", true);
        stack.on_turn(&state);
        assert_eq!(stack.active_overlay(), Some("bye"));
        assert_eq!(stack.active_postures(&state), ["SAY GOODBYE"]);
        assert!(!stack.is_terminated());
        assert!(!stack.is_complete());

        stack.on_turn(&state);
        assert!(stack.is_terminated());
        assert!(stack.is_complete());
        assert!(stack.active_overlay().is_none());
        assert!(stack.active_steps(&state).is_empty());
        assert!(stack.active_postures(&state).is_empty());
        assert!(stack.unmet_requirements().is_empty());
        let denied = stack.admits_tool("main_tool", &state).unwrap_err();
        assert!(denied.contains("terminated"), "{denied}");
        assert!(denied.contains("bye"), "{denied}");
        let ex = stack.explain(&state);
        assert!(ex.active.is_empty());
        assert!(ex.allowed_tools.is_empty());
        assert_eq!(ex.blocked_tools.get("main_tool"), Some(&denied));

        // Later turns change nothing, whatever state says — and neither does a
        // tool that slipped through (only possible in `Observe`): a flow that
        // has ended must not keep advancing.
        let _ = state.set("a_done", true);
        stack.on_turn(&state);
        assert!(stack.main().marking().done.is_empty());
        stack.on_tool_ok("main_tool", &state);
        stack.observe_tool("main_tool", true, &state);
        assert!(stack.main().marking().done.is_empty());
        assert!(stack.admits_tool("main_tool", &state).is_err());
    }

    /// `Observe` exists to record what a session did wrong without blocking it.
    /// A tool called after the conversation ended is exactly that, and the
    /// monitor cannot see it — the denial belongs to the stack — so the stack
    /// records it. The marking still does not move.
    #[test]
    fn observe_mode_records_a_tool_used_after_termination() {
        let state = State::new();
        let bye = {
            let flow = Flow::new()
                .step("bye")
                .terminal()
                .require(["bye"])
                .build()
                .expect("valid")
                .compile()
                .expect("compiles");
            Overlay::new("bye", Guard::is_true("intent:bye"), flow, Resume::Terminate)
        };
        let mut stack = FlowStack::new(main_flow(), Enforcement::Observe).with_overlay(bye);
        let _ = state.set("intent:bye", true);
        stack.on_turn(&state); // closing turn
        stack.on_turn(&state); // terminated
        assert!(stack.is_terminated());

        let _ = state.set("a_done", true);
        stack.observe_tool("anything", true, &state);
        let violations = stack.main().violations();
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].subject, "anything");
        assert!(
            violations[0].reason.contains("terminated"),
            "{violations:?}"
        );
        // Recorded, not acted on: the ended flow did not advance.
        assert!(stack.main().marking().done.is_empty());
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

    /// The shape the conversation compiler lowers for `escalate_to`: `collect`
    /// completes on `info` or on its escalate flag; `handoff` follows, gated on
    /// the flag; `done` follows `collect` only when it completed properly.
    fn escalating_flow() -> CompiledFlow {
        Flow::new()
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
            .expect("compiles")
    }

    /// `escalate_to` lowers to an edge gated on the escalate signal. Gates are
    /// evaluated every turn, so the signal must stay latched once the step
    /// has completed by escalating, or the hand-off target is active for one
    /// turn and then vanishes.
    #[test]
    fn escalation_edge_stays_open_after_the_step_completes() {
        let state = State::new();
        let mut stack = FlowStack::new(escalating_flow(), Enforcement::Enforce)
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

    /// A `Constraint::Reset` that un-latches an escalated step must also shed
    /// its latched escalate signal, before the re-latch: the lowered
    /// completion guard references that signal and would complete the step
    /// again on the spot, routing straight back to the hand-off target.
    #[test]
    fn reset_clears_a_latched_escalation() {
        let state = State::new();
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
            .reset(["collect"])
            .when(Guard::is_true("retry"))
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        let mut stack = FlowStack::new(main, Enforcement::Enforce)
            .with_repair("collect", RepairPolicy::new(1, 2).escalate_to("handoff"));

        stack.on_turn(&state);
        stack.on_turn(&state); // escalated
        assert_eq!(stack.explain(&state).active, ["handoff"]);

        // The reset fires: collect is back, handoff is gone, the count restarts.
        let _ = state.set("retry", true);
        stack.on_turn(&state);
        assert_eq!(stack.explain(&state).active, ["collect"]);
        assert_eq!(state.get::<bool>(&escalate_flag("collect")), Some(false));
        assert_eq!(state.get::<bool>(&reprompt_flag("collect")), Some(true));

        // A second stall escalates again from that fresh count.
        stack.on_turn(&state);
        assert_eq!(stack.explain(&state).active, ["handoff"]);
        assert_eq!(state.get::<bool>(&escalate_flag("collect")), Some(true));
    }

    /// A reset can be gated on a *tool*, not just a state flag — `reset(..)
    /// .when(called_ok("start_over"))` is the natural "start over" button. That
    /// edge fires inside `on_tool_ok`, not at a turn boundary, so the repair
    /// signals must be shed there too, or the escalated step re-completes on its
    /// own stale flag exactly as it would at a turn boundary.
    #[test]
    fn a_tool_triggered_reset_clears_a_latched_escalation() {
        let state = State::new();
        let main = Flow::new()
            .step("collect")
            .allow(["start_over"])
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
            .reset(["collect"])
            .when(Guard::called_ok("start_over"))
            .build()
            .expect("valid")
            .compile()
            .expect("compiles");
        let mut stack = FlowStack::new(main, Enforcement::Enforce)
            .with_repair("collect", RepairPolicy::new(1, 2).escalate_to("handoff"));

        stack.on_turn(&state);
        stack.on_turn(&state); // escalated
        assert_eq!(stack.explain(&state).active, ["handoff"]);

        // The caller hits "start over" mid-turn.
        stack.on_tool_ok("start_over", &state);
        assert_eq!(state.get::<bool>(&escalate_flag("collect")), Some(false));
        assert_eq!(stack.explain(&state).active, ["collect"]);

        // And it stays reset across the next boundary rather than snapping back.
        stack.on_turn(&state);
        assert_eq!(stack.explain(&state).active, ["collect"]);
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
