//! Operator algebra for agent composition.
//!
//! All types implementing `Composable` participate in the algebra:
//!
//! | Operator | Meaning            | Example                    |
//! |----------|--------------------|----------------------------|
//! | `>>`     | Sequential pipeline| `agent_a >> agent_b`       |
//! | `\|`     | Parallel fan-out   | `agent_a \| agent_b`       |
//! | `*`      | Loop (fixed)       | `agent * 3`                |
//! | `//`     | Fallback chain     | `agent_a // agent_b`       |

use std::sync::Arc;

use gemini_adk_rs::llm::BaseLlm;
use gemini_adk_rs::middleware::{Middleware, MiddlewareChain};
use gemini_adk_rs::text::{
    FallbackTextAgent, LoopTextAgent, ParallelTextAgent, SequentialTextAgent, TextAgent,
};

use crate::builder::AgentBuilder;
use crate::compose::middleware::MiddlewareComposite;

/// A composable workflow node — can be sequenced, fan-out, looped, etc.
#[derive(Clone, Debug)]
pub enum Composable {
    /// A single agent node.
    Agent(AgentBuilder),
    /// A sequential pipeline of steps.
    Pipeline(Pipeline),
    /// A parallel fan-out of branches.
    FanOut(FanOut),
    /// A loop with optional termination predicate.
    Loop(Loop),
    /// A fallback chain (try each until one succeeds).
    Fallback(Fallback),
}

/// Sequential pipeline: execute steps in order, passing state between them.
#[derive(Clone, Debug)]
pub struct Pipeline {
    /// Ordered steps to execute sequentially.
    pub steps: Vec<Composable>,
}

/// Parallel fan-out: execute branches concurrently, merge results.
#[derive(Clone, Debug)]
pub struct FanOut {
    /// Branches to execute concurrently.
    pub branches: Vec<Composable>,
}

/// Loop: repeat an agent or pipeline up to `max` times, or until a predicate.
#[derive(Clone)]
pub struct Loop {
    /// The composable to repeat.
    pub body: Box<Composable>,
    /// Maximum number of iterations.
    pub max: u32,
    /// Optional early-exit predicate evaluated after each iteration.
    pub until: Option<LoopPredicate>,
    /// Middleware attached to the loop agent (e.g. `M::on_loop` observers).
    /// Set via [`Loop::middleware`] / [`Composable::middleware`]; construct as
    /// `Vec::new()` in literals.
    #[doc(hidden)]
    pub middleware: Vec<Arc<dyn Middleware>>,
}

/// Predicate for conditional loop termination.
#[derive(Clone)]
pub struct LoopPredicate {
    predicate: std::sync::Arc<dyn Fn(&serde_json::Value) -> bool + Send + Sync>,
}

impl LoopPredicate {
    /// Create a new predicate from a closure that checks loop state.
    pub fn new(f: impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static) -> Self {
        Self {
            predicate: std::sync::Arc::new(f),
        }
    }

    /// Evaluate the predicate against the current state.
    pub fn check(&self, state: &serde_json::Value) -> bool {
        (self.predicate)(state)
    }
}

impl std::fmt::Debug for LoopPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoopPredicate(<fn>)")
    }
}

impl std::fmt::Debug for Loop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loop")
            .field("body", &self.body)
            .field("max", &self.max)
            .field("until", &self.until)
            .finish()
    }
}

/// Fallback chain: try each agent in sequence until one succeeds.
#[derive(Clone)]
pub struct Fallback {
    /// Candidate composables tried in order until one succeeds.
    pub candidates: Vec<Composable>,
    /// Middleware attached to the fallback agent (e.g. `M::on_fallback`).
    middleware: Vec<Arc<dyn Middleware>>,
}

impl std::fmt::Debug for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fallback")
            .field("candidates", &self.candidates)
            .finish()
    }
}

/// Create a conditional loop predicate.
pub fn until(
    predicate: impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static,
) -> LoopPredicate {
    LoopPredicate::new(predicate)
}

