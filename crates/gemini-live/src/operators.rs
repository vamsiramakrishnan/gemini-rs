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

use crate::builder::AgentBuilder;

/// A composable workflow node — can be sequenced, fan-out, looped, etc.
#[derive(Clone, Debug)]
pub enum Composable {
    Agent(AgentBuilder),
    Pipeline(Pipeline),
    FanOut(FanOut),
    Loop(Loop),
    Fallback(Fallback),
}

/// Sequential pipeline: execute steps in order, passing state between them.
#[derive(Clone, Debug)]
pub struct Pipeline {
    pub steps: Vec<Composable>,
}

/// Parallel fan-out: execute branches concurrently, merge results.
#[derive(Clone, Debug)]
pub struct FanOut {
    pub branches: Vec<Composable>,
}

/// Loop: repeat an agent or pipeline up to `max` times, or until a predicate.
#[derive(Clone)]
pub struct Loop {
    pub body: Box<Composable>,
    pub max: u32,
    pub until: Option<LoopPredicate>,
}

/// Predicate for conditional loop termination.
#[derive(Clone)]
pub struct LoopPredicate {
    predicate: std::sync::Arc<dyn Fn(&serde_json::Value) -> bool + Send + Sync>,
}

impl LoopPredicate {
    pub fn new(f: impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static) -> Self {
        Self {
            predicate: std::sync::Arc::new(f),
        }
    }

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

// ── Pipeline construction helpers ──

impl Pipeline {
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
}
