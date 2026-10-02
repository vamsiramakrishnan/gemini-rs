//! Task definitions, commands, and observations shared by Live and replay.
use crate::{
    flow::{FlowSnapshot, FlowStack},
    state::State,
    tool::ToolDispatcher,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

/// A session-local activation identity.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct TaskId(pub String);
/// A session-local invocation and approval identity.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct OperationId(pub String);
/// The installed definition pinned by an activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillKey {
    /// Installed name.
    pub name: String,
    /// Installed version.
    pub version: String,
}
/// Input/output validation supplied by the authoring layer.
pub type ValueValidator = Arc<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;
/// Builds fresh governance for each activation.
pub type FlowFactory = Arc<dyn Fn() -> Result<FlowStack, String> + Send + Sync>;
/// Binds all captured state in tools to an isolated invocation state.
pub type TaskToolFactory = Arc<dyn Fn(State) -> Result<ToolDispatcher, String> + Send + Sync>;
/// Projects declared outputs from accepted task state.
pub type OutputProjector = Arc<dyn Fn(&State) -> Result<Value, String> + Send + Sync>;
/// Reconstructs declared state effects from a verified external receipt without executing a tool.
pub type ReceiptApplier = Arc<dyn Fn(&str, &Value, &State) -> Result<(), String> + Send + Sync>;
/// Builds fresh service registries, counters and resource bindings for an activation.
pub type TaskServicesFactory = Arc<dyn Fn() -> Result<super::TaskServices, String> + Send + Sync>;
/// Executable capability definition. Trusted closures must not capture unrelated mutable state.
#[derive(Clone)]
pub struct CompiledSkill {
    /// Optional per-activation extraction, derivation, watchers, patterns and memory.
    pub services: Option<TaskServicesFactory>,
    /// Catalog lookup name.
    pub name: String,
    /// Version pinned on activation.
    pub version: String,
    /// Routing description.
    pub description: String,
    /// Foreground speaking instruction.
    pub instruction: String,
    /// JSON input contract.
    pub input_schema: Value,
    /// JSON output contract.
    pub output_schema: Value,
    /// Compiled input boundary check.
    pub validate_input: ValueValidator,
    /// Compiled output boundary check.
    pub validate_output: ValueValidator,
    /// Independent defaults copied at activation.
    pub initial_state: HashMap<String, Value>,
    /// Optional conversation governance.
    pub flow_factory: Option<FlowFactory>,
    /// Invocation-local tools.
    pub tool_factory: TaskToolFactory,
    /// Available tool metadata and effect policies.
    pub tools: Vec<TaskTool>,
    /// When present, completion exports these accepted state values instead of caller output.
    pub project_output: Option<OutputProjector>,
    /// Pure state reconstruction for trusted reconciliation. Custom stateful tools
    /// must supply their own mapping; failed-worker writes are never trusted.
    pub receipt_applier: Option<ReceiptApplier>,
}
/// One declared operation. Names are local to a skill.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskTool {
    /// Local tool name.
    pub name: String,
    /// Model-facing purpose.
    pub description: String,
    /// Function argument schema.
    pub parameters: Option<Value>,
    /// Whether this operation can commit an external effect.
    pub effect: TaskToolEffect,
}
/// Effects determine admission, approval, cancellation and reconciliation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskToolEffect {
    /// Does not commit an external mutation.
    Read,
    /// Requires explicit approval and a complete key passed to the external adapter.
    Commit {
        /// String argument carrying the external idempotency key.
        idempotency_argument: String,
    },
}
/// Trusted application/UI command. Models must never receive Decide or Reconcile.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum TaskCommand {
    /// Start a new independent activation and make it foreground.
    Start {
        /// Installed skill name.
        skill: String,
        /// Input validated against that skill's contract.
        #[serde(default = "empty_object")]
        input: Value,
        /// Optional parent resumed when this child finishes.
        #[serde(default)]
        parent: Option<TaskId>,
    },
    /// Suspend a task without losing its accepted progress.
    Suspend {
        #[doc = "Activation to suspend."]
        task: TaskId,
    },
    /// Make an existing nonterminal activation foreground.
    Resume {
        #[doc = "Activation to resume."]
        task: TaskId,
    },
    /// Replace inputs and governance, invalidating prior work and approvals.
    Revise {
        /// Activation to revise.
        task: TaskId,
        /// Revision the caller observed.
        expected_revision: u64,
        /// Complete replacement input.
        input: Value,
    },
    /// Cancel this task; an already dispatched commit may still finish.
    Cancel {
        #[doc = "Activation to cancel."]
        task: TaskId,
    },
    /// Validate and export a task's result; resume its suspended parent.
    Complete {
        /// Activation to finish.
        task: TaskId,
        /// Output used when the skill has no state projector; defaults to an empty object.
        #[serde(default = "empty_object")]
        output: Value,
    },
    /// Admit a tool for the foreground activation.
    Invoke {
        /// Activation whose tool should run.
        task: TaskId,
        /// Skill-local tool name.
        tool: String,
        /// Exact proposed arguments.
        args: Value,
        /// Stable external operation key for a commit.
        #[serde(default)]
        idempotency_key: Option<String>,
    },
    /// Trusted decision about an immutable pending proposal.
    Decide {
        /// Pending operation to approve or decline.
        operation: OperationId,
        /// Whether the exact proposal is approved.
        approve: bool,
    },
    /// Trusted externally verified resolution of an unknown commit outcome.
    Reconcile {
        /// Operation whose external outcome was verified.
        operation: OperationId,
        /// Verified result.
        outcome: ReconciledOutcome,
    },
}
/// An externally verified commit result, not an inference from cancellation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReconciledOutcome {
    /// External service confirms the operation succeeded.
    Succeeded {
        #[doc = "Verified external result."]
        result: Value,
    },
    /// External service confirms no committed effect remains.
    Failed {
        #[doc = "Verified failure description."]
        error: String,
    },
}
/// Task lifecycle, separate from the lifecycle of its external operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Owns the speaking foreground.
    Running,
    /// Retains progress while another task speaks.
    Suspended,
    /// Exported a validated output.
    Completed,
    /// Cancelled; external operations retain independent receipts.
    Cancelled,
    /// Could not continue.
    Failed,
}
/// Operation lifecycle, including uncertainty after an interrupted commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Waiting for a trusted decision about exact arguments.
    AwaitingApproval,
    /// Released to an owned worker.
    Running,
    /// Successful result was observed.
    Succeeded,
    /// Read failed, or an external non-commit was verified.
    Failed,
    /// User declined or a proposal was invalidated.
    Declined,
    /// Execution was revoked or a read was cancelled.
    Cancelled,
    /// A dispatched commit has no reliable external outcome yet.
    Unknown,
}
/// Identity carried through contextual tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskOwner {
    /// Owning activation.
    pub task: TaskId,
    /// Owning input revision.
    pub revision: u64,
    /// Owning invocation.
    pub operation: OperationId,
}
/// Discoverable installed skill metadata.
#[derive(Debug, Clone, Serialize)]
pub struct SkillSummary {
    /// Installed name/version.
    pub key: SkillKey,
    /// Routing description.
    pub description: String,
    /// Input contract.
    pub input_schema: Value,
    /// Output contract.
    pub output_schema: Value,
    /// Skill-local tool declarations.
    pub tools: Vec<TaskTool>,
}
/// One activation's observable progress. Raw state is deliberately excluded.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSnapshot {
    /// Whether this task has running or queued service work.
    #[serde(default)]
    pub services_pending: bool,
    /// Recent owned service failures, without raw provider payloads.
    #[serde(default)]
    pub service_errors: Vec<String>,
    /// Activation ID.
    pub id: TaskId,
    /// Optional parent resumed after child completion.
    pub parent: Option<TaskId>,
    /// Pinned definition.
    pub skill: SkillKey,
    /// Current input revision.
    pub revision: u64,
    /// Activation lifecycle.
    pub status: TaskStatus,
    /// Current task-local flow observation.
    pub flow: Option<FlowSnapshot>,
    /// Current pending operation, including approval requests.
    pub pending: Option<OperationId>,
    /// Validated completion output.
    pub output: Option<Value>,
}
/// An operation receipt or immutable approval proposal.
#[derive(Debug, Clone, Serialize)]
pub struct OperationSnapshot {
    /// Stable invocation/approval identity.
    pub id: OperationId,
    /// Original owning activation and revision.
    pub owner: TaskOwner,
    /// Local tool name.
    pub tool: String,
    /// Exact admitted arguments; approval never substitutes them.
    pub args: Value,
    /// External operation key for commit tools.
    pub idempotency_key: Option<String>,
    /// Current operation status.
    pub status: OperationStatus,
    /// Cancellation requested after dispatch; not a claim that an effect was undone.
    pub cancellation_requested: bool,
    /// Successful result receipt, if known.
    pub result: Option<Value>,
    /// Failure or uncertainty description.
    pub error: Option<String>,
}
/// Complete observable task session. This is not a durable restore checkpoint.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSessionSnapshot {
    /// Currently speaking activation.
    pub foreground: Option<TaskId>,
    /// Installed catalog.
    pub skills: Vec<SkillSummary>,
    /// All activation observations.
    pub tasks: Vec<TaskSnapshot>,
    /// Pending proposals and retained operation outcomes.
    pub operations: Vec<OperationSnapshot>,
}
/// A tool completion's publication decision.
#[derive(Debug)]
pub struct TaskDelivery {
    /// Operation the host correlates to its protocol call.
    pub operation_id: OperationId,
    /// Original task owner.
    pub task_id: TaskId,
    /// Original input revision.
    pub revision: u64,
    /// Tool result for the host to format.
    pub result: Result<Value, String>,
    /// Valid completion for the foreground task.
    pub foreground: bool,
    /// Completion did not update the current activation revision.
    pub stale: bool,
}
/// Configuration, admission or lifecycle error.
#[derive(Debug, thiserror::Error)]
#[error("task runtime: {0}")]
pub struct TaskError(pub String);

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Validate identity shared by compiled catalogs and authoring documents.
pub fn validate_skill_identity(name: &str, version: &str) -> Result<(), TaskError> {
    if !valid_name(name) || version.trim().is_empty() {
        return Err(TaskError(
            "skill names must be ASCII identifiers without '__', and versions must be nonempty"
                .into(),
        ));
    }
    Ok(())
}
impl TaskTool {
    /// Validate the local name, qualified provider name and commit key policy.
    pub fn validate_for_skill(&self, skill: &str) -> Result<(), TaskError> {
        if !valid_name(&self.name) || skill.len() + 2 + self.name.len() > 64 {
            return Err(TaskError(format!(
                "tool '{}' must be an ASCII identifier without '__'; its qualified name must fit 64 characters",
                self.name
            )));
        }
        if let TaskToolEffect::Commit {
            idempotency_argument,
        } = &self.effect
            && idempotency_argument.trim().is_empty()
        {
            return Err(TaskError(
                "commit idempotency argument must be nonempty".into(),
            ));
        }
        Ok(())
    }
}
fn valid_name(name: &str) -> bool {
    !name.contains("__")
        && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