// ── Conversions ──

impl From<AgentBuilder> for Composable {
    fn from(b: AgentBuilder) -> Self {
        Composable::Agent(b)
    }
}

impl From<Pipeline> for Composable {
    fn from(p: Pipeline) -> Self {
        Composable::Pipeline(p)
    }
}

impl From<FanOut> for Composable {
    fn from(f: FanOut) -> Self {
        Composable::FanOut(f)
    }
}

impl From<Loop> for Composable {
    fn from(l: Loop) -> Self {
        Composable::Loop(l)
    }
}

impl From<Fallback> for Composable {
    fn from(f: Fallback) -> Self {
        Composable::Fallback(f)
    }
}

// ── Compilation: Composable → TextAgent ──

impl Composable {
    /// Compile this composable tree into an executable `TextAgent`.
    ///
    /// Recursively compiles the tree: pipelines become `SequentialTextAgent`,
    /// fan-outs become `ParallelTextAgent`, loops become `LoopTextAgent`,
    /// fallbacks become `FallbackTextAgent`, and agents compile via
    /// `AgentBuilder::build()`.
    ///
    /// ```rust,ignore
    /// let pipeline = AgentBuilder::new("writer").instruction("Write a draft")
    ///     >> AgentBuilder::new("reviewer").instruction("Review and improve");
    ///
    /// let agent = pipeline.compile(llm);
    /// let result = agent.run(&state).await?;
    /// ```
    pub fn compile(self, llm: Arc<dyn BaseLlm>) -> Arc<dyn TextAgent> {
        match self {
            Composable::Agent(builder) => builder.build(llm),

            Composable::Pipeline(pipeline) => {
                let children: Vec<Arc<dyn TextAgent>> = pipeline
                    .steps
                    .into_iter()
                    .map(|step| step.compile(llm.clone()))
                    .collect();
                Arc::new(SequentialTextAgent::new("pipeline", children))
            }

            Composable::FanOut(fan_out) => {
                let branches: Vec<Arc<dyn TextAgent>> = fan_out
                    .branches
                    .into_iter()
                    .map(|branch| branch.compile(llm.clone()))
                    .collect();
                Arc::new(ParallelTextAgent::new("fan_out", branches))
            }

            Composable::Loop(loop_node) => {
                let middleware = loop_node.middleware;
                let body = loop_node.body.compile(llm);
                let mut loop_agent = LoopTextAgent::new("loop", body, loop_node.max);

                if let Some(predicate) = loop_node.until {
                    loop_agent = loop_agent.until(move |state: &gemini_adk_rs::State| {
                        // Convert State to serde_json::Value for LoopPredicate compatibility.
                        let keys = state.keys();
                        let mut map = serde_json::Map::new();
                        for key in keys {
                            if let Some(val) = state.get_raw(&key) {
                                map.insert(key, val);
                            }
                        }
                        predicate.check(&serde_json::Value::Object(map))
                    });
                }

                if !middleware.is_empty() {
                    loop_agent = loop_agent.with_middleware_chain(chain_from(middleware));
                }

                Arc::new(loop_agent)
            }

            Composable::Fallback(fallback) => {
                let middleware = fallback.middleware;
                let candidates: Vec<Arc<dyn TextAgent>> = fallback
                    .candidates
                    .into_iter()
                    .map(|c| c.compile(llm.clone()))
                    .collect();
                let mut agent = FallbackTextAgent::new("fallback", candidates);
                if !middleware.is_empty() {
                    agent = agent.with_middleware_chain(chain_from(middleware));
                }
                Arc::new(agent)
            }
        }
    }
}

/// Build a [`MiddlewareChain`] from an ordered list of middleware layers.
fn chain_from(layers: Vec<Arc<dyn Middleware>>) -> MiddlewareChain {
    let mut chain = MiddlewareChain::new();
    for layer in layers {
        chain.add(layer);
    }
    chain
}

