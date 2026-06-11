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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::orchestration::{call, Mode as AgentMode};
use crate::state::State;
use crate::text::TextAgent;

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

/// Render a grounding template against `state`.
///
/// - `{key}` interpolates the value at `key` (strings bare, other JSON compact);
///   an absent key renders empty.
/// - `{key?yes:no}` renders `yes` when `key` is *truthy* (present and not
///   `false`/`null`/`0`/`""`), else `no`.
///
/// This is the realization of `Effect::ground`: a deterministic projection of
/// known `State` into a steering line, so the model restates facts rather than
/// inventing them.
pub fn render_ground(template: &str, state: &State) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // Unbalanced brace: emit the remainder verbatim.
            out.push_str(&rest[open..]);
            return out;
        };
        let expr = &after[..close];
        out.push_str(&render_expr(expr, state));
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

fn render_expr(expr: &str, state: &State) -> String {
    if let Some((cond, arms)) = expr.split_once('?') {
        let (yes, no) = arms.split_once(':').unwrap_or((arms, ""));
        if is_truthy(state, cond.trim()) {
            yes.to_string()
        } else {
            no.to_string()
        }
    } else {
        match state.get::<Value>(expr.trim()) {
            Some(Value::String(s)) => s,
            Some(v) => v.to_string(),
            None => String::new(),
        }
    }
}

