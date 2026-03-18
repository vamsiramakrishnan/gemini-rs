//! Pre-built patterns — common multi-agent workflows.
//!
//! High-level functions that compose agents into standard patterns:
//! review loops, cascades, fan-out-merge, supervised workflows, etc.
//!
//! Each function returns a [`Composable`] that can be compiled into an
//! executable [`TextAgent`](gemini_adk::text::TextAgent) via
//! [`Composable::compile()`](crate::operators::Composable::compile).
//!
//! # Examples
//!
//! ```rust,ignore
//! use gemini_adk_fluent::prelude::*;
//!
//! // Review loop: author writes, reviewer checks, loop until approved
//! let draft = review_loop(
//!     AgentBuilder::new("author").instruction("Write an essay"),
//!     AgentBuilder::new("reviewer").instruction("Review and set approved=true when good"),
//!     3,
//! );
//!
//! // Cascade: try agents in order, first success wins
//! let robust = cascade(vec![
//!     AgentBuilder::new("primary"),
//!     AgentBuilder::new("fallback"),
//! ]);
//!
//! // Fan-out-merge: parallel agents, then merge
//! let research = fan_out_merge(
//!     vec![AgentBuilder::new("web"), AgentBuilder::new("db")],
//!     AgentBuilder::new("synthesizer"),
//! );
//! ```

use crate::builder::AgentBuilder;
use crate::operators::{Composable, Fallback, FanOut, Loop, LoopPredicate, Pipeline};

/// Review loop: author writes, reviewer checks, loop until approved.
///
/// The author agent produces output, then the reviewer evaluates it.
/// The loop terminates when the reviewer sets `"approved"` to `true`
/// in the state, or after `max_rounds` iterations.
///
/// # Arguments
///
/// * `author` — The agent that produces drafts.
/// * `reviewer` — The agent that evaluates and sets `"approved": true` when satisfied.
/// * `max_rounds` — Maximum number of author-reviewer cycles.
///
/// # Example
///
/// ```rust,ignore
/// let workflow = review_loop(
///     AgentBuilder::new("writer").instruction("Write a blog post"),
///     AgentBuilder::new("editor").instruction("Review. Set approved=true if publication-ready."),
///     3,
/// );
/// let agent = workflow.compile(llm);
/// ```
pub fn review_loop(author: AgentBuilder, reviewer: AgentBuilder, max_rounds: usize) -> Composable {
    let inner = Composable::Pipeline(Pipeline::new(vec![
        Composable::Agent(author),
        Composable::Agent(reviewer),
    ]));

    Composable::Loop(Loop {
        body: Box::new(inner),
        max: max_rounds as u32,
        until: Some(LoopPredicate::new(|state| {
            state
                .get("approved")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })),
    })
}

