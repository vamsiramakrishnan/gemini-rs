//! `Flow` — a governed conversation/tool DAG.
//!
//! A [`Flow`] is a directed acyclic graph of [`Step`]s. A `Step` is the *only*
//! node type; it unifies "conversation stage" and "tool-call milestone" by
//! differing only in attributes, not in kind. A step is *done* when its
//! completion [`Guard`] latches true; edges (`after`) are dependencies. The
//! [`FlowMonitor`] maintains a [`Marking`] (the set of done steps) by observing
//! the session trace, **projects** active steps' postures into turn-boundary
//! steering, and **enforces** ordering by admitting/denying tool calls.
//!
//! The vocabulary is deliberately closed (see the crate docs / RFC): the only
//! nouns are `Flow`, `Step`, `Guard`, `Posture`, `Marking`, `Verdict`. Words
//! like *phase*, *transition*, *watch*, *needs* are lowering details and never
//! appear here.
//!
//! Because every [`Guard`] atom is a named, parameterized predicate, a `Flow`
//! is fully serializable — enabling data-driven scripts edited without a
//! recompile. The `custom` closure escape hatch is available in code but is not
//! serializable.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::state::State;

/// Evaluation context handed to a [`Guard`]: the session state plus the
/// current flow marking.
pub struct FlowCtx<'a> {
    /// The session state.
    pub state: &'a State,
    /// The current flow marking (done steps + tool-call counts).
    pub marking: &'a Marking,
}

/// A serializable predicate atom — the closed set of guard primitives.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Pred {
    /// Always true.
    Always,
    /// State key holds boolean `true`.
    IsTrue(String),
    /// State key is present.
    IsSet(String),
    /// State key equals the given JSON value.
    Eq(String, Value),
    /// All of the given state keys are present (e.g. extracted slots).
    Captured(Vec<String>),
    /// The named tool has completed successfully at least once.
    CalledOk(String),
    /// The named step is done.
    Done(String),
    /// Conjunction.
    All(Vec<Pred>),
    /// Disjunction.
    Any(Vec<Pred>),
    /// Negation.
    Not(Box<Pred>),
}

impl Pred {
    fn eval(&self, ctx: &FlowCtx) -> bool {
        match self {
            Pred::Always => true,
            Pred::IsTrue(k) => ctx.state.get::<bool>(k) == Some(true),
            Pred::IsSet(k) => ctx.state.contains(k),
            Pred::Eq(k, v) => ctx.state.get::<Value>(k).as_ref() == Some(v),
            Pred::Captured(fields) => fields.iter().all(|f| ctx.state.contains(f)),
            Pred::CalledOk(t) => ctx.marking.tool_ok.contains_key(t),
            Pred::Done(s) => ctx.marking.done.contains(s),
            Pred::All(ps) => ps.iter().all(|p| p.eval(ctx)),
            Pred::Any(ps) => ps.iter().any(|p| p.eval(ctx)),
            Pred::Not(p) => !p.eval(ctx),
        }
    }

    /// Step ids referenced by `Done(..)` atoms (for validation).
    fn referenced_steps(&self, out: &mut Vec<String>) {
        match self {
            Pred::Done(s) => out.push(s.clone()),
            Pred::All(ps) | Pred::Any(ps) => ps.iter().for_each(|p| p.referenced_steps(out)),
            Pred::Not(p) => p.referenced_steps(out),
            _ => {}
        }
    }
}

type CustomFn = Arc<dyn Fn(&FlowCtx) -> bool + Send + Sync>;

/// A boolean predicate over `(state, marking)` — the *only* predicate type.
///
/// Use the constructors ([`Guard::is_true`], [`Guard::captured`],
/// [`Guard::called_ok`], …) for the serializable closed atoms, or
/// [`Guard::custom`] for a bespoke closure (not serializable).
#[derive(Clone)]
pub enum Guard {
    /// A serializable predicate built from the closed atom set.
    Spec(Pred),
    /// A code-only escape hatch. Not serializable.
    Custom(CustomFn),
}

