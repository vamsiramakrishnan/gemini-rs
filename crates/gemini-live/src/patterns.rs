//! Pre-built patterns — common multi-agent workflows.
//!
//! High-level functions that compose agents into standard patterns:
//! review loops, cascades, fan-out-merge, supervised workflows, etc.

use crate::builder::AgentBuilder;
use crate::operators::{Composable, Fallback, FanOut, Loop, LoopPredicate, Pipeline};

/// Review loop: worker → reviewer → repeat until quality target.
///
/// The worker produces output, the reviewer evaluates on `quality_key`,
/// and the loop repeats until the target quality is reached (max `max_rounds`).
pub fn review_loop(
    worker: AgentBuilder,
    reviewer: AgentBuilder,
    quality_key: &str,
    target: &str,
    max_rounds: u32,
) -> Composable {
    let key = quality_key.to_string();
    let target = target.to_string();

    let inner = Composable::Pipeline(Pipeline::new(vec![
        Composable::Agent(worker),
        Composable::Agent(reviewer),
    ]));

    Composable::Loop(Loop {
        body: Box::new(inner),
        max: max_rounds,
        until: Some(LoopPredicate::new(move |state| {
            state
                .get(&key)
                .and_then(|v| v.as_str())
                .map(|v| v == target)
                .unwrap_or(false)
        })),
    })
}

/// Cascade: try each agent in sequence until one succeeds.
pub fn cascade(agents: Vec<AgentBuilder>) -> Composable {
    Composable::Fallback(Fallback::new(
        agents.into_iter().map(Composable::Agent).collect(),
    ))
}

/// Fan-out-merge: run all agents in parallel, merge results.
pub fn fan_out_merge(agents: Vec<AgentBuilder>) -> Composable {
    Composable::FanOut(FanOut::new(
        agents.into_iter().map(Composable::Agent).collect(),
    ))
}

/// Supervised: worker → supervisor → repeat until approval.
///
/// Like `review_loop` but uses a distinct approval key and semantic framing.
pub fn supervised(
    worker: AgentBuilder,
    supervisor: AgentBuilder,
    approval_key: &str,
    max_revisions: u32,
) -> Composable {
    let key = approval_key.to_string();

    let inner = Composable::Pipeline(Pipeline::new(vec![
        Composable::Agent(worker),
        Composable::Agent(supervisor),
    ]));

    Composable::Loop(Loop {
        body: Box::new(inner),
        max: max_revisions,
        until: Some(LoopPredicate::new(move |state| {
            state
                .get(&key)
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })),
    })
}

/// Map-over: apply a single agent to multiple items concurrently.
///
/// Returns a `MapOver` composable that stores the agent template and concurrency limit.
pub fn map_over(agent: AgentBuilder, concurrency: usize) -> MapOver {
    MapOver {
        agent,
        concurrency,
    }
}

/// A map-over workflow node — applies one agent to many items.
#[derive(Clone, Debug)]
pub struct MapOver {
    pub agent: AgentBuilder,
    pub concurrency: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> AgentBuilder {
        AgentBuilder::new(name)
    }

    #[test]
    fn review_loop_creates_loop_with_pipeline() {
        let result = review_loop(
            agent("writer"),
            agent("reviewer"),
            "quality",
            "good",
            3,
        );
        match &result {
            Composable::Loop(l) => {
                assert_eq!(l.max, 3);
                assert!(l.until.is_some());
                assert!(matches!(&*l.body, Composable::Pipeline(p) if p.steps.len() == 2));
            }
            _ => panic!("expected Loop"),
        }
    }

    #[test]
    fn review_loop_predicate_works() {
        let result = review_loop(agent("w"), agent("r"), "quality", "good", 3);
        if let Composable::Loop(l) = result {
            let pred = l.until.unwrap();
            assert!(!pred.check(&serde_json::json!({"quality": "bad"})));
            assert!(pred.check(&serde_json::json!({"quality": "good"})));
        }
    }

    #[test]
    fn cascade_creates_fallback() {
        let result = cascade(vec![agent("a"), agent("b"), agent("c")]);
        match result {
            Composable::Fallback(f) => assert_eq!(f.candidates.len(), 3),
            _ => panic!("expected Fallback"),
        }
    }

    #[test]
    fn fan_out_merge_creates_fan_out() {
        let result = fan_out_merge(vec![agent("a"), agent("b")]);
        match result {
            Composable::FanOut(f) => assert_eq!(f.branches.len(), 2),
            _ => panic!("expected FanOut"),
        }
    }

    #[test]
    fn supervised_creates_loop() {
        let result = supervised(agent("worker"), agent("supervisor"), "approved", 5);
        match &result {
            Composable::Loop(l) => {
                assert_eq!(l.max, 5);
                assert!(l.until.is_some());
            }
            _ => panic!("expected Loop"),
        }
    }

    #[test]
    fn supervised_predicate_works() {
        let result = supervised(agent("w"), agent("s"), "approved", 5);
        if let Composable::Loop(l) = result {
            let pred = l.until.unwrap();
            assert!(!pred.check(&serde_json::json!({"approved": false})));
            assert!(pred.check(&serde_json::json!({"approved": true})));
        }
    }

    #[test]
    fn map_over_stores_params() {
        let m = map_over(agent("processor"), 4);
        assert_eq!(m.agent.name(), "processor");
        assert_eq!(m.concurrency, 4);
    }
}