/// Review loop with a custom quality key and target value.
///
/// Like [`review_loop`] but allows specifying which state key the reviewer
/// writes to and what value signals completion.
///
/// # Arguments
///
/// * `worker` — The agent that produces output.
/// * `reviewer` — The agent that evaluates quality.
/// * `quality_key` — State key the reviewer writes (e.g., `"quality"`).
/// * `target` — Value of `quality_key` that signals completion (e.g., `"good"`).
/// * `max_rounds` — Maximum iterations.
pub fn review_loop_keyed(
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

/// Cascade: try agents in sequence, first success wins.
///
/// This is an alias for a fallback chain. Each agent is tried in order;
/// the first one that succeeds provides the result.
///
/// # Example
///
/// ```rust,ignore
/// let robust = cascade(vec![
///     AgentBuilder::new("fast").instruction("Quick answer"),
///     AgentBuilder::new("thorough").instruction("Detailed answer"),
/// ]);
/// ```
pub fn cascade(agents: Vec<AgentBuilder>) -> Composable {
    Composable::Fallback(Fallback::new(
        agents.into_iter().map(Composable::Agent).collect(),
    ))
}

/// Fan-out-merge: run agents in parallel, then merge results with a merger agent.
///
/// All `agents` execute concurrently via fan-out. Their combined output is
/// then fed into the `merger` agent, which synthesizes a final result.
///
/// # Arguments
///
/// * `agents` — Agents to run in parallel.
/// * `merger` — Agent that merges the parallel results.
///
/// # Example
///
/// ```rust,ignore
/// let research = fan_out_merge(
///     vec![
///         AgentBuilder::new("web-search").instruction("Search the web"),
///         AgentBuilder::new("db-lookup").instruction("Query the database"),
///     ],
///     AgentBuilder::new("synthesizer").instruction("Combine research findings"),
/// );
/// ```
pub fn fan_out_merge(agents: Vec<AgentBuilder>, merger: AgentBuilder) -> Composable {
    let fan_out = Composable::FanOut(FanOut::new(
        agents.into_iter().map(Composable::Agent).collect(),
    ));

    Composable::Pipeline(Pipeline::new(vec![fan_out, Composable::Agent(merger)]))
}

/// Chain: simple sequential pipeline of agents.
///
/// This is an alias for the `>>` operator but accepts a `Vec`.
/// Each agent runs in order, with the output of one feeding into the next.
///
/// # Example
///
/// ```rust,ignore
/// let pipeline = chain(vec![
///     AgentBuilder::new("extract"),
///     AgentBuilder::new("transform"),
///     AgentBuilder::new("load"),
/// ]);
/// ```
pub fn chain(agents: Vec<AgentBuilder>) -> Composable {
    Composable::Pipeline(Pipeline::new(
        agents.into_iter().map(Composable::Agent).collect(),
    ))
}

/// Conditional: route to one of two agents based on a state predicate.
///
/// Evaluates `predicate` against the current state. If it returns `true`,
/// the `if_true` agent runs; otherwise, the `if_false` agent runs.
///
/// # Arguments
///
/// * `predicate` — Function that inspects state (as `serde_json::Value`) and returns a bool.
/// * `if_true` — Agent to run when the predicate is true.
/// * `if_false` — Agent to run when the predicate is false.
///
/// # Example
///
/// ```rust,ignore
/// let routed = conditional(
///     |state| state.get("premium").and_then(|v| v.as_bool()).unwrap_or(false),
///     AgentBuilder::new("premium-agent").instruction("Full-featured response"),
///     AgentBuilder::new("basic-agent").instruction("Basic response"),
/// );
/// ```
pub fn conditional(
    predicate: impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static,
    if_true: AgentBuilder,
    if_false: AgentBuilder,
) -> Composable {
    let pred = std::sync::Arc::new(predicate);
    let pred_clone = pred.clone();

    let true_branch = AgentBuilder::new(if_true.name())
        .instruction(if_true.get_instruction().unwrap_or_default());
    let false_branch = AgentBuilder::new(if_false.name())
        .instruction(if_false.get_instruction().unwrap_or_default());

    // Store predicate in a loop with max=1 for the true branch,
    // fall back to false branch.
    let guarded = Composable::Loop(Loop {
        body: Box::new(Composable::Agent(true_branch)),
        max: 1,
        until: Some(LoopPredicate::new(move |state| pred_clone(state))),
    });

    Composable::Fallback(Fallback::new(vec![
        guarded,
        Composable::Agent(false_branch),
    ]))
}

/// Supervised: worker with supervisor oversight loop.
///
/// The worker agent produces output, then the supervisor reviews it.
/// The loop repeats until the supervisor sets `"approved"` to `true`
/// in the state, or after `max_rounds` iterations.
///
/// This is semantically similar to [`review_loop`] but framed as a
/// worker-supervisor relationship rather than author-reviewer.
///
/// # Arguments
///
/// * `worker` — The agent that performs the task.
/// * `supervisor` — The agent that oversees and approves work.
/// * `max_rounds` — Maximum number of worker-supervisor cycles.
///
/// # Example
///
/// ```rust,ignore
/// let managed = supervised(
///     AgentBuilder::new("coder").instruction("Write the implementation"),
///     AgentBuilder::new("lead").instruction("Code review. Set approved=true if ready to merge."),
///     5,
/// );
/// ```
pub fn supervised(worker: AgentBuilder, supervisor: AgentBuilder, max_rounds: usize) -> Composable {
    let inner = Composable::Pipeline(Pipeline::new(vec![
        Composable::Agent(worker),
        Composable::Agent(supervisor),
    ]));

    Composable::Loop(Loop {
        body: Box::new(inner),
        max: max_rounds as u32,
        until: Some(LoopPredicate::new(|state| {
            state
                .get("approved")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })),
    })
}

/// Supervised with a custom approval key.
///
/// Like [`supervised`] but allows specifying which state key signals approval.
///
/// # Arguments
///
/// * `worker` — The agent that performs the task.
/// * `supervisor` — The agent that oversees work.
/// * `approval_key` — State key the supervisor sets to `true` when satisfied.
/// * `max_revisions` — Maximum iterations.
pub fn supervised_keyed(
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
            state.get(&key).and_then(|v| v.as_bool()).unwrap_or(false)
        })),
    })
}