impl Composable {
    /// Attach middleware to a `Loop` or `Fallback` node — the place where
    /// combinator-level observers (`M::on_loop`, `M::on_fallback`) live.
    ///
    /// For other node kinds (single agent, pipeline, fan-out) this is a no-op:
    /// attach `M::` middleware to the agent itself via
    /// [`AgentBuilder::middleware`](crate::AgentBuilder::middleware) instead.
    pub fn middleware(self, composite: MiddlewareComposite) -> Self {
        match self {
            Composable::Loop(l) => Composable::Loop(l.middleware(composite)),
            Composable::Fallback(f) => Composable::Fallback(f.middleware(composite)),
            other => other,
        }
    }
}

// ── Safe variant accessors ──
//
// These inspect a `Composable` for a specific shape and return `None` when the
// variant does not match, rather than panicking. Callers that want the
// underlying structure should pattern-match directly; these are convenience
// accessors for introspection (tests, tooling, debugging).

impl Composable {
    /// The first step of a [`Pipeline`], or `None` if this is not a pipeline
    /// (or the pipeline is empty).
    pub fn first_step(&self) -> Option<&Composable> {
        match self {
            Composable::Pipeline(p) => p.steps.first(),
            _ => None,
        }
    }

    /// The last step of a [`Pipeline`], or `None` if this is not a pipeline
    /// (or the pipeline is empty).
    pub fn last_step(&self) -> Option<&Composable> {
        match self {
            Composable::Pipeline(p) => p.steps.last(),
            _ => None,
        }
    }

    /// The `n`th step of a [`Pipeline`], or `None` if this is not a pipeline
    /// or the index is out of bounds.
    pub fn nth_step(&self, n: usize) -> Option<&Composable> {
        match self {
            Composable::Pipeline(p) => p.steps.get(n),
            _ => None,
        }
    }

    /// All steps of a [`Pipeline`], or `None` if this is not a pipeline.
    pub fn pipeline_steps(&self) -> Option<&[Composable]> {
        match self {
            Composable::Pipeline(p) => Some(&p.steps),
            _ => None,
        }
    }

    /// The branches of a [`FanOut`], or `None` if this is not a fan-out.
    pub fn fan_out_branches(&self) -> Option<&[Composable]> {
        match self {
            Composable::FanOut(f) => Some(&f.branches),
            _ => None,
        }
    }

    /// The termination predicate of a [`Loop`], or `None` if this is not a loop
    /// (or the loop has no predicate).
    pub fn loop_predicate(&self) -> Option<&LoopPredicate> {
        match self {
            Composable::Loop(l) => l.until.as_ref(),
            _ => None,
        }
    }

    /// The body of a [`Loop`], or `None` if this is not a loop.
    pub fn loop_body(&self) -> Option<&Composable> {
        match self {
            Composable::Loop(l) => Some(&l.body),
            _ => None,
        }
    }

    /// The candidates of a [`Fallback`] chain, or `None` if this is not a fallback.
    pub fn fallback_candidates(&self) -> Option<&[Composable]> {
        match self {
            Composable::Fallback(f) => Some(&f.candidates),
            _ => None,
        }
    }
}

// ── Pipeline construction helpers ──

impl Pipeline {
    /// Create a pipeline from the given steps.
    pub fn new(steps: Vec<Composable>) -> Self {
        Self { steps }
    }

    /// Create an empty named pipeline (fluent builder entry point).
    ///
    /// ```ignore
    /// Pipeline::builder("etl")
    ///     .step(extract_agent)
    ///     .step(transform_agent)
    ///     .step(load_agent)
    /// ```
    pub fn builder(_name: &str) -> Self {
        Self { steps: Vec::new() }
    }

    /// Add a sequential step to this pipeline (fluent builder).
    pub fn step(mut self, agent: impl Into<Composable>) -> Self {
        self.steps.push(agent.into());
        self
    }

    /// Add a sub-agent step (alias for `step` — matches upstream naming).
    pub fn sub_agent(self, agent: AgentBuilder) -> Self {
        self.step(agent)
    }

