//! Workflow graph runtime — the ADK 2.0 "graph execution" pattern.
//!
//! A [`Workflow`] is a directed acyclic graph (DAG) of execution nodes with
//! dependencies. Unlike the [`crate::flow`] module (which *governs* conversation
//! and tool-call ordering), workflows *execute* node graphs concurrently: agents,
//! functions, and human-in-the-loop approval nodes.
//!
//! # Execution Model
//!
//! The runtime repeatedly collects *ready* nodes — those whose dependencies are
//! all in a terminal state (finished or skipped; `join_any` nodes are ready as
//! soon as one dependency *finishes*). Ready nodes whose `when` guard evaluates
//! to `false` are marked *skipped* and do not run, and skips cascade: a node
//! all of whose dependencies were skipped is itself skipped — an unselected
//! branch never executes side effects. Ready nodes run concurrently via
//! `tokio::task::JoinSet`, and readiness is recomputed after **every** node
//! completion, so a `join_any` dependent starts the moment its first
//! dependency finishes rather than waiting out the wave.
//!
//! Agent and function nodes write their outputs to:
//! - The `WorkflowRun::outputs` map (under their node ID)
//! - State key `workflow:<node_id>` (as JSON, for downstream instruction templates)
//!
//! Approval nodes suspend until an external [`WorkflowController`] approves or
//! rejects them; decisions arriving before the node is ready are stored, never
//! lost. Rejection fails the entire run. [`Workflow::run`] (no controller)
//! rejects workflows containing approval nodes up front instead of hanging.
//!
//! # DAG Invariants
//!
//! - Cycle detection (topological check) at build time.
//! - Unknown dependencies rejected at build time.
//! - Duplicate node IDs rejected at build time.
//! - Deadlock guard: if unfinished nodes exist but none are ready, the run fails.
//!
//! # Example
//!
//! ```rust,ignore
//! let wf = Workflow::builder()
//!     .function("fetch", |s| async { Ok(json!({"data": "value"})) })
//!     .function("process", |s| async { Ok(json!({"result": "done"})) })
//!     .after(&["fetch"])
//!     .approval("review")
//!     .after(&["process"])
//!     .build()?;
//!
//! let run = wf.run_with(&state, controller).await?;
//! assert!(run.outputs.contains_key("review"));
//! ```

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Notify;
use tokio::task::JoinSet;

use crate::error::AgentError;
use crate::state::State;
use crate::text::TextAgent;
use crate::{AsyncSourceFn, StatePredicate};

// ──────────────────────────────────────────────────────────────────────────────
// Error Type
// ──────────────────────────────────────────────────────────────────────────────