/// Map-over: apply a single agent to multiple items concurrently.
///
/// Returns a `MapOver` composable that stores the agent template and concurrency limit.
pub fn map_over(agent: AgentBuilder, concurrency: usize) -> MapOver {
    MapOver { agent, concurrency }
}

/// A map-over workflow node — applies one agent to many items.
#[derive(Clone, Debug)]
pub struct MapOver {
    /// The agent template applied to each item.
    pub agent: AgentBuilder,
    /// Maximum number of concurrent executions.
    pub concurrency: usize,
}

/// Map-reduce: apply a mapper agent to items, then a reducer to combine results.
pub fn map_reduce(mapper: AgentBuilder, reducer: AgentBuilder, concurrency: usize) -> MapReduce {
    MapReduce {
        mapper,
        reducer,
        concurrency,
    }
}

/// A map-reduce workflow node.
#[derive(Clone, Debug)]
pub struct MapReduce {
    /// The mapper agent applied to each item.
    pub mapper: AgentBuilder,
    /// The reducer agent that combines mapped results.
    pub reducer: AgentBuilder,
    /// Maximum concurrency for the map phase.
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
        let result = review_loop(agent("writer"), agent("reviewer"), 3);
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
    fn review_loop_predicate_checks_approved() {
        let result = review_loop(agent("w"), agent("r"), 3);
        if let Composable::Loop(l) = result {
            let pred = l.until.unwrap();
            assert!(!pred.check(&serde_json::json!({"approved": false})));
            assert!(pred.check(&serde_json::json!({"approved": true})));
            assert!(!pred.check(&serde_json::json!({})));
        }
    }

    #[test]
    fn review_loop_keyed_predicate_works() {
        let result = review_loop_keyed(agent("w"), agent("r"), "quality", "good", 3);
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
    fn fan_out_merge_creates_pipeline_with_fan_out_then_merger() {
        let result = fan_out_merge(vec![agent("a"), agent("b")], agent("merger"));
        match &result {
            Composable::Pipeline(p) => {
                assert_eq!(p.steps.len(), 2);
                assert!(matches!(&p.steps[0], Composable::FanOut(f) if f.branches.len() == 2));
                assert!(matches!(&p.steps[1], Composable::Agent(a) if a.name() == "merger"));
            }
            _ => panic!("expected Pipeline"),
        }
    }

    #[test]
    fn chain_creates_pipeline() {
        let result = chain(vec![agent("a"), agent("b"), agent("c")]);
        match result {
            Composable::Pipeline(p) => assert_eq!(p.steps.len(), 3),
            _ => panic!("expected Pipeline"),
        }
    }

    #[test]
    fn conditional_creates_fallback_with_guard() {
        let result = conditional(
            |state| state.get("flag").and_then(|v| v.as_bool()).unwrap_or(false),
            agent("yes").instruction("true branch"),
            agent("no").instruction("false branch"),
        );
        match &result {
            Composable::Fallback(f) => assert_eq!(f.candidates.len(), 2),
            _ => panic!("expected Fallback"),
        }
    }

    #[test]
    fn supervised_creates_loop() {
        let result = supervised(agent("worker"), agent("supervisor"), 5);
        match &result {
            Composable::Loop(l) => {
                assert_eq!(l.max, 5);
                assert!(l.until.is_some());
                assert!(matches!(&*l.body, Composable::Pipeline(p) if p.steps.len() == 2));
            }
            _ => panic!("expected Loop"),
        }
    }

    #[test]
    fn supervised_predicate_checks_approved() {
        let result = supervised(agent("w"), agent("s"), 5);
        if let Composable::Loop(l) = result {
            let pred = l.until.unwrap();
            assert!(!pred.check(&serde_json::json!({"approved": false})));
            assert!(pred.check(&serde_json::json!({"approved": true})));
        }
    }

    #[test]
    fn supervised_keyed_predicate_works() {
        let result = supervised_keyed(agent("w"), agent("s"), "approved", 5);
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

    #[test]
    fn map_reduce_stores_params() {
        let mr = map_reduce(agent("mapper"), agent("reducer"), 8);
        assert_eq!(mr.mapper.name(), "mapper");
        assert_eq!(mr.reducer.name(), "reducer");
        assert_eq!(mr.concurrency, 8);
    }
}