    /// Set a description (metadata, not used at runtime).
    pub fn describe(self, _desc: &str) -> Self {
        self
    }

    /// Flatten: if a step is itself a Pipeline, inline its steps.
    fn push_flat(&mut self, step: Composable) {
        match step {
            Composable::Pipeline(p) => self.steps.extend(p.steps),
            other => self.steps.push(other),
        }
    }
}

impl FanOut {
    /// Create a fan-out from the given branches.
    pub fn new(branches: Vec<Composable>) -> Self {
        Self { branches }
    }

    /// Create an empty named fan-out (fluent builder entry point).
    ///
    /// ```ignore
    /// FanOut::builder("research")
    ///     .branch(web_agent)
    ///     .branch(db_agent)
    /// ```
    pub fn builder(_name: &str) -> Self {
        Self {
            branches: Vec::new(),
        }
    }

    /// Add a parallel branch (fluent builder).
    pub fn branch(mut self, agent: impl Into<Composable>) -> Self {
        self.branches.push(agent.into());
        self
    }

    /// Add a sub-agent branch (alias for `branch` — matches upstream naming).
    pub fn sub_agent(self, agent: AgentBuilder) -> Self {
        self.branch(agent)
    }

    /// Set a description (metadata, not used at runtime).
    pub fn describe(self, _desc: &str) -> Self {
        self
    }

    fn push_flat(&mut self, branch: Composable) {
        match branch {
            Composable::FanOut(f) => self.branches.extend(f.branches),
            other => self.branches.push(other),
        }
    }
}

impl Fallback {
    /// Create a fallback chain from the given candidates.
    pub fn new(candidates: Vec<Composable>) -> Self {
        Self {
            candidates,
            middleware: Vec::new(),
        }
    }

    /// Attach middleware to the fallback agent (e.g. `M::on_fallback(|name| …)`),
    /// observed when a fallback branch activates.
    pub fn middleware(mut self, composite: MiddlewareComposite) -> Self {
        self.middleware.extend(composite.layers);
        self
    }

    fn push_flat(&mut self, candidate: Composable) {
        match candidate {
            Composable::Fallback(f) => self.candidates.extend(f.candidates),
            other => self.candidates.push(other),
        }
    }
}

// ── Operator: >> (Shr) = Sequential Pipeline ──

/// AgentBuilder >> AgentBuilder → Pipeline
impl std::ops::Shr for AgentBuilder {
    type Output = Composable;

    fn shr(self, rhs: AgentBuilder) -> Self::Output {
        Composable::Pipeline(Pipeline::new(vec![
            Composable::Agent(self),
            Composable::Agent(rhs),
        ]))
    }
}

/// Composable >> AgentBuilder → Pipeline (flattening)
impl std::ops::Shr<AgentBuilder> for Composable {
    type Output = Composable;

    fn shr(self, rhs: AgentBuilder) -> Self::Output {
        let mut pipeline = match self {
            Composable::Pipeline(p) => p,
            other => Pipeline::new(vec![other]),
        };
        pipeline.push_flat(Composable::Agent(rhs));
        Composable::Pipeline(pipeline)
    }
}

/// AgentBuilder >> Composable → Pipeline (flattening)
impl std::ops::Shr<Composable> for AgentBuilder {
    type Output = Composable;

    fn shr(self, rhs: Composable) -> Self::Output {
        let mut pipeline = Pipeline::new(vec![Composable::Agent(self)]);
        pipeline.push_flat(rhs);
        Composable::Pipeline(pipeline)
    }
}

/// Composable >> Composable → Pipeline (flattening)
impl std::ops::Shr for Composable {
    type Output = Composable;

    fn shr(self, rhs: Composable) -> Self::Output {
        let mut pipeline = match self {
            Composable::Pipeline(p) => p,
            other => Pipeline::new(vec![other]),
        };
        pipeline.push_flat(rhs);
        Composable::Pipeline(pipeline)
    }
}

