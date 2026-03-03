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

use rs_adk::llm::BaseLlm;
use rs_adk::text::{
    FallbackTextAgent, LoopTextAgent, ParallelTextAgent, SequentialTextAgent, TextAgent,
};

use crate::builder::AgentBuilder;

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
#[derive(Clone, Debug)]
pub struct Fallback {
    /// Candidate composables tried in order until one succeeds.
    pub candidates: Vec<Composable>,
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
                let body = loop_node.body.compile(llm);
                let mut loop_agent = LoopTextAgent::new("loop", body, loop_node.max);

                if let Some(predicate) = loop_node.until {
                    loop_agent = loop_agent.until(move |state: &rs_adk::State| {
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

                Arc::new(loop_agent)
            }

            Composable::Fallback(fallback) => {
                let candidates: Vec<Arc<dyn TextAgent>> = fallback
                    .candidates
                    .into_iter()
                    .map(|c| c.compile(llm.clone()))
                    .collect();
                Arc::new(FallbackTextAgent::new("fallback", candidates))
            }
        }
    }
}

// ── Pipeline construction helpers ──

impl Pipeline {
    /// Create a pipeline from the given steps.
    pub fn new(steps: Vec<Composable>) -> Self {
        Self { steps }
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
        Self { candidates }
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
    /// Set a maximum number of iterations for a conditional loop.
    pub fn max(mut self, max: u32) -> Self {
        self.max = max;
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
    fn loop_predicate_check() {
        let pred = until(|v| v.get("done").and_then(|v| v.as_bool()).unwrap_or(false));
        assert!(!pred.check(&serde_json::json!({"done": false})));
        assert!(pred.check(&serde_json::json!({"done": true})));
    }

    // ── compile() tests ──

    mod compile_tests {
        use super::*;
        use async_trait::async_trait;
        use rs_adk::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
        use rs_genai::prelude::{Content, Part, Role};

        /// A mock LLM that returns its agent's name from the system instruction.
        struct NameEchoLlm;

        #[async_trait]
        impl BaseLlm for NameEchoLlm {
            fn model_id(&self) -> &str { "name-echo" }
            async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
                let text = req.system_instruction.unwrap_or_else(|| "no-instruction".into());
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
            let composable = Composable::Agent(
                AgentBuilder::new("solo").instruction("hello"),
            );
            let agent = composable.compile(llm());
            let state = rs_adk::State::new();
            let result = agent.run(&state).await.unwrap();
            assert_eq!(result, "hello");
        }

        #[tokio::test]
        async fn compile_pipeline() {
            let pipeline = agent("a").instruction("step-a")
                >> agent("b").instruction("step-b");
            let compiled = pipeline.compile(llm());
            let state = rs_adk::State::new();
            let result = compiled.run(&state).await.unwrap();
            // Sequential: last agent's output wins. step-b echoes its instruction.
            assert_eq!(result, "step-b");
        }

        #[tokio::test]
        async fn compile_fan_out() {
            let fan_out = Composable::Agent(agent("a").instruction("branch-a"))
                | Composable::Agent(agent("b").instruction("branch-b"));
            let compiled = fan_out.compile(llm());
            let state = rs_adk::State::new();
            let result = compiled.run(&state).await.unwrap();
            assert!(result.contains("branch-a"));
            assert!(result.contains("branch-b"));
        }

        #[tokio::test]
        async fn compile_loop() {
            let looped = agent("counter").instruction("tick") * 3;
            let compiled = looped.compile(llm());
            let state = rs_adk::State::new();
            let result = compiled.run(&state).await.unwrap();
            assert_eq!(result, "tick");
        }

        #[tokio::test]
        async fn compile_fallback() {
            let fallback = agent("a").instruction("first") / agent("b").instruction("second");
            let compiled = fallback.compile(llm());
            let state = rs_adk::State::new();
            let result = compiled.run(&state).await.unwrap();
            // First agent succeeds, so its result is returned.
            assert_eq!(result, "first");
        }

        #[tokio::test]
        async fn compile_loop_with_predicate() {
            // Use a mock LLM that increments state on each call.
            struct IncrementLlm;

            #[async_trait]
            impl BaseLlm for IncrementLlm {
                fn model_id(&self) -> &str { "incr" }
                async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                    Ok(LlmResponse {
                        content: Content {
                            role: Some(Role::Model),
                            parts: vec![Part::Text { text: "done".into() }],
                        },
                        finish_reason: Some("STOP".into()),
                        usage: None,
                    })
                }
            }

            // Build a FnTextAgent-driven loop instead to test predicate.
            // We'll test via the operators directly.
            let pred = until(|v| {
                v.get("n").and_then(|v| v.as_i64()).unwrap_or(0) >= 3
            });
            let body = agent("incr").instruction("increment");
            let looped = body * pred;

            // Compile it. The predicate checks state for "n" >= 3, but
            // the mock LLM doesn't set "n". Loop will run max iterations.
            // This tests that the predicate is wired through.
            let compiled = looped.compile(Arc::new(IncrementLlm));
            let state = rs_adk::State::new();
            state.set("n", 5); // Pre-set to pass predicate immediately.
            let result = compiled.run(&state).await.unwrap();
            assert_eq!(result, "done"); // Ran once, predicate passed.
        }

        #[tokio::test]
        async fn compile_mixed_pipeline_with_fan_out() {
            let mixed = agent("a").instruction("start")
                >> (Composable::Agent(agent("b").instruction("left"))
                    | Composable::Agent(agent("c").instruction("right")));
            let compiled = mixed.compile(llm());
            let state = rs_adk::State::new();
            let result = compiled.run(&state).await.unwrap();
            assert!(result.contains("left"));
            assert!(result.contains("right"));
        }
    }
}