impl Guard {
    /// Always true.
    pub fn always() -> Self {
        Guard::Spec(Pred::Always)
    }
    /// State key holds boolean `true`.
    pub fn is_true(key: impl Into<String>) -> Self {
        Guard::Spec(Pred::IsTrue(key.into()))
    }
    /// State key is present.
    pub fn is_set(key: impl Into<String>) -> Self {
        Guard::Spec(Pred::IsSet(key.into()))
    }
    /// State key equals the given JSON value.
    pub fn eq(key: impl Into<String>, value: impl Into<Value>) -> Self {
        Guard::Spec(Pred::Eq(key.into(), value.into()))
    }
    /// All of the given state keys are present (extracted slots).
    pub fn captured<I, S>(fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Guard::Spec(Pred::Captured(fields.into_iter().map(Into::into).collect()))
    }
    /// The named tool has completed successfully.
    pub fn called_ok(tool: impl Into<String>) -> Self {
        Guard::Spec(Pred::CalledOk(tool.into()))
    }
    /// The named step is done.
    pub fn done(step: impl Into<String>) -> Self {
        Guard::Spec(Pred::Done(step.into()))
    }
    /// Conjunction.
    pub fn all(guards: impl IntoIterator<Item = Guard>) -> Self {
        Guard::Spec(Pred::All(collect_specs(guards)))
    }
    /// Disjunction.
    pub fn any(guards: impl IntoIterator<Item = Guard>) -> Self {
        Guard::Spec(Pred::Any(collect_specs(guards)))
    }
    /// Negation of a serializable atom.
    #[allow(clippy::should_implement_trait)]
    pub fn not(guard: Guard) -> Self {
        match guard {
            Guard::Spec(p) => Guard::Spec(Pred::Not(Box::new(p))),
            // Negating a custom guard yields a custom guard.
            Guard::Custom(f) => Guard::Custom(Arc::new(move |ctx| !f(ctx))),
        }
    }
    /// A bespoke closure over `(state, marking)`. Not serializable.
    pub fn custom(f: impl Fn(&FlowCtx) -> bool + Send + Sync + 'static) -> Self {
        Guard::Custom(Arc::new(f))
    }

    /// Evaluate the guard.
    pub fn eval(&self, ctx: &FlowCtx) -> bool {
        match self {
            Guard::Spec(p) => p.eval(ctx),
            Guard::Custom(f) => f(ctx),
        }
    }

    fn referenced_steps(&self, out: &mut Vec<String>) {
        if let Guard::Spec(p) = self {
            p.referenced_steps(out);
        }
    }
}

fn collect_specs(guards: impl IntoIterator<Item = Guard>) -> Vec<Pred> {
    guards
        .into_iter()
        .map(|g| match g {
            Guard::Spec(p) => p,
            // Custom guards can't live inside a serializable combinator; treat
            // as opaque-always for serialization purposes. (Compose custom
            // guards at the top level instead.)
            Guard::Custom(_) => Pred::Always,
        })
        .collect()
}

impl Serialize for Guard {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Guard::Spec(p) => p.serialize(s),
            Guard::Custom(_) => Err(serde::ser::Error::custom(
                "custom guards are not serializable; use Guard atoms for data-driven flows",
            )),
        }
    }
}

impl<'de> Deserialize<'de> for Guard {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Guard::Spec(Pred::deserialize(d)?))
    }
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Guard::Spec(p) => write!(f, "{p:?}"),
            Guard::Custom(_) => write!(f, "Custom(<fn>)"),
        }
    }
}

/// A node in the flow DAG — the only node type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    /// Unique step id.
    pub id: String,
    /// Dependency step ids; this step is only eligible once all are done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    /// Extra eligibility predicate beyond dependencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<Guard>,
    /// Completion condition. Required for non-terminal steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<Guard>,
    /// Instruction imposed while this step is active (projected as steering).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posture: Option<String>,
    /// Tools available while this step is active (whitelist; empty = no restriction).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Tools forbidden while this step is active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    /// A terminal step — reaching it (deps + gate) marks it done with no milestone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