// ── Operator: | (BitOr) = Parallel Fan-Out ──

/// AgentBuilder | AgentBuilder → FanOut
impl std::ops::BitOr for AgentBuilder {
    type Output = Composable;

    fn bitor(self, rhs: AgentBuilder) -> Self::Output {
        Composable::FanOut(FanOut::new(vec![
            Composable::Agent(self),
            Composable::Agent(rhs),
        ]))
    }
}

/// Composable | AgentBuilder → FanOut (flattening)
impl std::ops::BitOr<AgentBuilder> for Composable {
    type Output = Composable;

    fn bitor(self, rhs: AgentBuilder) -> Self::Output {
        let mut fan_out = match self {
            Composable::FanOut(f) => f,
            other => FanOut::new(vec![other]),
        };
        fan_out.push_flat(Composable::Agent(rhs));
        Composable::FanOut(fan_out)
    }
}

/// Composable | Composable → FanOut (flattening)
impl std::ops::BitOr for Composable {
    type Output = Composable;

    fn bitor(self, rhs: Composable) -> Self::Output {
        let mut fan_out = match self {
            Composable::FanOut(f) => f,
            other => FanOut::new(vec![other]),
        };
        fan_out.push_flat(rhs);
        Composable::FanOut(fan_out)
    }
}

// ── Operator: * (Mul<u32>) = Fixed Loop ──

/// AgentBuilder * 3 → Loop(max=3)
impl std::ops::Mul<u32> for AgentBuilder {
    type Output = Composable;

    fn mul(self, rhs: u32) -> Self::Output {
        Composable::Loop(Loop {
            body: Box::new(Composable::Agent(self)),
            max: rhs,
            until: None,
            middleware: Vec::new(),
        })
    }
}

/// Composable * 3 → Loop(max=3)
impl std::ops::Mul<u32> for Composable {
    type Output = Composable;

    fn mul(self, rhs: u32) -> Self::Output {
        Composable::Loop(Loop {
            body: Box::new(self),
            max: rhs,
            until: None,
            middleware: Vec::new(),
        })
    }
}

/// AgentBuilder * until(pred) → conditional Loop
impl std::ops::Mul<LoopPredicate> for AgentBuilder {
    type Output = Composable;

    fn mul(self, rhs: LoopPredicate) -> Self::Output {
        Composable::Loop(Loop {
            body: Box::new(Composable::Agent(self)),
            max: u32::MAX,
            until: Some(rhs),
            middleware: Vec::new(),
        })
    }
}

/// Composable * until(pred) → conditional Loop
impl std::ops::Mul<LoopPredicate> for Composable {
    type Output = Composable;

    fn mul(self, rhs: LoopPredicate) -> Self::Output {
        Composable::Loop(Loop {
            body: Box::new(self),
            max: u32::MAX,
            until: Some(rhs),
            middleware: Vec::new(),
        })
    }
}

// ── Operator: / (Div) = Fallback Chain ──
// Note: Rust doesn't have a `//` operator. We use `/` (Div) instead.

/// AgentBuilder / AgentBuilder → Fallback
impl std::ops::Div for AgentBuilder {
    type Output = Composable;

    fn div(self, rhs: AgentBuilder) -> Self::Output {
        Composable::Fallback(Fallback::new(vec![
            Composable::Agent(self),
            Composable::Agent(rhs),
        ]))
    }
}

/// Composable / AgentBuilder → Fallback (flattening)
impl std::ops::Div<AgentBuilder> for Composable {
    type Output = Composable;

    fn div(self, rhs: AgentBuilder) -> Self::Output {
        let mut fallback = match self {
            Composable::Fallback(f) => f,
            other => Fallback::new(vec![other]),
        };
        fallback.push_flat(Composable::Agent(rhs));
        Composable::Fallback(fallback)
    }
}

/// Composable / Composable → Fallback (flattening)
impl std::ops::Div for Composable {
    type Output = Composable;

