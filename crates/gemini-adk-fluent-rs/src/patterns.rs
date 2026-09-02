//! Pre-built patterns — common multi-agent workflows.
//!
//! High-level functions that compose agents into standard patterns:
//! review loops, cascades, fan-out-merge, supervised workflows, etc.
//!
//! Each function returns a [`Composable`] that can be compiled into an
//! executable [`TextAgent`](gemini_adk_rs::text::TextAgent) via
//! [`Composable::compile()`](crate::operators::Composable::compile).
//!
//! # Examples
//!
//! ```
//! use gemini_adk_fluent_rs::prelude::*;
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
//! # let _ = (draft, robust, research);
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
/// ```no_run
/// # use gemini_adk_fluent_rs::prelude::*;
/// # use std::sync::Arc;
/// # fn run(llm: Arc<dyn BaseLlm>) -> Result<(), ConfigError> {
/// let workflow = review_loop(
///     AgentBuilder::new("writer").instruction("Write a blog post"),
///     AgentBuilder::new("editor").instruction("Review. Set approved=true if publication-ready."),
///     3,
/// );
/// let agent = workflow.compile(llm)?;
/// # let _ = agent; Ok(())
/// # }
/// ```
pub fn review_loop(author: AgentBuilder, reviewer: AgentBuilder, max_rounds: usize) -> Composable {
    let inner = Composable::Pipeline(Pipeline::new(vec![
        Composable::Agent(author),
        Composable::Agent(reviewer),
    ]));

    Composable::Loop(Loop {
        body: Box::new(inner),
        max: max_rounds as u32,
        middleware: Vec::new(),
        name: None,
        description: None,
        until: Some(LoopPredicate::new(|state| {
            state
                .get("approved")
                .and_then(serde_json::Value::as_bool)
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
        middleware: Vec::new(),
        name: None,
        description: None,
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
/// ```
/// # use gemini_adk_fluent_rs::prelude::*;
/// let robust = cascade(vec![
///     AgentBuilder::new("fast").instruction("Quick answer"),
///     AgentBuilder::new("thorough").instruction("Detailed answer"),
/// ]);
/// assert!(matches!(robust, Composable::Fallback(_)));
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
/// ```
/// # use gemini_adk_fluent_rs::prelude::*;
/// let research = fan_out_merge(
///     vec![
///         AgentBuilder::new("web-search").instruction("Search the web"),
///         AgentBuilder::new("db-lookup").instruction("Query the database"),
///     ],
///     AgentBuilder::new("synthesizer").instruction("Combine research findings"),
/// );
/// assert!(matches!(research, Composable::Pipeline(_)));
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
/// ```
/// # use gemini_adk_fluent_rs::prelude::*;
/// let pipeline = chain(vec![
///     AgentBuilder::new("extract"),
///     AgentBuilder::new("transform"),
///     AgentBuilder::new("load"),
/// ]);
/// assert!(matches!(pipeline, Composable::Pipeline(_)));
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
/// ```
/// # use gemini_adk_fluent_rs::prelude::*;
/// let routed = conditional(
///     |state| state.get("premium").and_then(|v| v.as_bool()).unwrap_or(false),
///     AgentBuilder::new("premium-agent").instruction("Full-featured response"),
///     AgentBuilder::new("basic-agent").instruction("Basic response"),
/// );
/// assert!(matches!(routed, Composable::Fallback(_)));
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
        middleware: Vec::new(),
        name: None,
        description: None,
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
/// ```
/// # use gemini_adk_fluent_rs::prelude::*;
/// let managed = supervised(
///     AgentBuilder::new("coder").instruction("Write the implementation"),
///     AgentBuilder::new("lead").instruction("Code review. Set approved=true if ready to merge."),
///     5,
/// );
/// assert!(matches!(managed, Composable::Loop(_)));
/// ```
pub fn supervised(worker: AgentBuilder, supervisor: AgentBuilder, max_rounds: usize) -> Composable {
    let inner = Composable::Pipeline(Pipeline::new(vec![
        Composable::Agent(worker),
        Composable::Agent(supervisor),
    ]));

    Composable::Loop(Loop {
        body: Box::new(inner),
        max: max_rounds as u32,
        middleware: Vec::new(),
        name: None,
        description: None,
        until: Some(LoopPredicate::new(|state| {
            state
                .get("approved")
                .and_then(serde_json::Value::as_bool)
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
        middleware: Vec::new(),
        name: None,
        description: None,
        until: Some(LoopPredicate::new(move |state| {
            state
                .get(&key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })),
    })
}

/// Map-over: apply one agent to every item of a state list.
///
/// At run time the compiled node reads the JSON array at `state[list_key]`,
/// runs `agent` once per item with the item in `state["_item"]` (and as the
/// agent's `input`), and collects the outputs into `state["_results"]`.
/// Items run sequentially — `MapOver::item_key`/`output_key` rename the
/// slots. Composes like any other node:
///
/// ```
/// # use gemini_adk_fluent_rs::prelude::*;
/// let batch = map_over(AgentBuilder::new("summarize"), "documents")
///     >> AgentBuilder::new("merge");
/// assert!(matches!(batch, Composable::Pipeline(_)));
/// ```
pub fn map_over(agent: AgentBuilder, list_key: impl Into<String>) -> Composable {
    Composable::MapOver(MapOver::new(agent, list_key))
}

/// A map-over workflow node — applies one agent to many items. Build with
/// [`map_over`]; compiles to `MapOverTextAgent`.
#[derive(Clone, Debug)]
pub struct MapOver {
    /// The agent template applied to each item.
    pub agent: AgentBuilder,
    /// State key holding the JSON array to iterate.
    pub list_key: String,
    /// State key the current item is written to (default `"_item"`).
    pub item_key: String,
    /// State key the collected outputs are written to (default `"_results"`).
    pub output_key: String,
    /// Name given to the compiled agent (default `"map_over"`).
    pub name: Option<String>,
}

impl MapOver {
    /// Create a map-over node for `agent` over the list at `list_key`.
    pub fn new(agent: AgentBuilder, list_key: impl Into<String>) -> Self {
        Self {
            agent,
            list_key: list_key.into(),
            item_key: "_item".into(),
            output_key: "_results".into(),
            name: None,
        }
    }

    /// State key the current item is written to for each run.
    pub fn item_key(mut self, key: impl Into<String>) -> Self {
        self.item_key = key.into();
        self
    }

    /// State key the collected outputs are written to.
    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = key.into();
        self
    }

    /// Name the compiled agent.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Map-reduce: map `mapper` over the list at `list_key`, then run `reducer`
/// over the collected results (`state["_results"]`). A pipeline of a
/// [`map_over`] node and the reducer.
pub fn map_reduce(
    mapper: AgentBuilder,
    reducer: AgentBuilder,
    list_key: impl Into<String>,
) -> Composable {
    Composable::Pipeline(Pipeline::new(vec![
        map_over(mapper, list_key),
        Composable::Agent(reducer),
    ]))
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
            |state| {
                state
                    .get("flag")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            },
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
    fn map_over_is_a_composable_node() {
        match map_over(agent("processor"), "items") {
            Composable::MapOver(m) => {
                assert_eq!(m.agent.name(), "processor");
                assert_eq!(m.list_key, "items");
                assert_eq!(m.item_key, "_item");
            }
            other => panic!("expected MapOver, got {other:?}"),
        }
    }

    #[test]
    fn map_reduce_is_map_over_then_reducer() {
        match map_reduce(agent("mapper"), agent("reducer"), "items") {
            Composable::Pipeline(p) => {
                assert_eq!(p.steps.len(), 2);
                assert!(
                    matches!(&p.steps[0], Composable::MapOver(m) if m.agent.name() == "mapper")
                );
                assert!(matches!(&p.steps[1], Composable::Agent(a) if a.name() == "reducer"));
            }
            other => panic!("expected Pipeline, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn map_over_compiles_and_runs_per_item() {
        use gemini_adk_rs::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
        use gemini_genai_rs::prelude::{Content, Part, Role};
        use std::sync::Arc;

        struct Echo;
        #[async_trait::async_trait]
        impl BaseLlm for Echo {
            fn model_id(&self) -> &str {
                "echo"
            }
            async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
                let text = req
                    .contents
                    .iter()
                    .flat_map(|c| &c.parts)
                    .filter_map(|p| match p {
                        Part::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
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

        let node = map_over(agent("echo"), "items");
        let compiled = node.compile(Arc::new(Echo)).expect("compiles");
        let state = gemini_adk_rs::State::new();
        let _ = state.set("items", serde_json::json!(["a", "b"]));
        let out = compiled.run(&state).await.expect("runs");
        assert!(out.contains("\"a\"") && out.contains("\"b\""), "{out}");
        let results: Vec<String> = state.get("_results").unwrap_or_default();
        assert_eq!(results.len(), 2);
    }
}