/// A cross-cutting flow constraint.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Constraint {
    /// A tool may complete at most once.
    Once(String),
    /// Step `0` must be done before step `1` starts.
    Before(String, String),
    /// A tool is forbidden until the guard holds.
    NeverUntil {
        /// The gated tool.
        tool: String,
        /// The guard that must hold to permit it.
        until: Guard,
    },
    /// These steps must be done for the flow to be complete.
    Require(Vec<String>),
}

/// A governed conversation/tool DAG.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Flow {
    /// The steps (DAG nodes).
    pub steps: Vec<Step>,
    /// Cross-cutting constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,
    /// Tools that require confirmation when reached (set by `commit`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirm_tools: Vec<String>,
}

impl Flow {
    /// Start building a flow.
    pub fn new() -> FlowBuilder {
        FlowBuilder::default()
    }

    fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    /// Validate referential integrity and acyclicity.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        let ids: BTreeSet<&str> = self.steps.iter().map(|s| s.id.as_str()).collect();
        if ids.len() != self.steps.len() {
            errs.push("duplicate step ids".into());
        }
        for s in &self.steps {
            for d in &s.after {
                if !ids.contains(d.as_str()) {
                    errs.push(format!("step '{}' depends on unknown step '{}'", s.id, d));
                }
            }
            if !s.terminal && s.done.is_none() {
                errs.push(format!(
                    "non-terminal step '{}' has no `done` condition (it can never complete)",
                    s.id
                ));
            }
            for g in s.gate.iter().chain(s.done.iter()) {
                let mut refs = Vec::new();
                g.referenced_steps(&mut refs);
                for r in refs {
                    if !ids.contains(r.as_str()) {
                        errs.push(format!(
                            "step '{}' guard references unknown step '{r}'",
                            s.id
                        ));
                    }
                }
            }
        }
        for c in &self.constraints {
            match c {
                Constraint::Before(a, b) => {
                    for x in [a, b] {
                        if !ids.contains(x.as_str()) {
                            errs.push(format!("constraint `before` references unknown step '{x}'"));
                        }
                    }
                }
                Constraint::Require(rs) => {
                    for r in rs {
                        if !ids.contains(r.as_str()) {
                            errs.push(format!(
                                "constraint `require` references unknown step '{r}'"
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        if self.has_cycle() {
            errs.push("flow dependency graph has a cycle (must be a DAG)".into());
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }

    fn has_cycle(&self) -> bool {
        // DFS with colors over the `after` dependency edges.
        let mut color: BTreeMap<&str, u8> = BTreeMap::new();
        fn dfs<'a>(flow: &'a Flow, id: &'a str, color: &mut BTreeMap<&'a str, u8>) -> bool {
            color.insert(id, 1);
            if let Some(step) = flow.step(id) {
                for d in &step.after {
                    match color.get(d.as_str()).copied() {
                        Some(1) => return true,
                        Some(2) => {}
                        _ => {
                            if dfs(flow, d, color) {
                                return true;
                            }
                        }
                    }
                }
            }
            color.insert(id, 2);
            false
        }
        for s in &self.steps {
            if color.get(s.id.as_str()).copied().unwrap_or(0) == 0 && dfs(self, &s.id, &mut color) {
                return true;
            }
        }
        false
    }

    /// Render the flow as a Mermaid `flowchart` — the spec *is* the diagram.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("flowchart TD\n");
        for s in &self.steps {
            let shape = if s.terminal {
                format!("    {}([{}])\n", s.id, s.id)
            } else {
                format!("    {}[{}]\n", s.id, s.id)
            };
            out.push_str(&shape);
        }
        for s in &self.steps {
            for d in &s.after {
                out.push_str(&format!("    {d} --> {}\n", s.id));
            }
        }
        out
    }
}

/// The runtime position in a flow: which steps are done and how often each
/// tool has succeeded.
#[derive(Clone, Debug, Default)]
pub struct Marking {
    /// Steps that have latched done.
    pub done: BTreeSet<String>,
    /// Per-tool successful-completion counts.
    pub tool_ok: BTreeMap<String, u32>,
    /// Turns observed.
    pub turns: u32,
}

/// The conformance status of a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Not yet eligible.
    Pending,
    /// Eligible and awaiting completion.
    Active,
    /// Completed.
    Done,
    /// A successor completed while this never did (an out-of-order deviation).
    Skipped,
}

/// A recorded conformance deviation (observe mode) or denial (enforce mode).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Violation {
    /// What was attempted (e.g. a tool name).
    pub subject: String,
    /// Why it was a violation.
    pub reason: String,
}

/// Enforcement vs observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Block inadmissible tool calls and steer back on-path.
    #[default]
    Enforce,
    /// Allow everything, but record deviations for audit/analytics.
    Observe,
}

/// Observes the session trace, maintains the [`Marking`], answers tool
/// admissibility, and projects active postures.
pub struct FlowMonitor {
    flow: Flow,
    mode: Mode,
    marking: Marking,
    violations: Vec<Violation>,
}

impl FlowMonitor {
    /// Create a monitor for a (validated) flow.
    pub fn new(flow: Flow, mode: Mode) -> Self {
        Self {
            flow,
            mode,
            marking: Marking::default(),
            violations: Vec::new(),
        }
    }

    /// The mode this monitor runs in.
    pub fn mode(&self) -> Mode {
        self.mode
    }
    /// The current marking.
    pub fn marking(&self) -> &Marking {
        &self.marking
    }
    /// Recorded violations.
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }
    /// The underlying flow.
    pub fn flow(&self) -> &Flow {
        &self.flow
    }

    fn ctx<'a>(&'a self, state: &'a State) -> FlowCtx<'a> {
        FlowCtx {
            state,
            marking: &self.marking,
        }
    }

    fn eligible(&self, step: &Step, state: &State) -> bool {
        let deps_done = step.after.iter().all(|d| self.marking.done.contains(d));
        let gate_ok = step
            .gate
            .as_ref()
            .map(|g| g.eval(&self.ctx(state)))
            .unwrap_or(true);
        deps_done && gate_ok
    }

    /// Re-evaluate completion latches to a fixpoint. Call after any event that
    /// can change state or the marking (turn boundary, tool completion).
    pub fn relatch(&mut self, state: &State) {
        loop {
            let mut newly_done: Vec<String> = Vec::new();
            for s in &self.flow.steps {
                if self.marking.done.contains(&s.id) {
                    continue;
                }
                if !self.eligible(s, state) {
                    continue;
                }
                let complete = if s.terminal {
                    true
                } else {
                    s.done
                        .as_ref()
                        .map(|g| g.eval(&self.ctx(state)))
                        .unwrap_or(false)
                };
                if complete {
                    newly_done.push(s.id.clone());
                }
            }
            if newly_done.is_empty() {
                break;
            }
            for id in newly_done {
                self.marking.done.insert(id);
            }
        }
    }

    /// Record a turn boundary, then re-latch.
    pub fn on_turn(&mut self, state: &State) {
        self.marking.turns += 1;
        self.relatch(state);
    }

    /// Record a successful tool call, then re-latch.
    pub fn on_tool_ok(&mut self, tool: &str, state: &State) {
        *self.marking.tool_ok.entry(tool.to_string()).or_insert(0) += 1;
        self.relatch(state);
    }

    /// Steps that are eligible but not yet done.
    pub fn active_steps(&self, state: &State) -> Vec<&Step> {
        self.flow
            .steps
            .iter()
            .filter(|s| !self.marking.done.contains(&s.id) && self.eligible(s, state))
            .collect()
    }

    /// Postures of the active steps — to inject as turn-boundary steering.
    pub fn active_postures(&self, state: &State) -> Vec<String> {
        self.active_steps(state)
            .into_iter()
            .filter_map(|s| s.posture.clone())
            .collect()
    }

    /// Required steps not yet done (drives repair).
    pub fn unmet_requirements(&self) -> Vec<String> {
        self.flow
            .constraints
            .iter()
            .flat_map(|c| match c {
                Constraint::Require(rs) => rs.clone(),
                _ => Vec::new(),
            })
            .filter(|r| !self.marking.done.contains(r))
            .collect()
    }

    /// Whether all required steps are done.
    pub fn is_complete(&self) -> bool {
        self.unmet_requirements().is_empty()
    }

    /// The conformance verdict for a step.
    pub fn verdict(&self, step_id: &str, state: &State) -> Verdict {
        if self.marking.done.contains(step_id) {
            return Verdict::Done;
        }
        if let Some(step) = self.flow.step(step_id) {
            if self.eligible(step, state) {
                return Verdict::Active;
            }
        }
        // Skipped: a successor is done but this step never completed.
        let bypassed = self
            .flow
            .steps
            .iter()
            .any(|s| s.after.iter().any(|d| d == step_id) && self.marking.done.contains(&s.id));
        if bypassed {
            Verdict::Skipped
        } else {
            Verdict::Pending
        }
    }

    /// Decide whether a tool call may proceed. `Ok(())` admits it; `Err(reason)`
    /// denies it (the caller blocks in Enforce mode, or records in Observe).
    pub fn admits_tool(&self, tool: &str, state: &State) -> Result<(), String> {
        // 1. once(tool)
        for c in &self.flow.constraints {
            if let Constraint::Once(t) = c {
                if t == tool && self.marking.tool_ok.contains_key(tool) {
                    return Err(format!("'{tool}' may run at most once"));
                }
            }
        }
        // 2. never(tool).until(guard)
        for c in &self.flow.constraints {
            if let Constraint::NeverUntil { tool: t, until } = c {
                if t == tool && !until.eval(&self.ctx(state)) {
                    return Err(format!("'{tool}' is not permitted yet"));
                }
            }
        }
        // 3. active allow/deny (whitelist while any active step restricts).
        let active = self.active_steps(state);
        if active.iter().any(|s| s.deny.iter().any(|d| d == tool)) {
            return Err(format!("'{tool}' is not available in the current step"));
        }
        let restricting: Vec<&&Step> = active.iter().filter(|s| !s.allow.is_empty()).collect();
        if !restricting.is_empty()
            && !restricting
                .iter()
                .any(|s| s.allow.iter().any(|a| a == tool))
        {
            return Err(format!("'{tool}' is not available in the current step"));
        }
        Ok(())
    }

    /// Observe a tool call for conformance. In Enforce mode the caller has
    /// already gated via [`admits_tool`](Self::admits_tool); this records the
    /// call and, in Observe mode, logs a deviation if it was inadmissible.
    pub fn observe_tool(&mut self, tool: &str, ok: bool, state: &State) {
        if self.mode == Mode::Observe {
            if let Err(reason) = self.admits_tool(tool, state) {
                self.violations.push(Violation {
                    subject: tool.to_string(),
                    reason,
                });
            }
        }
        if ok {
            self.on_tool_ok(tool, state);
        }
    }
}

/// Builder for a [`Flow`] using the cemented verbs.
#[derive(Default)]
pub struct FlowBuilder {
    steps: Vec<Step>,
    constraints: Vec<Constraint>,
    confirm_tools: Vec<String>,
}

impl FlowBuilder {
    fn current(&mut self) -> &mut Step {
        self.steps
            .last_mut()
            .expect("call `.step(id)` before configuring a step")
    }