    fn div(self, rhs: Composable) -> Self::Output {
        let mut fallback = match self {
            Composable::Fallback(f) => f,
            other => Fallback::new(vec![other]),
        };
        fallback.push_flat(rhs);
        Composable::Fallback(fallback)
    }
}

// ── Loop builder method (for chaining max on until-loops) ──

impl Loop {
    /// Create a loop builder with a body agent and default max iterations.
    ///
    /// ```ignore
    /// Loop::builder("refine")
    ///     .step(refine_agent)
    ///     .max_iterations(5)
    /// ```
    pub fn builder(_name: &str) -> Self {
        Self {
            body: Box::new(Composable::Pipeline(Pipeline::new(Vec::new()))),
            max: 10,
            until: None,
            middleware: Vec::new(),
        }
    }

    /// Attach middleware to the loop agent (e.g. `M::on_loop(|i| …)`), observed
    /// on every iteration.
    pub fn middleware(mut self, composite: MiddlewareComposite) -> Self {
        self.middleware.extend(composite.layers);
        self
    }

    /// Set the body composable to loop over.
    pub fn step(mut self, agent: impl Into<Composable>) -> Self {
        self.body = Box::new(agent.into());
        self
    }

    /// Set a maximum number of iterations.
    pub fn max_iterations(mut self, n: u32) -> Self {
        self.max = n;
        self
    }

    /// Set a maximum number of iterations for a conditional loop.
    pub fn max(mut self, max: u32) -> Self {
        self.max = max;
        self
    }

    /// Set a description (metadata, not used at runtime).
    pub fn describe(self, _desc: &str) -> Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> AgentBuilder {
        AgentBuilder::new(name)
    }

    #[test]
    fn pipeline_from_shr() {
        let result = agent("a") >> agent("b");
        match result {
            Composable::Pipeline(p) => assert_eq!(p.steps.len(), 2),
            _ => panic!("expected Pipeline"),
        }
    }

    #[test]
    fn pipeline_flattens() {
        let result = agent("a") >> agent("b") >> agent("c");
        match result {
            Composable::Pipeline(p) => assert_eq!(p.steps.len(), 3),
            _ => panic!("expected Pipeline"),
        }
    }

    #[test]
    fn fan_out_from_bitor() {
        let result = agent("a") | agent("b");
        match result {
            Composable::FanOut(f) => assert_eq!(f.branches.len(), 2),
            _ => panic!("expected FanOut"),
        }
    }

    #[test]
    fn fan_out_flattens() {
        let result = (agent("a") | agent("b")) | agent("c");
        match result {
            Composable::FanOut(f) => assert_eq!(f.branches.len(), 3),
            _ => panic!("expected FanOut"),
        }
    }

    #[test]
    fn fixed_loop_from_mul() {
        let result = agent("a") * 3;
        match result {
            Composable::Loop(l) => {
                assert_eq!(l.max, 3);
                assert!(l.until.is_none());
            }
            _ => panic!("expected Loop"),
        }
    }

    #[test]
    fn conditional_loop_from_mul_until() {
        let pred = until(|_v| true);
        let result = agent("a") * pred;
        match result {
            Composable::Loop(l) => {
                assert_eq!(l.max, u32::MAX);
                assert!(l.until.is_some());
            }
            _ => panic!("expected Loop"),
        }
    }

    #[test]
    fn fallback_from_div() {
        let result = agent("a") / agent("b");
        match result {
            Composable::Fallback(f) => assert_eq!(f.candidates.len(), 2),
            _ => panic!("expected Fallback"),
        }
    }

    #[test]
    fn fallback_flattens() {
        let result = (agent("a") / agent("b")) / agent("c");
        match result {
            Composable::Fallback(f) => assert_eq!(f.candidates.len(), 3),
            _ => panic!("expected Fallback"),
        }
    }