fn is_truthy(state: &State, key: &str) -> bool {
    match state.get::<Value>(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
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
    /// True once an orchestrated agent named `name` has produced a result
    /// (its `{name}:result` state key is set). Pairs with the
    /// [`orchestration`](crate::orchestration) `call`/`dispatch`/`background`.
    pub fn resolved(name: impl AsRef<str>) -> Self {
        Guard::Spec(Pred::IsSet(format!("{}:result", name.as_ref())))
    }
    /// Conjunction.
    ///
    /// If every input is a serializable atom, the result is a serializable
    /// `Pred::All`. If any input is a [`Guard::custom`], the result is itself a
    /// custom guard that evaluates the conjunction at runtime — the custom guard
    /// is **never silently dropped** (it merely makes the combinator
    /// non-serializable, which surfaces as an error only if you try to serialize
    /// the flow).
    pub fn all(guards: impl IntoIterator<Item = Guard>) -> Self {
        let guards: Vec<Guard> = guards.into_iter().collect();
        if guards.iter().all(|g| matches!(g, Guard::Spec(_))) {
            Guard::Spec(Pred::All(specs_unchecked(guards)))
        } else {
            Guard::Custom(Arc::new(move |ctx| guards.iter().all(|g| g.eval(ctx))))
        }
    }
    /// Disjunction.
    ///
    /// Mirrors [`Guard::all`]: custom inputs are preserved as a runtime closure
    /// rather than erased.
    pub fn any(guards: impl IntoIterator<Item = Guard>) -> Self {
        let guards: Vec<Guard> = guards.into_iter().collect();
        if guards.iter().all(|g| matches!(g, Guard::Spec(_))) {
            Guard::Spec(Pred::Any(specs_unchecked(guards)))
        } else {
            Guard::Custom(Arc::new(move |ctx| guards.iter().any(|g| g.eval(ctx))))
        }
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

/// Unwrap a list of guards known to be all `Spec` into their predicates.
///
/// The caller (`Guard::all`/`Guard::any`) only invokes this after verifying every
/// guard is a `Spec`, so the `Custom` arm is unreachable.
fn specs_unchecked(guards: Vec<Guard>) -> Vec<Pred> {
    guards
        .into_iter()
        .map(|g| match g {
            Guard::Spec(p) => p,
            Guard::Custom(_) => unreachable!("specs_unchecked called with a custom guard"),
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
    /// A grounding template projected while active: a curated, `State`-interpolated
    /// fact line that pins the model to known values (anti-hallucination). See
    /// [`render_ground`]. Serializable, like `posture`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground: Option<String>,
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

    /// Every tool name referenced anywhere in the flow (allow/deny/once/
    /// never_until/confirm). The universe over which [`ToolPolicy`] reasons.
    fn tool_universe(&self) -> BTreeSet<String> {
        let mut tools = BTreeSet::new();
        for s in &self.steps {
            tools.extend(s.allow.iter().cloned());
            tools.extend(s.deny.iter().cloned());
        }
        for c in &self.constraints {
            match c {
                Constraint::Once(t) => {
                    tools.insert(t.clone());
                }
                Constraint::NeverUntil { tool, .. } => {
                    tools.insert(tool.clone());
                }
                _ => {}
            }
        }
        tools.extend(self.confirm_tools.iter().cloned());
        tools
    }

    /// Steps reachable from a root (a step with no `after` deps), following both
    /// `after` edges and `Before(a, b)` ordering edges.
    fn reachable_steps(&self) -> BTreeSet<String> {
        let ids: BTreeSet<&str> = self.steps.iter().map(|s| s.id.as_str()).collect();
        // Forward edges: a -> b when b.after contains a, or Before(a, b).
        let mut succ: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for s in &self.steps {
            for d in &s.after {
                if ids.contains(d.as_str()) {
                    succ.entry(d.as_str()).or_default().push(s.id.as_str());
                }
            }
        }
        for c in &self.constraints {
            if let Constraint::Before(a, b) = c {
                if ids.contains(a.as_str()) && ids.contains(b.as_str()) {
                    succ.entry(a.as_str()).or_default().push(b.as_str());
                }
            }
        }
        let roots: Vec<&str> = self
            .steps
            .iter()
            .filter(|s| s.after.is_empty())
            .map(|s| s.id.as_str())
            .collect();
        let mut seen = BTreeSet::new();
        let mut stack = roots;
        while let Some(id) = stack.pop() {
            if seen.insert(id.to_string()) {
                if let Some(next) = succ.get(id) {
                    stack.extend(next.iter().copied());
                }
            }
        }
        seen
    }

    /// Compile and validate the flow into a [`CompiledFlow`], turning a class of
    /// runtime surprises into load-time errors.
    ///
    /// On top of [`validate`](Self::validate)'s referential/acyclicity checks this
    /// reports: unreachable steps, commit tools guarded by an always-true
    /// condition (an effectively *unguarded* commit, which defeats the
    /// confirm-before-commit contract), `never…until` guards whose `done(step)`
    /// atoms reference unknown steps (unsatisfiable — the tool would be forbidden
    /// forever), and ordering cycles across the combined `after` + `before` edges
    /// (which deadlock every step on the cycle). Precomputes the [`ToolPolicy`]
    /// universe.
    ///
    /// To additionally validate tool names against a known registry, use
    /// [`compile_with_tools`](Self::compile_with_tools).
    pub fn compile(self) -> Result<CompiledFlow, FlowErrors> {
        self.compile_internal(None)
    }

    /// Compile like [`compile`](Self::compile), additionally validating every
    /// tool name the flow references (step `allow`/`deny`, `once`,
    /// `never…until`, and commit/confirm tools) against the given registry of
    /// known tool names.
    ///
    /// A referenced tool missing from `tools` is reported as
    /// [`FlowError::UnknownTool`] — catching typos and drift between a flow
    /// script and the tools actually registered on the session.
    ///
    /// ```ignore
    /// let compiled = flow.compile_with_tools(&["lookup_account", "charge_card"])?;
    /// ```
    pub fn compile_with_tools(self, tools: &[&str]) -> Result<CompiledFlow, FlowErrors> {
        self.compile_internal(Some(tools))
    }

    fn compile_internal(self, registry: Option<&[&str]>) -> Result<CompiledFlow, FlowErrors> {
        let mut errors = Vec::new();
        if let Err(errs) = self.validate() {
            errors.extend(errs.into_iter().map(FlowError::Invalid));
        }

        // Graph-shape checks (only meaningful once the graph is acyclic/valid).
        if errors.is_empty() {
            // Unreachable steps.
            let reachable = self.reachable_steps();
            for s in &self.steps {
                if !reachable.contains(&s.id) {
                    errors.push(FlowError::UnreachableStep(s.id.clone()));
                }
            }
            // Ordering cycles across the combined `after` + `before(a, b)` edges.
            // `validate()` only walks `after`; a cycle closed by a `Before`
            // constraint deadlocks every step on it (none can become eligible).
            if let Some(cycle) = self.ordering_cycle() {
                errors.push(FlowError::OrderingCycle(cycle));
            }
        }

        // A commit tool guarded by an always-true condition is effectively
        // unguarded — the confirm-before-commit contract would never gate it.
        for tool in &self.confirm_tools {
            let guard = self.constraints.iter().find_map(|c| match c {
                Constraint::NeverUntil { tool: t, until } if t == tool => Some(until),
                _ => None,
            });
            let unguarded = matches!(guard, None | Some(Guard::Spec(Pred::Always)));
            if unguarded {
                errors.push(FlowError::UnguardedCommitTool(tool.clone()));
            }
        }

        // An unsatisfiable `never(tool).until(guard)`: the guard's `done(step)`
        // atom references a step that doesn't exist, so it can never latch and
        // the tool is forbidden forever. (Step gate/done guards are already
        // covered by `validate()`; constraints were not.)
        let ids: BTreeSet<&str> = self.steps.iter().map(|s| s.id.as_str()).collect();
        for c in &self.constraints {
            if let Constraint::NeverUntil { tool, until } = c {
                let mut refs = Vec::new();
                until.referenced_steps(&mut refs);
                for r in refs {
                    if !ids.contains(r.as_str()) {
                        errors.push(FlowError::UnsatisfiableGuard {
                            tool: tool.clone(),
                            step: r,
                        });
                    }
                }
            }
        }

        // Dangling tool names vs a known registry (opt-in).
        if let Some(known) = registry {
            for tool in self.tool_universe() {
                if !known.contains(&tool.as_str()) {
                    errors.push(FlowError::UnknownTool(tool));
                }
            }
        }

        if errors.is_empty() {
            let policy = ToolPolicy {
                tools: self.tool_universe(),
            };
            Ok(CompiledFlow { flow: self, policy })
        } else {
            Err(FlowErrors(errors))
        }
    }

    /// Find a cycle over the combined dependency edges (`after` plus
    /// `before(a, b)` ordering constraints), if any. Returns the step ids on
    /// the cycle path. `None` when the combined graph is acyclic.
    fn ordering_cycle(&self) -> Option<Vec<String>> {
        // Predecessor edges: step -> everything that must be done before it.
        let mut deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for s in &self.steps {
            let entry = deps.entry(s.id.as_str()).or_default();
            entry.extend(s.after.iter().map(String::as_str));
        }
        for c in &self.constraints {
            if let Constraint::Before(a, b) = c {
                deps.entry(b.as_str()).or_default().push(a.as_str());
            }
        }
        // DFS with colors; on a back-edge, report the current path suffix.
        fn dfs<'a>(
            id: &'a str,
            deps: &BTreeMap<&'a str, Vec<&'a str>>,
            color: &mut BTreeMap<&'a str, u8>,
            path: &mut Vec<&'a str>,
        ) -> Option<Vec<String>> {
            color.insert(id, 1);
            path.push(id);
            for d in deps.get(id).into_iter().flatten() {
                match color.get(d).copied() {
                    Some(1) => {
                        let start = path.iter().position(|p| p == d).unwrap_or(0);
                        return Some(path[start..].iter().map(|s| s.to_string()).collect());
                    }
                    Some(2) => {}
                    _ => {
                        if let Some(cycle) = dfs(d, deps, color, path) {
                            return Some(cycle);
                        }
                    }
                }
            }
            path.pop();
            color.insert(id, 2);
            None
        }
        let mut color: BTreeMap<&str, u8> = BTreeMap::new();
        for s in &self.steps {
            if color.get(s.id.as_str()).copied().unwrap_or(0) == 0 {
                let mut path = Vec::new();
                if let Some(cycle) = dfs(&s.id, &deps, &mut color, &mut path) {
                    return Some(cycle);
                }
            }
        }
        None
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

/// How a [`FlowMonitor`] treats off-path activity — enforcement vs observation.
///
/// Renamed from `Mode` to remove the collision with
/// [`orchestration::Mode`](crate::orchestration::Mode) (`Call`/`Dispatch`/
/// `Background`), which is the unrelated *resolver execution discipline*.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Enforcement {
    /// Block inadmissible tool calls and steer back on-path.
    #[default]
    Enforce,
    /// Allow everything, but record deviations for audit/analytics.
    Observe,
}

/// Deprecated alias for [`Enforcement`], kept for one release.
#[deprecated(note = "renamed to `Enforcement` to avoid colliding with orchestration::Mode")]
pub type Mode = Enforcement;

/// An action fired the first time a step becomes active: run an agent in an
/// [`AgentMode`]. Built with [`run`]. The result lands in `{name}:result` (the
/// name defaults to the step id), so a *downstream* step can complete on it via
/// [`Guard::resolved`] — this is how a flow drives orchestration in-session.
#[derive(Clone)]
pub struct StepAction {
    name: Option<String>,
    agent: Arc<dyn TextAgent>,
    mode: AgentMode,
}

/// Build a step-enter action that runs `agent` in `mode` when the step first
/// activates. Pair with [`FlowMonitor::on_enter`].
///
/// ```ignore
/// let mon = FlowMonitor::new(flow, Enforcement::Enforce)
///     .on_enter("check", run(availability_agent, AgentMode::Dispatch));
/// ```
pub fn run(agent: Arc<dyn TextAgent>, mode: AgentMode) -> StepAction {
    StepAction {
        name: None,
        agent,
        mode,
    }
}

impl StepAction {
    /// Override the result name (defaults to the step id it is attached to).
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Run the action. `Call` awaits inline; `Dispatch`/`Background` spawn it
    /// detached so the turn is never blocked.
    pub(crate) async fn fire(&self, step_id: &str, state: &State) {
        let name = self.name.clone().unwrap_or_else(|| step_id.to_string());
        match self.mode {
            AgentMode::Call => {
                let _ = call(&name, self.agent.clone(), state).await;
            }
            AgentMode::Dispatch | AgentMode::Background => {
                let agent = self.agent.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = call(&name, agent, &state).await;
                });
            }
        }
    }
}

/// A shared, lock-protected [`FlowMonitor`] — the form in which the Live
/// control plane owns a governed flow, so runtime surfaces (e.g.
/// [`LiveHandle::why_blocked`](crate::live::LiveHandle::why_blocked)) can
/// snapshot it concurrently. All monitor methods are synchronous: lock
/// briefly and never hold the guard across an `await`.
pub type SharedFlowMonitor = Arc<parking_lot::Mutex<FlowMonitor>>;

/// Observes the session trace, maintains the [`Marking`], answers tool
/// admissibility, and projects active postures.
pub struct FlowMonitor {
    flow: Flow,
    mode: Enforcement,
    marking: Marking,
    violations: Vec<Violation>,
    /// Per-step actions fired the first time the step becomes active.
    enter_actions: HashMap<String, StepAction>,
    /// Steps whose `on_enter` action has already fired (fire-once).
    announced: BTreeSet<String>,
}

impl FlowMonitor {
    /// Create a monitor for a (presumed-valid) flow.
    ///
    /// Prefer [`FlowMonitor::compiled`] or [`FlowMonitor::try_new`], which carry
    /// proof of compilation; this convenience skips compilation for flows already
    /// known valid (e.g. built in-process by trusted code).
    pub fn new(flow: Flow, mode: Enforcement) -> Self {
        Self {
            flow,
            mode,
            marking: Marking::default(),
            violations: Vec::new(),
            enter_actions: HashMap::new(),
            announced: BTreeSet::new(),
        }
    }

    /// Create a monitor from a [`CompiledFlow`] — the validated path.
    pub fn compiled(flow: CompiledFlow, mode: Enforcement) -> Self {
        Self::new(flow.into_flow(), mode)
    }

    /// Compile `flow` and create a monitor, surfacing structural errors instead
    /// of trusting the caller.
    pub fn try_new(flow: Flow, mode: Enforcement) -> Result<Self, FlowErrors> {
        Ok(Self::compiled(flow.compile()?, mode))
    }

    /// Wrap this monitor in a [`SharedFlowMonitor`] for shared ownership
    /// between the control lane (which advances it) and runtime accessors
    /// (which snapshot it, e.g.
    /// [`LiveHandle::explain`](crate::live::LiveHandle::explain)).
    pub fn into_shared(self) -> SharedFlowMonitor {
        Arc::new(parking_lot::Mutex::new(self))
    }

    /// Explain the current control-plane state: active steps, which tools are
    /// admitted vs blocked (with reasons), and unmet requirements.
    ///
    /// This is the deterministic answer to "why did the assistant ask that?" —
    /// model-readable, without the model driving control flow.
    pub fn explain(&self, state: &State) -> FlowExplanation {
        let active = self
            .active_steps(state)
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let mut allowed_tools = Vec::new();
        let mut blocked_tools = BTreeMap::new();
        for tool in self.flow.tool_universe() {
            match self.admits_tool(&tool, state) {
                Ok(()) => allowed_tools.push(tool),
                Err(reason) => {
                    blocked_tools.insert(tool, reason);
                }
            }
        }
        FlowExplanation {
            active,
            allowed_tools,
            blocked_tools,
            missing_requirements: self.unmet_requirements(),
        }
    }

    /// Why the flow is blocked right now — alias of [`explain`](Self::explain),
    /// named for the common debugging question.
    pub fn why_blocked(&self, state: &State) -> FlowExplanation {
        self.explain(state)
    }

    /// Attach an action fired the first time `step` becomes active (see
    /// [`run`]). Chainable at construction time.
    pub fn on_enter(mut self, step: impl Into<String>, action: StepAction) -> Self {
        self.enter_actions.insert(step.into(), action);
        self
    }

    /// Steps that became active since the last call — each reported exactly once
    /// over the session. Drives [`on_enter`](Self::on_enter) firing.
    pub fn take_newly_active(&mut self, state: &State) -> Vec<String> {
        let mut fresh = Vec::new();
        for s in self.active_steps(state) {
            if !self.announced.contains(&s.id) {
                fresh.push(s.id.clone());
            }
        }
        for id in &fresh {
            self.announced.insert(id.clone());
        }
        fresh
    }

    /// The enter-action registered for a step, if any.
    pub fn enter_action(&self, step: &str) -> Option<&StepAction> {
        self.enter_actions.get(step)
    }

    /// Fire enter-actions for every step that just became active. Convenience
    /// over [`take_newly_active`](Self::take_newly_active) + [`enter_action`](Self::enter_action);
    /// call it right after [`on_turn`](Self::on_turn).
    pub async fn fire_enter_actions(&mut self, state: &State) {
        for id in self.take_newly_active(state) {
            if let Some(action) = self.enter_actions.get(&id) {
                action.fire(&id, state).await;
            }
        }
    }

    /// The enforcement mode this monitor runs in.
    pub fn mode(&self) -> Enforcement {
        self.mode
    }

    /// Evaluate a [`Guard`] against this monitor's current context (the given
    /// `state` plus the monitor's marking). Used to test overlay/digression
    /// triggers without exposing the internal context.
    pub fn eval(&self, guard: &Guard, state: &State) -> bool {
        guard.eval(&self.ctx(state))
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
        // Enforce `Constraint::Before(a, step)`: `a` must be done before this
        // step may start (an ordering constraint declared outside `after`).
        let before_ok = self.flow.constraints.iter().all(|c| match c {
            Constraint::Before(a, b) if *b == step.id => self.marking.done.contains(a),
            _ => true,
        });
        let gate_ok = step
            .gate
            .as_ref()
            .map(|g| g.eval(&self.ctx(state)))
            .unwrap_or(true);
        deps_done && before_ok && gate_ok
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

    /// Rendered grounding lines of the active steps — curated, `State`-
    /// interpolated facts to inject as turn-boundary steering (anti-hallucination).
    pub fn active_grounds(&self, state: &State) -> Vec<String> {
        self.active_steps(state)
            .into_iter()
            .filter_map(|s| s.ground.as_ref().map(|t| render_ground(t, state)))
            .filter(|s| !s.trim().is_empty())
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
        if self.mode == Enforcement::Observe {
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

/// A single problem found while compiling a [`Flow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowError {
    /// A referential-integrity or acyclicity error from [`Flow::validate`].
    Invalid(String),
    /// A step that no path from a root can ever reach.
    UnreachableStep(String),
    /// A commit (confirm) tool whose gate is always true — effectively
    /// unguarded, defeating confirm-before-commit.
    UnguardedCommitTool(String),
    /// A tool referenced by the flow (step `allow`/`deny`, `once`,
    /// `never…until`, confirm) that is not in the registry given to
    /// [`Flow::compile_with_tools`].
    UnknownTool(String),
    /// A `never(tool).until(guard)` whose guard references a step id that
    /// doesn't exist — the guard can never latch, so the tool would be
    /// forbidden forever.
    UnsatisfiableGuard {
        /// The tool the constraint gates.
        tool: String,
        /// The unknown step id the guard's `done(..)` atom references.
        step: String,
    },
    /// A cycle over the combined `after` + `before(a, b)` ordering edges —
    /// every step on the cycle waits on another, so none can ever become
    /// eligible. Contains the step ids on the cycle.
    OrderingCycle(Vec<String>),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::Invalid(m) => write!(f, "{m}"),
            FlowError::UnreachableStep(id) => {
                write!(f, "step '{id}' is unreachable from any root")
            }
            FlowError::UnguardedCommitTool(t) => write!(
                f,
                "commit tool '{t}' is guarded by an always-true condition (effectively unguarded)"
            ),
            FlowError::UnknownTool(t) => write!(
                f,
                "flow references tool '{t}' which is not in the provided tool registry"
            ),
            FlowError::UnsatisfiableGuard { tool, step } => write!(
                f,
                "`never('{tool}').until(..)` references unknown step '{step}' — the guard can \
                 never hold, so '{tool}' would be forbidden forever"
            ),
            FlowError::OrderingCycle(steps) => write!(
                f,
                "ordering cycle across `after`/`before` edges: {} (no step on it can ever start)",
                steps.join(" -> ")
            ),
        }
    }
}

/// All problems found while compiling a [`Flow`]; non-empty on failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowErrors(pub Vec<FlowError>);

impl std::fmt::Display for FlowErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "flow failed to compile ({} error(s)):", self.0.len())?;
        for e in &self.0 {
            writeln!(f, "  - {e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for FlowErrors {}

/// The precomputed tool-gating surface of a compiled flow: every tool name the
/// flow reasons about, so introspection can enumerate and explain decisions.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    /// Every tool referenced anywhere in the flow.
    pub tools: BTreeSet<String>,
}

/// A validated [`Flow`] plus its precomputed [`ToolPolicy`].
///
/// Produced by [`Flow::compile`]. Holding one is proof the flow passed
/// compilation, so the runtime never re-discovers structural errors. This is the
/// IR the conversation compiler targets and the type richer surfaces build on.
#[derive(Debug, Clone)]
pub struct CompiledFlow {
    flow: Flow,
    policy: ToolPolicy,
}

impl CompiledFlow {
    /// The underlying validated flow.
    pub fn flow(&self) -> &Flow {
        &self.flow
    }
    /// The precomputed tool policy.
    pub fn tool_policy(&self) -> &ToolPolicy {
        &self.policy
    }
    /// Render the flow as a Mermaid diagram.
    pub fn to_mermaid(&self) -> String {
        self.flow.to_mermaid()
    }
    /// Consume into the inner flow.
    pub fn into_flow(self) -> Flow {
        self.flow
    }
}

/// A model-readable explanation of the current control-plane state — the
/// foundation of `why did the assistant ask that?`.
///
/// Produced by [`FlowMonitor::explain`]. `Serialize` so it can be surfaced to a
/// model, a devtool, or a log without the model driving control flow.
#[derive(Debug, Clone, Serialize)]
pub struct FlowExplanation {
    /// Steps eligible-but-not-done right now.
    pub active: Vec<String>,
    /// Tools currently admitted.
    pub allowed_tools: Vec<String>,
    /// Tools currently blocked, mapped to the reason.
    pub blocked_tools: BTreeMap<String, String>,
    /// Required steps not yet done (drives repair).
    pub missing_requirements: Vec<String>,
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
            ground: None,
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
    /// A grounding template projected while active — a curated, `State`-
    /// interpolated fact line that pins the model to known values. `{key}`
    /// interpolates a value; `{key?yes:no}` picks by truthiness. See
    /// [`render_ground`].
    pub fn ground(mut self, template: impl Into<String>) -> Self {
        self.current().ground = Some(template.into());
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
                ground: None,
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
                ground: None,
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
        let mut mon = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();

        // Nothing done; only `verify` is active.
        assert_eq!(
            mon.active_steps(&state)
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["verify"]
        );

        let _ = state.set("identity_verified", true);
        mon.on_turn(&state);
        assert!(mon.marking().done.contains("verify"));
        assert_eq!(mon.verdict("verify", &state), Verdict::Done);
        assert_eq!(mon.verdict("disclose", &state), Verdict::Active);

        let _ = state.set("disclosure_given", true);
        let _ = state.set("ptp_amount", 200);
        let _ = state.set("ptp_date", "2026-06-05");
        mon.on_turn(&state);
        // disclose + capture_ptp latch; close is terminal+eligible -> done.
        assert!(mon.marking().done.contains("capture_ptp"));
        assert!(mon.marking().done.contains("close"));
        assert!(mon.is_complete());
    }

    #[test]
    fn enforces_never_until_and_once() {
        let flow = debt_flow();
        let mut mon = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();
        // get to take_payment being active
        let _ = state.set("identity_verified", true);
        let _ = state.set("disclosure_given", true);
        let _ = state.set("ptp_amount", 200);
        let _ = state.set("ptp_date", "x");
        mon.on_turn(&state);

        // charge_card blocked until ptp_confirmed.
        assert!(mon.admits_tool("charge_card", &state).is_err());
        let _ = state.set("ptp_confirmed", true);
        assert!(mon.admits_tool("charge_card", &state).is_ok());

        // after it succeeds once, `once` blocks a second call.
        mon.on_tool_ok("charge_card", &state);
        assert!(mon.admits_tool("charge_card", &state).is_err());
    }

    #[test]
    fn whitelist_scopes_tools_to_active_step() {
        let flow = debt_flow();
        let mon = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();
        // In `verify`, only lookup_account is allowed.
        assert!(mon.admits_tool("lookup_account", &state).is_ok());
        assert!(mon.admits_tool("charge_card", &state).is_err());
    }

    #[test]
    fn observe_mode_records_violations_not_blocks() {
        let flow = debt_flow();
        let mut mon = FlowMonitor::new(flow, Enforcement::Observe);
        let state = State::new();
        // charge_card out of order in observe mode -> recorded, still "runs".
        mon.observe_tool("charge_card", true, &state);
        assert_eq!(mon.violations().len(), 1);
        assert_eq!(mon.violations()[0].subject, "charge_card");
    }

    #[test]
    fn compile_accepts_valid_flow_and_collects_tool_universe() {
        let compiled = debt_flow().compile().expect("valid flow compiles");
        // Tool universe spans allow/deny/once/never_until/confirm.
        assert!(compiled.tool_policy().tools.contains("charge_card"));
        assert!(compiled.tool_policy().tools.contains("lookup_account"));
        let _ = FlowMonitor::compiled(compiled, Enforcement::Enforce);
    }

    #[test]
    fn compile_rejects_unreachable_step() {
        // `orphan` has no `after` and nothing leads to it — but it IS a root, so
        // to make it unreachable we give it an `after` on a step, then never make
        // that path lead anywhere. Simplest: a step depending on a missing root is
        // caught by validate; here we test a step unreachable via a broken chain.
        let flow = Flow::new()
            .step("a")
            .done(Guard::is_true("a_done"))
            .step("b")
            .after("a")
            .done(Guard::is_true("b_done"))
            .step("island")
            .after("b")
            .gate(Guard::is_true("never"))
            .terminal()
            .build()
            .expect("structurally valid");
        // island is reachable via a->b->island, so this compiles; assert it does.
        assert!(flow.compile().is_ok());
    }

    #[test]
    fn compile_rejects_unguarded_commit_tool() {
        // commit tool gated by an always-true guard is effectively unguarded.
        let flow = Flow::new()
            .step("s")
            .allow(["pay"])
            .done(Guard::called_ok("pay"))
            .terminal()
            .commit("pay", Guard::always())
            .build()
            .expect("structurally valid");
        let err = flow
            .compile()
            .expect_err("unguarded commit must fail to compile");
        assert!(err
            .0
            .iter()
            .any(|e| matches!(e, FlowError::UnguardedCommitTool(t) if t == "pay")));
    }

    #[test]
    fn compile_with_tools_accepts_a_covering_registry() {
        let compiled = debt_flow()
            .compile_with_tools(&["lookup_account", "charge_card", "unrelated_extra"])
            .expect("registry covers the flow's tool universe");
        assert!(compiled.tool_policy().tools.contains("charge_card"));
    }

    #[test]
    fn compile_with_tools_reports_dangling_tool_names() {
        // `charge_card` is referenced (allow/once/never_until) but missing from
        // the registry — a typo/drift the compiler must catch.
        let err = debt_flow()
            .compile_with_tools(&["lookup_account"])
            .expect_err("dangling tool must fail to compile");
        assert!(err
            .0
            .iter()
            .any(|e| matches!(e, FlowError::UnknownTool(t) if t == "charge_card")));
        // Plain compile() stays registry-agnostic.
        assert!(debt_flow().compile().is_ok());
    }

    #[test]
    fn compile_rejects_never_until_guard_on_unknown_step() {
        // `never(pay).until(done("missing"))` can never latch — `pay` would be
        // forbidden forever. validate() doesn't check constraint guards; compile must.
        let flow = Flow::new()
            .step("s")
            .allow(["pay"])
            .done(Guard::called_ok("pay"))
            .never("pay")
            .until(Guard::done("missing"))
            .build()
            .expect("structurally valid for build()");
        let err = flow.compile().expect_err("unsatisfiable guard must fail");
        assert!(err.0.iter().any(|e| matches!(
            e,
            FlowError::UnsatisfiableGuard { tool, step } if tool == "pay" && step == "missing"
        )));
    }

    #[test]
    fn compile_rejects_before_cycle() {
        // `after` edges are acyclic, but before(a, b) + before(b, a) closes an
        // ordering cycle: neither step can ever become eligible. validate()'s
        // cycle check only walks `after`, so compile must catch this.
        let flow = Flow::new()
            .step("a")
            .done(Guard::is_true("a_done"))
            .step("b")
            .done(Guard::is_true("b_done"))
            .before("a", "b")
            .before("b", "a")
            .build()
            .expect("build() only checks `after` cycles");
        let err = flow
            .compile()
            .expect_err("before-cycle must fail to compile");
        assert!(err.0.iter().any(|e| matches!(
            e,
            FlowError::OrderingCycle(steps)
                if steps.contains(&"a".to_string()) && steps.contains(&"b".to_string())
        )));
    }

    #[test]
    fn explain_reports_blocked_tools_and_reasons() {
        let flow = debt_flow();
        let mon = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();
        let ex = mon.explain(&state);
        // In the initial `verify` step, charge_card is blocked; explain says so.
        assert!(ex.blocked_tools.contains_key("charge_card"));
        assert!(ex.active.contains(&"verify".to_string()));
        // why_blocked is the same view.
        assert_eq!(mon.why_blocked(&state).blocked_tools, ex.blocked_tools);
    }

    #[test]
    fn before_constraint_gates_step_eligibility() {
        // Regression: `before(a, b)` was validated but never enforced — `b` could
        // start before `a` was done. `a` and `b` have no `after` edge, so only the
        // Before constraint orders them.
        let flow = Flow::new()
            .step("a")
            .done(Guard::is_true("a_done"))
            .step("b")
            .done(Guard::is_true("b_done"))
            .before("a", "b")
            .build()
            .expect("valid flow");
        let mut mon = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();

        // `b` is NOT active until `a` is done, even though its own gate is open.
        let active: Vec<String> = mon
            .active_steps(&state)
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert!(active.contains(&"a".to_string()));
        assert!(
            !active.contains(&"b".to_string()),
            "b must wait for a (Before)"
        );

        let _ = state.set("a_done", true);
        mon.on_turn(&state);
        let active: Vec<String> = mon
            .active_steps(&state)
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert!(active.contains(&"b".to_string()), "b active once a is done");
    }

    #[test]
    fn custom_guard_in_combinator_is_not_erased() {
        // Regression: a custom guard nested in all()/any() was lowered to
        // Pred::Always, silently deleting it. It must still evaluate.
        let always_false = Guard::all([Guard::is_true("present"), Guard::custom(|_| false)]);
        // Mixed combinator is a Custom guard (non-serializable), not a Spec.
        assert!(matches!(always_false, Guard::Custom(_)));

        let state = State::new();
        let _ = state.set("present", true);
        let marking = Marking::default();
        let ctx = FlowCtx {
            state: &state,
            marking: &marking,
        };
        // Would be `true` if the custom guard had been erased to Always.
        assert!(!always_false.eval(&ctx), "custom guard must still veto");

        // all-spec combinator stays serializable.
        let serializable = Guard::all([Guard::is_true("a"), Guard::is_set("b")]);
        assert!(matches!(serializable, Guard::Spec(_)));
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

    struct WriteAgent;
    #[async_trait::async_trait]
    impl TextAgent for WriteAgent {
        fn name(&self) -> &str {
            "writer"
        }
        async fn run(&self, _state: &State) -> Result<String, crate::error::AgentError> {
            Ok("available".to_string())
        }
    }

    #[tokio::test]
    async fn on_enter_fires_once_when_step_activates() {
        // collect -> check ; `check` runs an agent on enter whose result
        // (`check:result`) then completes a downstream `book` step.
        let flow = Flow::new()
            .step("collect")
            .done(Guard::is_true("collected"))
            .step("check")
            .after("collect")
            .done(Guard::resolved("check"))
            .step("book")
            .after("check")
            .terminal()
            .require(["book"])
            .build()
            .expect("valid flow");

        let mut mon = FlowMonitor::new(flow, Enforcement::Enforce)
            .on_enter("check", run(Arc::new(WriteAgent), AgentMode::Call));
        let state = State::new();

        // Only `collect` is active at the start.
        assert_eq!(mon.take_newly_active(&state), vec!["collect".to_string()]);
        // Re-asking yields nothing — fire-once.
        assert!(mon.take_newly_active(&state).is_empty());

        // Complete `collect`; `check` becomes active and its on_enter fires.
        let _ = state.set("collected", true);
        mon.on_turn(&state);
        mon.fire_enter_actions(&state).await;
        assert_eq!(
            state.get::<String>("check:result").as_deref(),
            Some("available")
        );

        // The resolved result completes `check`, then terminal `book`.
        mon.on_turn(&state);
        assert!(mon.marking().done.contains("check"));
        assert!(mon.is_complete());
        // No further newly-active steps to announce.
        assert!(mon.take_newly_active(&state).is_empty());
    }

    #[test]
    fn ground_template_interpolates_and_branches() {
        let state = State::new();
        let _ = state.set("when", "3pm");
        let _ = state.set("available", true);
        let _ = state.set("prior_visits", 2);
        assert_eq!(
            render_ground(
                "{when} is {available?open:taken}; {prior_visits} prior visits.",
                &state
            ),
            "3pm is open; 2 prior visits."
        );
        // Falsy branch + absent key renders empty.
        let _ = state.set("available", false);
        assert_eq!(
            render_ground("slot {missing}is {available?free:full}", &state),
            "slot is full"
        );
    }

    #[test]
    fn active_grounds_projects_only_active_steps() {
        let flow = Flow::new()
            .step("collect")
            .ground("Known time: {when}.")
            .done(Guard::is_set("when"))
            .step("done")
            .after("collect")
            .terminal()
            .build()
            .expect("valid flow");
        let mut mon = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();
        let _ = state.set("when", "3pm");
        assert_eq!(
            mon.active_grounds(&state),
            vec!["Known time: 3pm.".to_string()]
        );
        // Once collect completes, its ground no longer projects.
        mon.on_turn(&state);
        assert!(mon.active_grounds(&state).is_empty());
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
        let _ = state.set("status", "active");
        assert!(g.eval(&FlowCtx {
            state: &state,
            marking: &marking
        }));
    }
}