    /// Declare a new step.
    pub fn step(mut self, id: impl Into<String>) -> Self {
        self.steps.push(Step {
            id: id.into(),
            after: Vec::new(),
            gate: None,
            done: None,
            posture: None,
            allow: Vec::new(),
            deny: Vec::new(),
            terminal: false,
        });
        self
    }
    /// Add a dependency (call multiple times for multiple deps).
    pub fn after(mut self, dep: impl Into<String>) -> Self {
        self.current().after.push(dep.into());
        self
    }
    /// Extra eligibility guard beyond dependencies.
    pub fn gate(mut self, g: Guard) -> Self {
        self.current().gate = Some(g);
        self
    }
    /// Completion condition.
    pub fn done(mut self, g: Guard) -> Self {
        self.current().done = Some(g);
        self
    }
    /// Instruction imposed while active.
    pub fn posture(mut self, text: impl Into<String>) -> Self {
        self.current().posture = Some(text.into());
        self
    }
    /// Tools available while active (whitelist).
    pub fn allow<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.current()
            .allow
            .extend(tools.into_iter().map(Into::into));
        self
    }
    /// Tools forbidden while active.
    pub fn deny<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.current()
            .deny
            .extend(tools.into_iter().map(Into::into));
        self
    }
    /// Mark the current step terminal.
    pub fn terminal(mut self) -> Self {
        self.current().terminal = true;
        self
    }

    /// A tool may run at most once.
    pub fn once(mut self, tool: impl Into<String>) -> Self {
        self.constraints.push(Constraint::Once(tool.into()));
        self
    }
    /// Ordering invariant: `a` before `b`.
    pub fn before(mut self, a: impl Into<String>, b: impl Into<String>) -> Self {
        self.constraints
            .push(Constraint::Before(a.into(), b.into()));
        self
    }
    /// Required terminal steps for completion.
    pub fn require<I, S>(mut self, steps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.constraints.push(Constraint::Require(
            steps.into_iter().map(Into::into).collect(),
        ));
        self
    }
    /// Forbid a tool until a guard holds (`never(tool).until(guard)`).
    pub fn never(self, tool: impl Into<String>) -> NeverBuilder {
        NeverBuilder {
            fb: self,
            tool: tool.into(),
        }
    }
    /// Commit-tool sugar: at most once, gated until `until`, and flagged for
    /// confirmation. Composes `once` + `never…until` + the confirmation seam.
    pub fn commit(mut self, tool: impl Into<String>, until: Guard) -> Self {
        let tool = tool.into();
        self.constraints.push(Constraint::Once(tool.clone()));
        self.constraints.push(Constraint::NeverUntil {
            tool: tool.clone(),
            until,
        });
        self.confirm_tools.push(tool);
        self
    }

    /// Finalize and validate the flow.
    pub fn build(self) -> Result<Flow, Vec<String>> {
        let flow = Flow {
            steps: self.steps,
            constraints: self.constraints,
            confirm_tools: self.confirm_tools,
        };
        flow.validate()?;
        Ok(flow)
    }
}