    #[test]
    fn mixed_pipeline_with_fan_out() {
        let result = agent("a") >> (agent("b") | agent("c"));
        match &result {
            Composable::Pipeline(p) => {
                assert_eq!(p.steps.len(), 2);
                assert!(matches!(&p.steps[1], Composable::FanOut(_)));
            }
            _ => panic!("expected Pipeline"),
        }
    }

    #[test]
    fn pipeline_then_loop() {
        let result = agent("a") >> (agent("b") * 5);
        match &result {
            Composable::Pipeline(p) => {
                assert_eq!(p.steps.len(), 2);
                assert!(matches!(&p.steps[1], Composable::Loop(_)));
            }
            _ => panic!("expected Pipeline"),
        }
    }

    #[test]
    fn safe_accessors_return_some_on_match() {
        let pipeline = agent("a").instruction("x") >> agent("b").instruction("y");
        assert!(pipeline.first_step().is_some());
        assert!(pipeline.last_step().is_some());
        assert!(pipeline.nth_step(1).is_some());
        assert!(pipeline.nth_step(99).is_none());
        assert_eq!(pipeline.pipeline_steps().map(|s| s.len()), Some(2));

        let fan_out = Composable::Agent(agent("a")) | Composable::Agent(agent("b"));
        assert_eq!(fan_out.fan_out_branches().map(|b| b.len()), Some(2));

        let looped = agent("a") * until(|_| true);
        assert!(looped.loop_predicate().is_some());
        assert!(looped.loop_body().is_some());

        let fallback = agent("a") / agent("b");
        assert_eq!(fallback.fallback_candidates().map(|c| c.len()), Some(2));
    }

    #[test]
    fn safe_accessors_return_none_on_mismatch() {
        // Calling a pipeline accessor on a non-Pipeline returns None, not panic.
        let solo = Composable::Agent(agent("solo"));
        assert!(solo.first_step().is_none());
        assert!(solo.last_step().is_none());
        assert!(solo.nth_step(0).is_none());
        assert!(solo.pipeline_steps().is_none());
        assert!(solo.fan_out_branches().is_none());
        assert!(solo.loop_predicate().is_none());
        assert!(solo.loop_body().is_none());
        assert!(solo.fallback_candidates().is_none());

        // A fixed loop (no predicate) returns None for loop_predicate but
        // Some for loop_body.
        let fixed = agent("a") * 3;
        assert!(fixed.loop_predicate().is_none());
        assert!(fixed.loop_body().is_some());
        // And a pipeline accessor on a loop is None.
        assert!(fixed.first_step().is_none());
    }

    #[test]
    fn loop_predicate_check() {
        let pred = until(|v| v.get("done").and_then(|v| v.as_bool()).unwrap_or(false));
        assert!(!pred.check(&serde_json::json!({"done": false})));
        assert!(pred.check(&serde_json::json!({"done": true})));
    }

    // ── compile() tests ──

    mod compile_tests {
        use super::*;
        use async_trait::async_trait;
        use gemini_adk_rs::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
        use gemini_genai_rs::prelude::{Content, Part, Role};

        /// A mock LLM that returns its agent's name from the system instruction.
        struct NameEchoLlm;