/// Errors that can occur during workflow execution or construction.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    /// The workflow graph contains a cycle.
    #[error("Workflow cycle detected: {0}")]
    CycleDetected(String),

    /// A node references a dependency that does not exist.
    #[error("Unknown dependency: {0}")]
    UnknownDependency(String),

    /// Duplicate node ID in the workflow.
    #[error("Duplicate node ID: {0}")]
    DuplicateNodeId(String),

    /// A node failed during execution.
    #[error("Node '{node_id}' failed: {reason}")]
    NodeFailed {
        /// The ID of the node that failed.
        node_id: String,
        /// The error message.
        reason: String,
    },

    /// An approval node was rejected.
    #[error("Approval node '{node_id}' rejected: {reason}")]
    Rejected {
        /// The ID of the approval node.
        node_id: String,
        /// The rejection reason.
        reason: String,
    },

    /// Deadlock: unfinished nodes but none are ready.
    #[error("Workflow deadlock: no ready nodes but work remains")]
    Deadlock,

    /// The workflow contains an approval node but was started without a
    /// controller ([`Workflow::run`]); nobody could ever approve it.
    #[error("Approval node '{0}' requires run_with(state, controller)")]
    ApprovalWithoutController(String),

    /// An agent node execution failed.
    #[error("Agent error in node '{node_id}': {source}")]
    AgentError {
        /// The ID of the agent node.
        node_id: String,
        /// The underlying agent error.
        source: AgentError,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// Public API Types
// ──────────────────────────────────────────────────────────────────────────────

/// The result of a completed workflow run.
#[derive(Debug, Clone)]
pub struct WorkflowRun {
    /// Output values keyed by node ID.
    pub outputs: HashMap<String, Value>,
    /// Node IDs that were skipped (guard returned false, or all deps were skipped).
    pub skipped: Vec<String>,
}

/// Controller for HITL (human-in-the-loop) approval nodes.
///
/// Approve or reject nodes by ID. Decisions are durable: a decision made
/// *before* the approval node becomes ready (an external response arriving
/// while upstream nodes still run) is stored and consumed when the node
/// reaches the wait — never lost to a wakeup race.
#[derive(Debug, Default)]
pub struct WorkflowController {
    /// Per-node decision: `Ok(())` approved, `Err(reason)` rejected.
    decisions: tokio::sync::RwLock<HashMap<String, Result<(), String>>>,
    /// Waiters registered by approval nodes that reached the wait.
    waiters: tokio::sync::RwLock<HashMap<String, Arc<Notify>>>,
}

impl WorkflowController {
    /// Create a new empty controller.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Approve a node by ID. Unblocks the waiting approval node; if the node
    /// has not reached its wait yet, the approval is stored for it.
    pub async fn approve(&self, node_id: &str) {
        self.decide(node_id, Ok(())).await;
    }

    /// Reject a node by ID with a reason. This fails the entire workflow run;
    /// an early rejection is stored like an early approval.
    pub async fn reject(&self, node_id: &str, reason: impl Into<String>) {
        self.decide(node_id, Err(reason.into())).await;
    }

    async fn decide(&self, node_id: &str, decision: Result<(), String>) {
        self.decisions
            .write()
            .await
            .insert(node_id.to_string(), decision);
        if let Some(notify) = self.waiters.read().await.get(node_id) {
            notify.notify_one();
        }
    }

    /// Suspend until a decision exists for `node_id`, consuming an earlier
    /// decision immediately if one was already recorded.
    async fn wait_for_decision(&self, node_id: &str) -> Result<(), String> {
        let notify = Arc::new(Notify::new());
        self.waiters
            .write()
            .await
            .insert(node_id.to_string(), notify.clone());
        loop {
            // Create the notified future BEFORE checking, so a decision that
            // lands between the check and the await stores a permit for us.
            let notified = notify.notified();
            if let Some(decision) = self.decisions.read().await.get(node_id) {
                self.waiters.write().await.remove(node_id);
                return decision.clone();
            }
            notified.await;
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Internal Node Types
// ──────────────────────────────────────────────────────────────────────────────

/// The kind of a workflow node.
enum NodeKind {
    Agent(Arc<dyn TextAgent>),
    Function(AsyncSourceFn),
    Approval,
}

/// Configuration for a single workflow node.
struct WorkflowNode {
    id: String,
    kind: NodeKind,
    dependencies: Vec<String>,
    when: Option<StatePredicate>,
    join_any: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// Workflow Builder
// ──────────────────────────────────────────────────────────────────────────────

/// Builder for constructing a [`Workflow`].
///
/// Each method appends a node and returns `self` for chaining. Modifiers like
/// `after()` and `when()` apply to the most recently added node.
pub struct WorkflowBuilder {
    nodes: Vec<WorkflowNode>,
}

impl WorkflowBuilder {
    /// Start building a workflow.
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Add an agent node. The agent's returned string is stored as a JSON string value.
    pub fn agent(mut self, id: &str, agent: Arc<dyn TextAgent>) -> Self {
        self.nodes.push(WorkflowNode {
            id: id.to_string(),
            kind: NodeKind::Agent(agent),
            dependencies: Vec::new(),
            when: None,
            join_any: false,
        });
        self
    }

    /// Add a function node. The function receives a State clone and returns a JSON value.
    pub fn function<F, Fut>(mut self, id: &str, f: F) -> Self
    where
        F: Fn(State) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        let wrapped: AsyncSourceFn = Arc::new(move |state| Box::pin(f(state)));

        self.nodes.push(WorkflowNode {
            id: id.to_string(),
            kind: NodeKind::Function(wrapped),
            dependencies: Vec::new(),
            when: None,
            join_any: false,
        });
        self
    }

    /// Add an approval node. This node suspends until the controller approves or rejects.
    pub fn approval(mut self, id: &str) -> Self {
        self.nodes.push(WorkflowNode {
            id: id.to_string(),
            kind: NodeKind::Approval,
            dependencies: Vec::new(),
            when: None,
            join_any: false,
        });
        self
    }

    /// Set dependencies for the most recently added node. Node runs after all deps are terminal.
    pub fn after(mut self, deps: &[&str]) -> Self {
        if let Some(node) = self.nodes.last_mut() {
            node.dependencies = deps.iter().map(std::string::ToString::to_string).collect();
        }
        self
    }

    /// Set a guard for the most recently added node. If guard returns false, node is skipped.
    pub fn when<G>(mut self, guard: G) -> Self
    where
        G: Fn(&State) -> bool + Send + Sync + 'static,
    {
        if let Some(node) = self.nodes.last_mut() {
            node.when = Some(Arc::new(guard));
        }
        self
    }

    /// For the most recently added node, use `join_any` semantics: ready when ANY dep finishes
    /// (instead of ALL). Skipped only if ALL deps skipped.
    pub fn join_any(mut self) -> Self {
        if let Some(node) = self.nodes.last_mut() {
            node.join_any = true;
        }
        self
    }

    /// Build and validate the workflow. Returns an error if there are cycles,
    /// unknown dependencies, or duplicate IDs.
    pub fn build(self) -> Result<Workflow, WorkflowError> {
        // Check for duplicate IDs.
        let mut seen_ids = HashSet::new();
        for node in &self.nodes {
            if !seen_ids.insert(&node.id) {
                return Err(WorkflowError::DuplicateNodeId(node.id.clone()));
            }
        }

        // Check for unknown dependencies.
        let id_set: HashSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        for node in &self.nodes {
            for dep in &node.dependencies {
                if !id_set.contains(dep.as_str()) {
                    return Err(WorkflowError::UnknownDependency(dep.clone()));
                }
            }
        }

        // Topological sort to detect cycles.
        self.check_acyclic()?;

        Ok(Workflow { nodes: self.nodes })
    }

    /// Check for cycles using DFS.
    fn check_acyclic(&self) -> Result<(), WorkflowError> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        for node in &self.nodes {
            if !visited.contains(&node.id) {
                self.dfs(&node.id, &mut visited, &mut rec_stack)?;
            }
        }
        Ok(())
    }

    /// DFS helper for cycle detection.
    fn dfs(
        &self,
        node_id: &str,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
    ) -> Result<(), WorkflowError> {
        visited.insert(node_id.to_string());
        rec_stack.insert(node_id.to_string());

        let node = self.nodes.iter().find(|n| n.id == node_id);
        if let Some(node) = node {
            for dep in &node.dependencies {
                if !visited.contains(dep) {
                    self.dfs(dep, visited, rec_stack)?;
                } else if rec_stack.contains(dep) {
                    return Err(WorkflowError::CycleDetected(format!("{node_id} -> {dep}")));
                }
            }
        }

        rec_stack.remove(node_id);
        Ok(())
    }
}

impl Default for WorkflowBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Workflow Execution
// ──────────────────────────────────────────────────────────────────────────────

/// A validated workflow graph ready for execution.
pub struct Workflow {
    nodes: Vec<WorkflowNode>,
}

impl Workflow {
    /// Create a new workflow builder.
    pub fn builder() -> WorkflowBuilder {
        WorkflowBuilder::new()
    }

    /// Run the workflow without HITL support. A workflow containing an
    /// approval node is rejected up front — with no controller exposed,
    /// nothing could ever approve it and the run would hang.
    pub async fn run(&self, state: &State) -> Result<WorkflowRun, WorkflowError> {
        if let Some(node) = self
            .nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Approval))
        {
            return Err(WorkflowError::ApprovalWithoutController(node.id.clone()));
        }
        self.run_with(state, WorkflowController::new()).await
    }

    /// Run the workflow with HITL controller for approval nodes.
    ///
    /// The scheduler recomputes readiness after **every** node completion, so
    /// a `join_any` dependent starts the moment its first dependency finishes
    /// — it never waits out the rest of the wave.
    pub async fn run_with(
        &self,
        state: &State,
        controller: Arc<WorkflowController>,
    ) -> Result<WorkflowRun, WorkflowError> {
        let mut outputs: HashMap<String, Value> = HashMap::new();
        let mut skipped: Vec<String> = Vec::new();
        let mut finished: HashSet<String> = HashSet::new();
        let mut running: HashSet<String> = HashSet::new();
        let mut join_set: JoinSet<(String, Result<Value, String>)> = JoinSet::new();

        loop {
            // Schedule until quiescent: guard-skips cascade (a skip can make a
            // dependent's deps terminal, which may skip it in turn), so loop
            // to a fixpoint before waiting on completions.
            loop {
                let mut changed = false;
                for node in &self.nodes {
                    if finished.contains(&node.id)
                        || skipped.iter().any(|s| s == &node.id)
                        || running.contains(&node.id)
                    {
                        continue;
                    }
                    let is_terminal =
                        |d: &String| finished.contains(d) || skipped.iter().any(|s| s == d);
                    let deps_ready = if node.join_any {
                        // join_any: a *finished* dep makes the node ready; a
                        // skipped dep alone does not trigger execution.
                        node.dependencies.is_empty()
                            || node.dependencies.iter().any(|d| finished.contains(d))
                    } else {
                        node.dependencies.iter().all(&is_terminal)
                    };
                    // A node all of whose deps were skipped is itself skipped
                    // — the branch was not selected, so its side effects must
                    // not run. (Both join modes: with every dep skipped there
                    // is no finished dep to satisfy join_any either.)
                    let all_deps_skipped = !node.dependencies.is_empty()
                        && node.dependencies.iter().all(&is_terminal)
                        && !node.dependencies.iter().any(|d| finished.contains(d));
                    if all_deps_skipped {
                        skipped.push(node.id.clone());
                        changed = true;
                        continue;
                    }
                    if !deps_ready {
                        continue;
                    }
                    if node.when.as_ref().is_some_and(|guard| !guard(state)) {
                        skipped.push(node.id.clone());
                        changed = true;
                        continue;
                    }

                    // Spawn the node.
                    running.insert(node.id.clone());
                    changed = true;
                    let node_id = node.id.clone();
                    let state_clone = state.clone();
                    match &node.kind {
                        NodeKind::Agent(agent) => {
                            let agent = agent.clone();
                            join_set.spawn(async move {
                                let value = match agent.run(&state_clone).await {
                                    Ok(s) => Ok(Value::String(s)),
                                    Err(e) => Err(e.to_string()),
                                };
                                (node_id, value)
                            });
                        }
                        NodeKind::Function(f) => {
                            let f = f.clone();
                            join_set.spawn(async move {
                                let result = f(state_clone).await;
                                (node_id, result)
                            });
                        }
                        NodeKind::Approval => {
                            let controller = controller.clone();
                            join_set.spawn(async move {
                                let value = match controller.wait_for_decision(&node_id).await {
                                    Ok(()) => Ok(Value::Bool(true)),
                                    Err(reason) => Err(format!("Rejected: {reason}")),
                                };
                                (node_id, value)
                            });
                        }
                    }
                }
                if !changed {
                    break;
                }
            }

            if running.is_empty() {
                if finished.len() + skipped.len() < self.nodes.len() {
                    return Err(WorkflowError::Deadlock);
                }
                break; // All done.
            }

            // Wait for ONE completion, then reschedule — this is what lets a
            // join_any dependent start while its slower siblings still run.
            match join_set.join_next().await {
                Some(Ok((node_id, value_result))) => {
                    running.remove(&node_id);
                    match value_result {
                        Ok(value) => {
                            outputs.insert(node_id.clone(), value.clone());
                            // Also write to state under workflow:<id>.
                            let _ = state.set(format!("workflow:{node_id}"), value);
                            finished.insert(node_id);
                        }
                        Err(e) => {
                            // Fail fast; dropping the JoinSet aborts siblings.
                            if let Some(reason) = e.strip_prefix("Rejected: ") {
                                return Err(WorkflowError::Rejected {
                                    node_id,
                                    reason: reason.to_string(),
                                });
                            }
                            return Err(WorkflowError::NodeFailed { node_id, reason: e });
                        }
                    }
                }
                Some(Err(e)) => {
                    return Err(WorkflowError::NodeFailed {
                        node_id: "unknown".to_string(),
                        reason: e.to_string(),
                    });
                }
                None => unreachable!("running non-empty implies join_set non-empty"),
            }
        }

        Ok(WorkflowRun { outputs, skipped })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Test types
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
/// A simple test agent that echoes the "input" state key.
struct EchoAgent;

#[cfg(test)]
#[async_trait::async_trait]
impl TextAgent for EchoAgent {
    fn name(&self) -> &str {
        "echo"
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let input: String = state.get("input").unwrap_or_else(|| "empty".to_string());
        Ok(format!("Echo: {input}"))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_diamond_graph() {
        // Diamond: a -> (b, c) -> d
        let wf = Workflow::builder()
            .function("a", |state| async move {
                state.set("a_val", "value_a").map_err(|e| e.to_string())?;
                Ok(json!({"a": "done"}))
            })
            .function("b", |state| async move {
                let a_val: Option<String> = state.get("a_val");
                let b_val = format!("value_b_from_{a_val:?}");
                state.set("b_val", b_val).map_err(|e| e.to_string())?;
                Ok(json!({"b": "done"}))
            })
            .after(&["a"])
            .function("c", |state| async move {
                let a_val: Option<String> = state.get("a_val");
                let c_val = format!("value_c_from_{a_val:?}");
                state.set("c_val", c_val).map_err(|e| e.to_string())?;
                Ok(json!({"c": "done"}))
            })
            .after(&["a"])
            .function("d", |state| async move {
                let b_val: Option<String> = state.get("b_val");
                let c_val: Option<String> = state.get("c_val");
                let d_val = format!("value_d_from_b_{b_val:?}_c_{c_val:?}");
                state.set("d_val", d_val).map_err(|e| e.to_string())?;
                Ok(json!({"d": "done"}))
            })
            .after(&["b", "c"])
            .build()
            .expect("valid workflow");

        let state = State::new();
        let run = wf.run(&state).await.expect("run succeeds");

        assert_eq!(run.outputs.len(), 4);
        assert!(run.outputs.contains_key("a"));
        assert!(run.outputs.contains_key("b"));
        assert!(run.outputs.contains_key("c"));
        assert!(run.outputs.contains_key("d"));
        assert!(run.skipped.is_empty());
    }

    #[tokio::test]
    async fn test_when_guard_skips_node_and_cascades() {
        let wf = Workflow::builder()
            .function("a", |_state| async move { Ok(json!({"a": "done"})) })
            .function("b", |_state| async move { Ok(json!({"b": "done"})) })
            .after(&["a"])
            .when(|_state| false) // Always skip
            .function("c", |_state| async move { Ok(json!({"c": "done"})) })
            .after(&["b"])
            .build()
            .expect("valid workflow");

        let state = State::new();
        let run = wf.run(&state).await.expect("run succeeds");

        // b's branch was not selected, so c (whose only dep was skipped)
        // must not execute its side effects either.
        assert_eq!(run.outputs.len(), 1);
        assert!(run.outputs.contains_key("a"));
        assert!(!run.outputs.contains_key("b"));
        assert!(!run.outputs.contains_key("c"));
        assert_eq!(run.skipped, vec!["b", "c"]);
    }

    #[tokio::test]
    async fn test_join_any() {
        let wf = Workflow::builder()
            .function("a", |_state| async move { Ok(json!({"a": "done"})) })
            .function("b", |_state| async move { Ok(json!({"b": "done"})) })
            .when(|_state| false) // Skipped
            .function("c", |_state| async move { Ok(json!({"c": "done"})) })
            .after(&["a", "b"])
            .join_any()
            .build()
            .expect("valid workflow");

        let state = State::new();
        let run = wf.run(&state).await.expect("run succeeds");

        assert!(run.outputs.contains_key("a"));
        assert!(!run.outputs.contains_key("b")); // Skipped
        assert!(run.outputs.contains_key("c")); // Ran despite b being skipped
        assert_eq!(run.skipped, vec!["b"]);
    }

    #[tokio::test]
    async fn test_unknown_dep_rejected_at_build() {
        let result = Workflow::builder()
            .function("a", |_state| async move { Ok(json!({})) })
            .function("b", |_state| async move { Ok(json!({})) })
            .after(&["unknown_node"])
            .build();

        assert!(matches!(result, Err(WorkflowError::UnknownDependency(_))));
    }

    #[tokio::test]
    async fn test_duplicate_id_rejected_at_build() {
        let result = Workflow::builder()
            .function("a", |_state| async move { Ok(json!({})) })
            .function("a", |_state| async move { Ok(json!({})) }) // Duplicate
            .build();

        assert!(matches!(result, Err(WorkflowError::DuplicateNodeId(_))));
    }

    #[tokio::test]
    async fn test_approval_approve() {
        let wf = Workflow::builder()
            .function("a", |_state| async move { Ok(json!({"a": "done"})) })
            .approval("review")
            .after(&["a"])
            .build()
            .expect("valid workflow");

        let state = State::new();
        let controller = WorkflowController::new();
        let controller_clone = controller.clone();

        // Spawn approval runner in background.
        let run_task = tokio::spawn(async move { wf.run_with(&state, controller_clone).await });

        // Give the run a moment to reach the approval node.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Approve it.
        controller.approve("review").await;

        // Wait for run to complete.
        let run = run_task.await.expect("task panicked");
        assert!(run.is_ok());
        let run = run.unwrap();
        assert!(run.outputs.contains_key("a"));
        assert!(run.outputs.contains_key("review"));
    }

    #[tokio::test]
    async fn test_approval_reject() {
        let wf = Workflow::builder()
            .function("a", |_state| async move { Ok(json!({"a": "done"})) })
            .approval("review")
            .after(&["a"])
            .build()
            .expect("valid workflow");

        let state = State::new();
        let controller = WorkflowController::new();
        let controller_clone = controller.clone();

        let run_task = tokio::spawn(async move { wf.run_with(&state, controller_clone).await });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        controller.reject("review", "not approved").await;

        let run = run_task.await.expect("task panicked");
        assert!(run.is_err());
        match run.unwrap_err() {
            WorkflowError::Rejected { node_id, .. } => {
                assert_eq!(node_id, "review");
            }
            e => panic!("Expected Rejected, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn test_run_rejects_approval_without_controller() {
        let wf = Workflow::builder()
            .approval("gate")
            .build()
            .expect("valid workflow");

        let state = State::new();
        // Must error immediately instead of hanging on an unreachable gate.
        match wf.run(&state).await.unwrap_err() {
            WorkflowError::ApprovalWithoutController(id) => assert_eq!(id, "gate"),
            e => panic!("Expected ApprovalWithoutController, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn test_early_approval_is_not_lost() {
        let wf = Workflow::builder()
            .function("a", |_state| async move { Ok(json!({"a": "done"})) })
            .approval("review")
            .after(&["a"])
            .build()
            .expect("valid workflow");

        let state = State::new();
        let controller = WorkflowController::new();
        // Decide BEFORE the approval node is ready (before the run starts):
        // the decision must be stored and consumed at the wait, not dropped.
        controller.approve("review").await;

        let run = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            wf.run_with(&state, controller),
        )
        .await
        .expect("run must not hang on an early approval")
        .expect("run succeeds");
        assert!(run.outputs.contains_key("review"));
    }

    #[tokio::test]
    async fn test_early_rejection_is_not_lost() {
        let wf = Workflow::builder()
            .approval("review")
            .build()
            .expect("valid workflow");

        let state = State::new();
        let controller = WorkflowController::new();
        controller.reject("review", "denied up front").await;

        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            wf.run_with(&state, controller),
        )
        .await
        .expect("run must not hang on an early rejection");
        assert!(matches!(result, Err(WorkflowError::Rejected { .. })));
    }

    #[tokio::test]
    async fn test_join_any_starts_before_slow_sibling_completes() {
        // d is join_any on (a, b). b blocks until d has run (via a state
        // flag), so the run only completes if the scheduler starts d after
        // a finishes, while b is still in flight. A wave-based scheduler
        // deadlocks here; the timeout turns that into a failure.
        let wf = Workflow::builder()
            .function("a", |_state| async move { Ok(json!({"a": "done"})) })
            .function("b", |state| async move {
                for _ in 0..500 {
                    if state.get::<bool>("d_ran").unwrap_or(false) {
                        return Ok(json!({"b": "done"}));
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
                Err("d never ran while b was in flight".to_string())
            })
            .function("d", |state| async move {
                state.set("d_ran", true).map_err(|e| e.to_string())?;
                Ok(json!({"d": "done"}))
            })
            .after(&["a", "b"])
            .join_any()
            .build()
            .expect("valid workflow");

        let state = State::new();
        let run = tokio::time::timeout(tokio::time::Duration::from_secs(10), wf.run(&state))
            .await
            .expect("join_any dependent must start before the slow sibling")
            .expect("run succeeds");
        assert!(run.outputs.contains_key("d"));
        assert!(run.outputs.contains_key("b"));
    }

    #[tokio::test]
    async fn test_agent_node() {
        let echo = Arc::new(EchoAgent);

        let wf = Workflow::builder()
            .agent("echo_node", echo)
            .build()
            .expect("valid workflow");

        let state = State::new();
        state.set("input", "hello").expect("state set succeeds");

        let run = wf.run(&state).await.expect("run succeeds");
        assert!(run.outputs.contains_key("echo_node"));
        let output = &run.outputs["echo_node"];
        assert!(output.is_string());
        let s = output.as_str().unwrap();
        assert!(s.contains("Echo") && s.contains("hello"));
    }
}