/// Sub-builder for `never(tool).until(guard)`.
pub struct NeverBuilder {
    fb: FlowBuilder,
    tool: String,
}

impl NeverBuilder {
    /// Permit the tool once the guard holds.
    pub fn until(mut self, guard: Guard) -> FlowBuilder {
        self.fb.constraints.push(Constraint::NeverUntil {
            tool: self.tool,
            until: guard,
        });
        self.fb
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn debt_flow() -> Flow {
        Flow::new()
            .step("verify")
            .posture("Verify the caller's identity.")
            .allow(["lookup_account"])
            .done(Guard::is_true("identity_verified"))
            .step("disclose")
            .after("verify")
            .posture("Give the disclosure.")
            .done(Guard::is_true("disclosure_given"))
            .step("capture_ptp")
            .after("disclose")
            .done(Guard::captured(["ptp_amount", "ptp_date"]))
            .step("take_payment")
            .after("capture_ptp")
            .allow(["charge_card"])
            .done(Guard::called_ok("charge_card"))
            .step("close")
            .after("capture_ptp")
            .terminal()
            .never("charge_card")
            .until(Guard::is_true("ptp_confirmed"))
            .once("charge_card")
            .require(["close"])
            .build()
            .expect("valid flow")
    }

    #[test]
    fn validates_and_detects_unknown_dep() {
        let bad = Flow::new()
            .step("a")
            .done(Guard::is_true("x"))
            .step("b")
            .after("missing")
            .terminal()
            .build();
        assert!(bad.is_err());
    }

    #[test]
    fn detects_cycle() {
        // a after b, b after a — build() should reject.
        let mut flow = Flow::default();
        flow.steps = vec![
            Step {
                id: "a".into(),
                after: vec!["b".into()],
                gate: None,
                done: Some(Guard::always()),
                posture: None,
                allow: vec![],
                deny: vec![],
                terminal: false,
            },
            Step {
                id: "b".into(),
                after: vec!["a".into()],
                gate: None,
                done: Some(Guard::always()),
                posture: None,
                allow: vec![],
                deny: vec![],
                terminal: false,
            },
        ];
        assert!(flow.validate().is_err());
    }

    #[test]
    fn marking_latches_in_order() {
        let flow = debt_flow();
        let mut mon = FlowMonitor::new(flow, Mode::Enforce);
        let state = State::new();

        // Nothing done; only `verify` is active.
        assert_eq!(
            mon.active_steps(&state)
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["verify"]
        );

        state.set("identity_verified", true);
        mon.on_turn(&state);
        assert!(mon.marking().done.contains("verify"));
        assert_eq!(mon.verdict("verify", &state), Verdict::Done);
        assert_eq!(mon.verdict("disclose", &state), Verdict::Active);

        state.set("disclosure_given", true);
        state.set("ptp_amount", 200);
        state.set("ptp_date", "2026-06-05");
        mon.on_turn(&state);
        // disclose + capture_ptp latch; close is terminal+eligible -> done.
        assert!(mon.marking().done.contains("capture_ptp"));
        assert!(mon.marking().done.contains("close"));
        assert!(mon.is_complete());
    }

    #[test]
    fn enforces_never_until_and_once() {
        let flow = debt_flow();
        let mut mon = FlowMonitor::new(flow, Mode::Enforce);
        let state = State::new();
        // get to take_payment being active
        state.set("identity_verified", true);
        state.set("disclosure_given", true);
        state.set("ptp_amount", 200);
        state.set("ptp_date", "x");
        mon.on_turn(&state);

        // charge_card blocked until ptp_confirmed.
        assert!(mon.admits_tool("charge_card", &state).is_err());
        state.set("ptp_confirmed", true);
        assert!(mon.admits_tool("charge_card", &state).is_ok());

        // after it succeeds once, `once` blocks a second call.
        mon.on_tool_ok("charge_card", &state);
        assert!(mon.admits_tool("charge_card", &state).is_err());
    }

    #[test]
    fn whitelist_scopes_tools_to_active_step() {
        let flow = debt_flow();
        let mon = FlowMonitor::new(flow, Mode::Enforce);
        let state = State::new();
        // In `verify`, only lookup_account is allowed.
        assert!(mon.admits_tool("lookup_account", &state).is_ok());
        assert!(mon.admits_tool("charge_card", &state).is_err());
    }

    #[test]
    fn observe_mode_records_violations_not_blocks() {
        let flow = debt_flow();
        let mut mon = FlowMonitor::new(flow, Mode::Observe);
        let state = State::new();
        // charge_card out of order in observe mode -> recorded, still "runs".
        mon.observe_tool("charge_card", true, &state);
        assert_eq!(mon.violations().len(), 1);
        assert_eq!(mon.violations()[0].subject, "charge_card");
    }

    #[test]
    fn serde_round_trips_data_driven_flow() {
        let flow = debt_flow();
        let jsonv = serde_json::to_value(&flow).expect("serialize");
        let back: Flow = serde_json::from_value(jsonv).expect("deserialize");
        back.validate().expect("round-tripped flow is valid");
        assert_eq!(back.steps.len(), flow.steps.len());
    }

    #[test]
    fn custom_guard_is_not_serializable() {
        let flow = Flow::new()
            .step("a")
            .done(Guard::custom(|ctx| ctx.state.contains("ready")))
            .terminal()
            .build()
            .unwrap();
        assert!(serde_json::to_value(&flow).is_err());
    }

    #[test]
    fn mermaid_export_has_nodes_and_edges() {
        let m = debt_flow().to_mermaid();
        assert!(m.contains("flowchart TD"));
        assert!(m.contains("verify --> disclose"));
        assert!(m.contains("close([close])")); // terminal shape
    }

    #[test]
    fn eq_guard_matches_state_value() {
        let g = Guard::eq("status", json!("active"));
        let state = State::new();
        let marking = Marking::default();
        assert!(!g.eval(&FlowCtx {
            state: &state,
            marking: &marking
        }));
        state.set("status", "active");
        assert!(g.eval(&FlowCtx {
            state: &state,
            marking: &marking
        }));
    }
}