        #[async_trait]
        impl BaseLlm for NameEchoLlm {
            fn model_id(&self) -> &str {
                "name-echo"
            }
            async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
                let text = req
                    .system_instruction
                    .unwrap_or_else(|| "no-instruction".into());
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text { text }],
                    },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            }
        }

        fn llm() -> Arc<dyn BaseLlm> {
            Arc::new(NameEchoLlm)
        }

        #[tokio::test]
        async fn compile_single_agent() {
            let composable = Composable::Agent(AgentBuilder::new("solo").instruction("hello"));
            let agent = composable.compile(llm());
            let state = gemini_adk_rs::State::new();
            let result = agent.run(&state).await.unwrap();
            assert_eq!(result, "hello");
        }

        #[tokio::test]
        async fn compile_pipeline() {
            let pipeline = agent("a").instruction("step-a") >> agent("b").instruction("step-b");
            let compiled = pipeline.compile(llm());
            let state = gemini_adk_rs::State::new();
            let result = compiled.run(&state).await.unwrap();
            // Sequential: last agent's output wins. step-b echoes its instruction.
            assert_eq!(result, "step-b");
        }

        #[tokio::test]
        async fn compile_fan_out() {
            let fan_out = Composable::Agent(agent("a").instruction("branch-a"))
                | Composable::Agent(agent("b").instruction("branch-b"));
            let compiled = fan_out.compile(llm());
            let state = gemini_adk_rs::State::new();
            let result = compiled.run(&state).await.unwrap();
            assert!(result.contains("branch-a"));
            assert!(result.contains("branch-b"));
        }

        #[tokio::test]
        async fn compile_loop() {
            let looped = agent("counter").instruction("tick") * 3;
            let compiled = looped.compile(llm());
            let state = gemini_adk_rs::State::new();
            let result = compiled.run(&state).await.unwrap();
            assert_eq!(result, "tick");
        }

        #[tokio::test]
        async fn compile_fallback() {
            let fallback = agent("a").instruction("first") / agent("b").instruction("second");
            let compiled = fallback.compile(llm());
            let state = gemini_adk_rs::State::new();
            let result = compiled.run(&state).await.unwrap();
            // First agent succeeds, so its result is returned.
            assert_eq!(result, "first");
        }

        #[tokio::test]
        async fn on_loop_fires_through_operator() {
            use crate::compose::M;
            use std::sync::atomic::{AtomicU32, Ordering};

            let count = Arc::new(AtomicU32::new(0));
            let c2 = count.clone();
            // Attach the combinator-level observer to the loop node.
            let looped =
                (agent("counter").instruction("tick") * 3).middleware(M::on_loop(move |_i| {
                    c2.fetch_add(1, Ordering::SeqCst);
                }));
            let compiled = looped.compile(llm());
            let state = gemini_adk_rs::State::new();
            compiled.run(&state).await.unwrap();
            // Three iterations → three LoopIteration events observed.
            assert_eq!(count.load(Ordering::SeqCst), 3);
        }

        #[tokio::test]
        async fn compile_loop_with_predicate() {
            // Use a mock LLM that increments state on each call.
            struct IncrementLlm;

            #[async_trait]
            impl BaseLlm for IncrementLlm {
                fn model_id(&self) -> &str {
                    "incr"
                }
                async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                    Ok(LlmResponse {
                        content: Content {
                            role: Some(Role::Model),
                            parts: vec![Part::Text {
                                text: "done".into(),
                            }],
                        },
                        finish_reason: Some("STOP".into()),
                        usage: None,
                    })
                }
            }

            // Build a FnTextAgent-driven loop instead to test predicate.
            // We'll test via the operators directly.
            let pred = until(|v| v.get("n").and_then(|v| v.as_i64()).unwrap_or(0) >= 3);
            let body = agent("incr").instruction("increment");
            let looped = body * pred;

            // Compile it. The predicate checks state for "n" >= 3, but
            // the mock LLM doesn't set "n". Loop will run max iterations.
            // This tests that the predicate is wired through.
            let compiled = looped.compile(Arc::new(IncrementLlm));
            let state = gemini_adk_rs::State::new();
            let _ = state.set("n", 5); // Pre-set to pass predicate immediately.
            let result = compiled.run(&state).await.unwrap();
            assert_eq!(result, "done"); // Ran once, predicate passed.
        }

        #[tokio::test]
        async fn compile_mixed_pipeline_with_fan_out() {
            let mixed = agent("a").instruction("start")
                >> (Composable::Agent(agent("b").instruction("left"))
                    | Composable::Agent(agent("c").instruction("right")));
            let compiled = mixed.compile(llm());
            let state = gemini_adk_rs::State::new();
            let result = compiled.run(&state).await.unwrap();
            assert!(result.contains("left"));
            assert!(result.contains("right"));
        }
    }
}
