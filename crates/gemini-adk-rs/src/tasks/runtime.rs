use super::services::*;
use super::types::*;
use crate::{
    flow::FlowStack,
    state::State,
    tool::{ToolContext, ToolDispatcher},
};
use futures_util::FutureExt;
use serde_json::Value;
use std::panic::AssertUnwindSafe;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

const MAX_TASKS: usize = 64;
const MAX_OPERATIONS: usize = 2048;
const QUEUED: u8 = 0;
const STARTED: u8 = 1;
const REVOKED: u8 = 2;

#[derive(Clone)]
struct Receipt {
    result: Value,
    writes: BTreeMap<String, Option<Value>>,
    already_applied: bool,
}

struct Task {
    id: TaskId,
    parent: Option<TaskId>,
    skill: Arc<CompiledSkill>,
    revision: u64,
    status: TaskStatus,
    state: State,
    flow: Option<FlowStack>,
    pending: Option<OperationId>,
    output: Option<Value>,
    deferred: Vec<DeferredEvent>,
    services: Option<ServiceState>,
}
enum DeferredEvent {
    Turn,
    Interrupted,
}
#[derive(Clone)]
struct ExecutionControl {
    cancel: CancellationToken,
    permit: Arc<AtomicU8>,
}

struct Operation {
    snapshot: OperationSnapshot,
    commit: bool,
    control: ExecutionControl,
    writes: BTreeMap<String, Option<Value>>,
    baseline: State,
    receipt_observed: bool,
}

/// The only executable ticket produced by task admission or trusted approval.
/// Its state and tool closures belong to this invocation, never another task.
pub struct OwnedInvocation {
    owner: TaskOwner,
    tool: String,
    args: Value,
    state: State,
    dispatcher: ToolDispatcher,
    control: ExecutionControl,
    cached: Option<Receipt>,
    scope: Arc<()>,
}
/// A worker result carrying the original owner and private state.
/// Fields are private so applications cannot manufacture accepted state writes.
#[derive(Clone)]
pub struct InvocationCompletion {
    owner: TaskOwner,
    state: State,
    result: Result<Value, String>,
    before: HashMap<String, Value>,
    cached_writes: Option<BTreeMap<String, Option<Value>>>,
    already_applied: bool,
    scope: Arc<()>,
}
impl OwnedInvocation {
    /// Invocation identity used by the host for protocol correlation.
    pub fn operation_id(&self) -> &OperationId {
        &self.owner.operation
    }
    /// Immutable task ownership, captured before execution.
    pub fn owner(&self) -> &TaskOwner {
        &self.owner
    }
    /// Execute without accessing session transport. Commits already passed approval.
    pub async fn execute(self) -> InvocationCompletion {
        let before = self.state.to_hashmap();
        let cached_writes = self.cached.as_ref().map(|receipt| receipt.writes.clone());
        let already_applied = self
            .cached
            .as_ref()
            .is_some_and(|receipt| receipt.already_applied);
        let result = if self
            .control
            .permit
            .compare_exchange(QUEUED, STARTED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            Err("operation cancelled before execution".into())
        } else if let Some(receipt) = self.cached {
            for (key, value) in receipt.writes.into_iter().filter(|_| !already_applied) {
                match value {
                    Some(value) => {
                        let _ = self.state.set(&key, value);
                    }
                    None => {
                        self.state.remove(&key);
                    }
                }
            }
            Ok(receipt.result)
        } else {
            let ctx = ToolContext::new(self.state.clone())
                .with_call_id(self.owner.operation.0.clone())
                .with_cancel(self.control.cancel)
                .with_task(self.owner.clone());
            match AssertUnwindSafe(self.dispatcher.call_function_in(&self.tool, self.args, ctx))
                .catch_unwind()
                .await
            {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(_) => Err("tool panicked before returning a receipt".into()),
            }
        };
        InvocationCompletion {
            owner: self.owner,
            state: self.state,
            result,
            before,
            cached_writes,
            already_applied,
            scope: self.scope,
        }
    }
}

/// Task lifecycle reducer. Drive it from one control lane or a deterministic replay.
/// At most one invocation is pending per activation; different tasks may execute
/// concurrently. All commands finish synchronously, including approval requests.
pub struct TaskRuntime {
    skills: BTreeMap<String, Arc<CompiledSkill>>,
    tasks: BTreeMap<TaskId, Task>,
    operations: BTreeMap<OperationId, Operation>,
    foreground: Option<TaskId>,
    next_task: u64,
    next_operation: u64,
    next_service: u64,
    ready_services: Vec<OwnedTaskServiceWork>,
    service_effects: Vec<AcceptedTaskEffect>,
    clock: crate::clock::SharedClock,
    scope: Arc<()>,
}
impl TaskRuntime {
    /// Install a finite catalog. Duplicate names/tool names are rejected.
    pub fn new(skills: Vec<CompiledSkill>) -> Result<Self, TaskError> {
        let mut catalog = BTreeMap::new();
        for skill in skills {
            validate_skill_identity(&skill.name, &skill.version)?;
            let mut names = BTreeSet::new();
            for tool in &skill.tools {
                tool.validate_for_skill(&skill.name)?;
                if !names.insert(&tool.name) {
                    return Err(err(format!(
                        "skill '{}' has duplicate tool names",
                        skill.name
                    )));
                }
            }
            let name = skill.name.clone();
            if catalog.insert(name.clone(), Arc::new(skill)).is_some() {
                return Err(err(format!("duplicate installed skill '{name}'")));
            }
        }
        Ok(Self {
            skills: catalog,
            tasks: BTreeMap::new(),
            operations: BTreeMap::new(),
            foreground: None,
            next_task: 1,
            next_operation: 1,
            next_service: 1,
            ready_services: Vec::new(),
            service_effects: Vec::new(),
            clock: Arc::new(crate::clock::SystemClock),
            scope: Arc::new(()),
        })
    }

    /// Use an injected clock for future task activations and deterministic replay.
    pub fn with_clock(mut self, clock: crate::clock::SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// Apply a command. An executable ticket is returned only after admission
    /// and, for a commit, an explicit trusted decision about its exact proposal.
    pub fn command(&mut self, command: TaskCommand) -> Result<Option<OwnedInvocation>, TaskError> {
        match command {
            TaskCommand::Start {
                skill,
                input,
                parent,
            } => self.start(&skill, input, parent)?,
            TaskCommand::Suspend { task } => {
                self.require_live(&task)?;
                self.suspend(&task);
            }
            TaskCommand::Resume { task } => {
                self.require_live(&task)?;
                self.focus(&task);
            }
            TaskCommand::Revise {
                task,
                expected_revision,
                input,
            } => self.revise(&task, expected_revision, input)?,
            TaskCommand::Cancel { task } => self.cancel(&task)?,
            TaskCommand::Complete { task, output } => self.finish_task(&task, output)?,
            TaskCommand::Invoke {
                task,
                tool,
                args,
                idempotency_key,
            } => return self.invoke(&task, &tool, args, idempotency_key),
            TaskCommand::Decide { operation, approve } => return self.decide(&operation, approve),
            TaskCommand::Reconcile { operation, outcome } => self.reconcile(&operation, outcome)?,
        }
        Ok(None)
    }

    fn start(&mut self, name: &str, input: Value, parent: Option<TaskId>) -> Result<(), TaskError> {
        if self.tasks.len() >= MAX_TASKS {
            return Err(err("session task limit reached"));
        }
        if let Some(parent) = &parent {
            self.require_live(parent)?;
        }
        let skill = self
            .skills
            .get(name)
            .cloned()
            .ok_or_else(|| err(format!("unknown skill '{name}'")))?;
        let (state, flow) = instantiate(&skill, input)?;
        state.set_clock(self.clock.clone());
        let services = ServiceState::create(&skill.services, &state).map_err(err)?;
        let id = TaskId(format!("task-{}", self.next_task));
        self.next_task += 1;
        self.tasks.insert(
            id.clone(),
            Task {
                id: id.clone(),
                parent,
                skill,
                revision: 1,
                status: TaskStatus::Suspended,
                state,
                flow,
                pending: None,
                output: None,
                deferred: Vec::new(),
                services,
            },
        );
        self.focus(&id);
        Ok(())
    }
    fn require_live(&self, id: &TaskId) -> Result<&Task, TaskError> {
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| err(format!("unknown task '{}'", id.0)))?;
        if !matches!(task.status, TaskStatus::Running | TaskStatus::Suspended) {
            return Err(err(format!("task '{}' is terminal", id.0)));
        }
        Ok(task)
    }
    fn suspend(&mut self, id: &TaskId) {
        self.invalidate_approval(id, "approval invalidated by suspension");
        if let Some(task) = self.tasks.get_mut(id) {
            if let Some(services) = &mut task.services {
                services.advance_clock(
                    &task.state,
                    task.status == TaskStatus::Running && services.pending.is_none(),
                );
            }
            task.status = TaskStatus::Suspended;
        }
        if self.foreground.as_ref() == Some(id) {
            self.foreground = None;
        }
    }
    fn focus(&mut self, id: &TaskId) {
        if self.foreground.as_ref() == Some(id) {
            return;
        }
        if let Some(previous) = self.foreground.clone() {
            self.suspend(&previous);
        }
        if let Some(task) = self.tasks.get_mut(id) {
            if let Some(services) = &mut task.services {
                services.advance_clock(&task.state, false);
                if services.plan.memory.is_some()
                    && !services
                        .queue
                        .iter()
                        .any(|event| matches!(event, ServiceEvent::Project))
                {
                    services.queue.push_back(ServiceEvent::Project);
                }
            }
            task.status = TaskStatus::Running;
        }
        self.foreground = Some(id.clone());
        self.drive_services_for(id);
    }
    fn invalidate_approval(&mut self, id: &TaskId, reason: &str) {
        let pending = self.tasks.get(id).and_then(|task| task.pending.clone());
        if let Some(pending) = pending
            && let Some(operation) = self.operations.get_mut(&pending)
            && operation.snapshot.status == OperationStatus::AwaitingApproval
        {
            operation.snapshot.status = OperationStatus::Declined;
            operation.snapshot.error = Some(reason.into());
            self.tasks.get_mut(id).expect("task owns proposal").pending = None;
        }
    }
    fn invalidate_work(&mut self, id: &TaskId) {
        if let Some(services) = self
            .tasks
            .get_mut(id)
            .and_then(|task| task.services.as_mut())
        {
            services.invalidate();
        }
        self.invalidate_approval(id, "approval invalidated by cancellation or revision");
        let pending = self.tasks.get_mut(id).and_then(|task| task.pending.take());
        if let Some(pending) = pending
            && let Some(operation) = self.operations.get_mut(&pending)
        {
            operation.snapshot.cancellation_requested = true;
            let revoked = operation
                .control
                .permit
                .compare_exchange(QUEUED, REVOKED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            if !operation.commit || revoked {
                operation.control.cancel.cancel();
                operation.snapshot.status = OperationStatus::Cancelled;
            }
        }
    }
    fn revise(&mut self, id: &TaskId, revision: u64, input: Value) -> Result<(), TaskError> {
        let task = self.require_live(id)?;
        if task.revision != revision {
            return Err(err("task revision changed"));
        }
        let (state, flow) = instantiate(&task.skill, input)?;
        state.set_clock(self.clock.clone());
        let services = ServiceState::create(&task.skill.services, &state).map_err(err)?;
        self.invalidate_work(id);
        let task = self.tasks.get_mut(id).expect("validated task");
        task.revision += 1;
        task.state = state;
        task.flow = flow;
        task.services = services;
        task.output = None;
        task.deferred.clear();
        self.drive_services_for(id);
        Ok(())
    }
    fn cancel(&mut self, id: &TaskId) -> Result<(), TaskError> {
        let task = self.tasks.get(id).ok_or_else(|| err("unknown task"))?;
        if task.status == TaskStatus::Cancelled {
            return Ok(());
        }
        self.require_live(id)?;
        self.invalidate_work(id);
        let task = self.tasks.get_mut(id).expect("validated task");
        task.revision += 1;
        task.status = TaskStatus::Cancelled;
        task.deferred.clear();
        self.resume_parent(id);
        Ok(())
    }
    fn finish_task(&mut self, id: &TaskId, output: Value) -> Result<(), TaskError> {
        let task = self.require_live(id)?;
        if task
            .services
            .as_ref()
            .is_some_and(|s| s.pending.is_some() || !s.queue.is_empty())
        {
            return Err(err("task has pending services"));
        }
        if task.pending.is_some() {
            return Err(err("task has a pending operation"));
        }
        if task.flow.as_ref().is_some_and(|flow| !flow.is_complete()) {
            return Err(err("task governance has not completed"));
        }
        if self.operations.values().any(|op| {
            op.snapshot.owner.task == *id
                && matches!(
                    op.snapshot.status,
                    OperationStatus::Running | OperationStatus::Unknown
                )
        }) {
            return Err(err("task has an unresolved external operation"));
        }
        let output = match &task.skill.project_output {
            Some(project) => project(&task.state).map_err(err)?,
            None => output,
        };
        (task.skill.validate_output)(&output).map_err(err)?;
        let task = self.tasks.get_mut(id).expect("validated task");
        task.output = Some(output);
        task.status = TaskStatus::Completed;
        self.resume_parent(id);
        Ok(())
    }
    fn resume_parent(&mut self, id: &TaskId) {
        if self.foreground.as_ref() != Some(id) {
            return;
        }
        self.foreground = None;
        let parent = self.tasks.get(id).and_then(|task| task.parent.clone());
        if let Some(parent) = parent
            && self
                .tasks
                .get(&parent)
                .is_some_and(|task| task.status == TaskStatus::Suspended)
        {
            self.focus(&parent);
        }
    }

    fn invoke(
        &mut self,
        id: &TaskId,
        name: &str,
        mut args: Value,
        key: Option<String>,
    ) -> Result<Option<OwnedInvocation>, TaskError> {
        if self.operations.len() >= MAX_OPERATIONS {
            return Err(err("session operation limit reached"));
        }
        let task = self.require_live(id)?;
        if self.foreground.as_ref() != Some(id) {
            return Err(err("only the foreground task admits new operations"));
        }
        if task
            .services
            .as_ref()
            .is_some_and(|s| s.pending.is_some() || !s.queue.is_empty())
        {
            return Err(err("task has pending services"));
        }
        if task.pending.is_some() {
            return Err(err("task already has a pending operation"));
        }
        let tool = task
            .skill
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .ok_or_else(|| err(format!("unknown task tool '{name}'")))?;
        let mut cached = None;
        let key = match &tool.effect {
            TaskToolEffect::Read => {
                if key.is_some() {
                    return Err(err("idempotency key is only valid for commit tools"));
                }
                None
            }
            TaskToolEffect::Commit {
                idempotency_argument,
            } => {
                if self.operations.values().any(|operation| {
                    operation.commit
                        && operation.snapshot.owner.task == *id
                        && matches!(
                            operation.snapshot.status,
                            OperationStatus::Running | OperationStatus::Unknown
                        )
                }) {
                    return Err(err(
                        "task has an unresolved commit; await or reconcile its outcome before another commit",
                    ));
                }
                let object = args
                    .as_object_mut()
                    .ok_or_else(|| err("commit arguments must be an object"))?;
                let argument_key = object
                    .get(idempotency_argument)
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let key = match (key, argument_key) {
                    (Some(key), Some(arg)) if key != arg => {
                        return Err(err("idempotency key disagrees with the tool argument"));
                    }
                    (Some(key), _) | (None, Some(key)) if !key.trim().is_empty() => key,
                    _ => return Err(err("commit requires a nonempty idempotency key")),
                };
                object.insert(idempotency_argument.clone(), Value::String(key.clone()));
                for previous in self.operations.values() {
                    let owner_skill = &self.tasks[&previous.snapshot.owner.task].skill.name;
                    if owner_skill == &task.skill.name
                        && previous.snapshot.tool == name
                        && previous.snapshot.idempotency_key.as_ref() == Some(&key)
                    {
                        if previous.snapshot.args != args {
                            return Err(err(
                                "idempotency key was already used with different arguments",
                            ));
                        }
                        match previous.snapshot.status {
                            OperationStatus::Succeeded => {
                                let already_applied = cached
                                    .as_ref()
                                    .is_some_and(|receipt: &Receipt| receipt.already_applied)
                                    || (previous.snapshot.owner.task == *id
                                        && previous.snapshot.owner.revision == task.revision);
                                cached = previous.snapshot.result.clone().map(|result| Receipt {
                                    result,
                                    writes: previous.writes.clone(),
                                    already_applied,
                                });
                            }
                            OperationStatus::AwaitingApproval
                            | OperationStatus::Running
                            | OperationStatus::Unknown => {
                                return Err(err(
                                    "operation key has a pending or unknown outcome; reconcile before retrying",
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                Some(key)
            }
        };
        if cached.is_none()
            && let Some(flow) = &task.flow
        {
            flow.admits_tool(name, &task.state).map_err(err)?;
        }
        let operation_id = OperationId(format!("operation-{}", self.next_operation));
        let owner = TaskOwner {
            task: id.clone(),
            revision: task.revision,
            operation: operation_id.clone(),
        };
        let commit = matches!(tool.effect, TaskToolEffect::Commit { .. });
        let approval = commit && cached.is_none();
        let control = ExecutionControl {
            cancel: CancellationToken::new(),
            permit: Arc::new(AtomicU8::new(QUEUED)),
        };
        let invocation = if approval {
            None
        } else {
            Some(make_invocation(
                task,
                owner.clone(),
                name,
                args.clone(),
                control.clone(),
                cached,
                self.scope.clone(),
            )?)
        };
        let baseline = independent_state(&task.state);
        self.next_operation += 1;
        self.operations.insert(
            operation_id.clone(),
            Operation {
                snapshot: OperationSnapshot {
                    id: operation_id.clone(),
                    owner,
                    tool: name.into(),
                    args,
                    idempotency_key: key,
                    status: if approval {
                        OperationStatus::AwaitingApproval
                    } else {
                        OperationStatus::Running
                    },
                    cancellation_requested: false,
                    result: None,
                    error: None,
                },
                commit,
                control,
                writes: BTreeMap::new(),
                baseline,
                receipt_observed: false,
            },
        );
        self.tasks.get_mut(id).expect("validated task").pending = Some(operation_id);
        Ok(invocation)
    }
    fn decide(
        &mut self,
        id: &OperationId,
        approve: bool,
    ) -> Result<Option<OwnedInvocation>, TaskError> {
        let operation = self
            .operations
            .get(id)
            .ok_or_else(|| err("unknown approval operation"))?;
        if operation.snapshot.status != OperationStatus::AwaitingApproval {
            return Err(err("approval is no longer pending"));
        }
        let owner = operation.snapshot.owner.clone();
        let task = self.require_live(&owner.task)?;
        if task.revision != owner.revision
            || task.pending.as_ref() != Some(id)
            || self.foreground.as_ref() != Some(&owner.task)
        {
            return Err(err("approval owner is no longer current"));
        }
        if !approve {
            let operation = self.operations.get_mut(id).expect("validated proposal");
            operation.snapshot.status = OperationStatus::Declined;
            operation.snapshot.error = Some("user declined the operation".into());
            self.tasks
                .get_mut(&owner.task)
                .expect("validated task")
                .pending = None;
            return Ok(None);
        }
        if task
            .services
            .as_ref()
            .is_some_and(|s| s.pending.is_some() || !s.queue.is_empty())
        {
            return Err(err("task has pending services before approval"));
        }
        if let Some(flow) = &task.flow {
            flow.admits_tool(&operation.snapshot.tool, &task.state)
                .map_err(err)?;
        }
        let invocation = make_invocation(
            task,
            owner,
            &operation.snapshot.tool,
            operation.snapshot.args.clone(),
            operation.control.clone(),
            None,
            self.scope.clone(),
        )?;
        let operation = self.operations.get_mut(id).expect("validated proposal");
        operation.snapshot.status = OperationStatus::Running;
        operation.baseline = independent_state(&invocation.state);
        Ok(Some(invocation))
    }

    /// Commit the result to its captured owner, never whichever task is now foreground.
    /// External commit receipts survive cancellation/revision even when state is stale.
    pub fn complete(&mut self, completion: InvocationCompletion) -> TaskDelivery {
        let InvocationCompletion {
            owner,
            state,
            result,
            before,
            cached_writes,
            already_applied,
            scope,
        } = completion;
        let mut delivery = TaskDelivery {
            operation_id: owner.operation.clone(),
            task_id: owner.task.clone(),
            revision: owner.revision,
            result: result.clone(),
            foreground: false,
            stale: true,
        };
        if !Arc::ptr_eq(&self.scope, &scope) {
            return delivery;
        }
        let Some(operation) = self.operations.get_mut(&owner.operation) else {
            return delivery;
        };
        if operation.receipt_observed
            || operation.snapshot.owner != owner
            || !matches!(
                operation.snapshot.status,
                OperationStatus::Running | OperationStatus::Unknown
            )
        {
            return delivery;
        }
        operation.receipt_observed = true;
        if result.is_ok() {
            operation.writes =
                cached_writes.unwrap_or_else(|| state_diff(&before, &state.to_hashmap()));
        }
        match &result {
            Ok(value) => {
                operation.snapshot.status = OperationStatus::Succeeded;
                operation.snapshot.result = Some(value.clone());
                operation.snapshot.error = None;
            }
            Err(error) => {
                operation.snapshot.status = if operation.commit {
                    OperationStatus::Unknown
                } else {
                    OperationStatus::Failed
                };
                operation.snapshot.error = Some(error.clone());
            }
        }
        let Some(task) = self.tasks.get_mut(&owner.task) else {
            return delivery;
        };
        if task.revision != owner.revision
            || task.pending.as_ref() != Some(&owner.operation)
            || !matches!(task.status, TaskStatus::Running | TaskStatus::Suspended)
        {
            return delivery;
        }
        let unknown_commit = operation.commit && result.is_err();
        if !unknown_commit {
            task.pending = None;
        }
        let service_before = task.state.to_hashmap();
        if result.is_ok() {
            task.state = independent_state(&state);
        }
        let service_owner = TaskOwner {
            task: owner.task.clone(),
            revision: owner.revision,
            operation: OperationId(format!("service-{}", self.next_service)),
        };
        self.next_service += 1;
        self.service_effects
            .extend(settle_task_services(task, service_before, None, service_owner).0);
        if let Some(flow) = &mut task.flow {
            if result.is_ok() && !already_applied {
                let _ = task.state.set(crate::flow::TOOL_RESULT_KEY, serde_json::json!({"tool": operation.snapshot.tool, "id": owner.operation.0, "ok": true}));
                flow.observe_tool(&operation.snapshot.tool, true, &task.state);
            } else if result.is_err() && !operation.commit {
                flow.observe_tool(&operation.snapshot.tool, false, &task.state);
            }
        }
        if !unknown_commit {
            drain_turns(task);
        }
        if let Some(services) = &mut task.services {
            if result.is_ok()
                && services.plan.memory.is_some()
                && matches!(
                    operation.snapshot.tool.as_str(),
                    "recall_context" | "manage_memory"
                )
            {
                services.queue.push_back(ServiceEvent::Project);
            }
            services.queue.push_back(ServiceEvent::AfterTool {
                tool: operation.snapshot.tool.clone(),
                args: operation.snapshot.args.clone(),
                result: result
                    .clone()
                    .unwrap_or_else(|error| serde_json::json!({"error":error})),
            });
        }
        delivery.foreground = self.foreground.as_ref() == Some(&owner.task);
        delivery.stale = false;
        self.drive_services_for(&owner.task);
        delivery
    }
    fn reconcile(&mut self, id: &OperationId, outcome: ReconciledOutcome) -> Result<(), TaskError> {
        let operation = self
            .operations
            .get(id)
            .ok_or_else(|| err("unknown operation"))?;
        if operation.snapshot.status != OperationStatus::Unknown {
            return Err(err("only an unknown outcome can be reconciled"));
        }
        let owner = operation.snapshot.owner.clone();
        let tool = operation.snapshot.tool.clone();
        let task = self
            .tasks
            .get(&owner.task)
            .expect("operation retains its owner");
        let current = task.revision == owner.revision
            && task.pending.as_ref() == Some(id)
            && matches!(task.status, TaskStatus::Running | TaskStatus::Suspended);
        if current
            && task
                .services
                .as_ref()
                .is_some_and(|services| services.pending.is_some() || !services.queue.is_empty())
        {
            return Err(err("task has pending services before reconciliation"));
        }
        let state = independent_state(if current {
            &task.state
        } else {
            &operation.baseline
        });
        let before = state.to_hashmap();
        let application = match &outcome {
            ReconciledOutcome::Succeeded { result } => task
                .skill
                .receipt_applier
                .as_ref()
                .map_or(Ok(()), |apply| apply(&tool, result, &state)),
            ReconciledOutcome::Failed { .. } => Ok(()),
        };
        let succeeded = matches!(outcome, ReconciledOutcome::Succeeded { .. });
        let operation = self.operations.get_mut(id).expect("validated operation");
        match outcome {
            ReconciledOutcome::Succeeded { result } => {
                operation.snapshot.status = OperationStatus::Succeeded;
                operation.snapshot.result = Some(result);
                operation.snapshot.error = application.as_ref().err().map(|error| {
                    format!("external success verified; state reconstruction failed: {error}")
                });
                if application.is_ok() {
                    operation.writes = state_diff(&before, &state.to_hashmap());
                }
            }
            ReconciledOutcome::Failed { error } => {
                operation.snapshot.status = OperationStatus::Failed;
                operation.snapshot.error = Some(error);
            }
        }
        if current {
            let task = self.tasks.get_mut(&owner.task).expect("validated task");
            task.pending = None;
            if application.is_ok() {
                let before = task.state.to_hashmap();
                if succeeded {
                    task.state = state;
                }
                let service_owner = TaskOwner {
                    task: owner.task.clone(),
                    revision: owner.revision,
                    operation: OperationId(format!("service-{}", self.next_service)),
                };
                self.next_service += 1;
                self.service_effects
                    .extend(settle_task_services(task, before, None, service_owner).0);
                if let Some(flow) = &mut task.flow {
                    flow.observe_tool(&tool, succeeded, &task.state);
                }
                drain_turns(task);
            } else {
                task.status = TaskStatus::Failed;
                task.deferred.clear();
            }
        }
        if current && application.is_err() {
            self.resume_parent(&owner.task);
        }
        self.drive_services_for(&owner.task);
        application.map_err(|error| {
            err(format!(
                "external outcome recorded, but task state reconstruction failed: {error}"
            ))
        })
    }

    /// Observe the pinned catalog without exposing factories or raw state.
    pub fn catalog(&self) -> Vec<SkillSummary> {
        self.skills
            .values()
            .map(|skill| SkillSummary {
                key: SkillKey {
                    name: skill.name.clone(),
                    version: skill.version.clone(),
                },
                description: skill.description.clone(),
                input_schema: skill.input_schema.clone(),
                output_schema: skill.output_schema.clone(),
                tools: skill.tools.clone(),
            })
            .collect()
    }
    /// Observe all activation and operation lifecycles. Not a restore checkpoint.
    pub fn snapshot(&self) -> TaskSessionSnapshot {
        TaskSessionSnapshot {
            foreground: self.foreground.clone(),
            skills: self.catalog(),
            tasks: self
                .tasks
                .values()
                .map(|task| TaskSnapshot {
                    services_pending: task
                        .services
                        .as_ref()
                        .is_some_and(|s| s.pending.is_some() || !s.queue.is_empty()),
                    service_errors: task
                        .services
                        .as_ref()
                        .map(|s| s.errors.clone())
                        .unwrap_or_default(),
                    id: task.id.clone(),
                    parent: task.parent.clone(),
                    skill: SkillKey {
                        name: task.skill.name.clone(),
                        version: task.skill.version.clone(),
                    },
                    revision: task.revision,
                    status: task.status,
                    flow: task.flow.as_ref().map(|flow| flow.snapshot(&task.state)),
                    pending: task.pending.clone(),
                    output: task.output.clone(),
                })
                .collect(),
            operations: self
                .operations
                .values()
                .map(|operation| operation.snapshot.clone())
                .collect(),
        }
    }
    /// Foreground skill instructions and current governed posture/grounding:
    /// [`foreground_brief`](Self::foreground_brief) followed by
    /// [`foreground_context`](Self::foreground_context).
    pub fn foreground_instruction(&self) -> Option<String> {
        let brief = self.foreground_brief()?;
        Some(match self.foreground_context() {
            Some(context) if !context.is_empty() => format!("{brief}\n{context}"),
            _ => brief,
        })
    }

    /// The part of the foreground instruction that changes only when the
    /// foreground task or its revision does: the task's identity and its
    /// skill's instruction. Under `contextUpdate` it rides the system
    /// instruction, which an update replaces, rather than a context turn,
    /// which stays in the conversation.
    pub fn foreground_brief(&self) -> Option<String> {
        let task = self.tasks.get(self.foreground.as_ref()?)?;
        Some(format!(
            "Task {} / skill {}@{} / revision {}\n{}",
            task.id.0, task.skill.name, task.skill.version, task.revision, task.skill.instruction
        ))
    }

    /// The foreground task's skill and the tools it offers the model now:
    /// those its flow would admit once their `never(..).until(..)` guards
    /// hold, as [`FlowStack::offers_tool`] decides, or every tool of a skill
    /// without a flow. `None` with no foreground task.
    pub fn foreground_offer(&self) -> Option<(String, Vec<String>)> {
        let task = self.tasks.get(self.foreground.as_ref()?)?;
        let tools = task
            .skill
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .filter(|name| {
                task.flow
                    .as_ref()
                    .is_none_or(|flow| flow.offers_tool(name, &task.state))
            })
            .collect();
        Some((task.skill.name.clone(), tools))
    }

    /// The part of the foreground instruction that changes as the task runs:
    /// service contexts, extracted, computed and remembered values, active
    /// postures and grounds, input and results. Empty when there is none.
    pub fn foreground_context(&self) -> Option<String> {
        let task = self.tasks.get(self.foreground.as_ref()?)?;
        let refreshing = task
            .services
            .as_ref()
            .is_some_and(ServiceState::refreshing_memory);
        let visible = if refreshing {
            let state = independent_state(&task.state);
            if let Some(services) = &task.services {
                for (key, previous) in &services.memory_values {
                    if state.get_raw(key).as_ref() == Some(previous) {
                        state.remove(key);
                    }
                }
                for computed in services.plan.computed.describe() {
                    state.remove(&format!("derived:{}", computed.key));
                }
                services.plan.computed.recompute(&state);
            }
            Some(state)
        } else {
            None
        };
        let state = visible.as_ref().unwrap_or(&task.state);
        let mut parts = Vec::new();
        if let Some(services) = &task.services {
            if refreshing {
                parts.push(
                    "Remembered context is refreshing; wait for task services before acting."
                        .into(),
                );
            } else {
                parts.extend(services.contexts.clone());
            }
            let keys = services
                .plan
                .extractors
                .iter()
                .flat_map(|extractor| {
                    if extractor.promotion_rules().is_empty() {
                        state
                            .get_raw(extractor.name())
                            .and_then(|value| {
                                value
                                    .as_object()
                                    .map(|fields| fields.keys().cloned().collect::<Vec<_>>())
                            })
                            .unwrap_or_default()
                    } else {
                        extractor
                            .promotion_rules()
                            .iter()
                            .map(|rule| rule.state_key.clone())
                            .collect::<Vec<_>>()
                    }
                })
                .chain(services.memory_keys.iter().cloned())
                .chain(
                    services
                        .plan
                        .computed
                        .describe()
                        .into_iter()
                        .map(|c| format!("derived:{}", c.key)),
                );
            let values: BTreeMap<_, _> = keys
                .filter_map(|key| {
                    state
                        .get_raw(&key)
                        .map(|value| (key.clone(), state.redact_value(&key, &value)))
                })
                .collect();
            if !values.is_empty() {
                parts.push(format!(
                    "This task's authoritative extracted, computed and remembered values: {}",
                    serde_json::json!(values)
                ));
            }
        }
        if let Some(flow) = &task.flow {
            parts.extend(flow.active_postures(state));
            parts.extend(flow.active_grounds(state));
        }
        if let Some(input) = state.get_raw("task:input") {
            parts.push(format!("Task input data: {}", state.redact_fields(&input)));
        }
        let results: Vec<_> = self
            .operations
            .values()
            .filter(|operation| {
                operation.snapshot.owner.task == task.id
                    && operation.snapshot.owner.revision == task.revision
                    && operation.snapshot.status == OperationStatus::Succeeded
                    && !matches!(
                        operation.snapshot.tool.as_str(),
                        "recall_context" | "manage_memory"
                    )
            })
            .filter_map(|operation| {
                operation.snapshot.result.as_ref().map(|result| {
                    serde_json::json!({
                        "operation": operation.snapshot.id,
                        "tool": operation.snapshot.tool,
                        "result": state.redact_fields(result),
                    })
                })
            })
            .collect();
        if !results.is_empty() {
            parts.push(format!(
                "This task's completed operation data: {}",
                Value::Array(results)
            ));
        }
        Some(parts.join("\n"))
    }
    /// Advance current foreground governance. Hosts with transcript ownership
    /// should prefer [`on_turn_for`](Self::on_turn_for).
    pub fn on_turn(&mut self) {
        if let Some(id) = self.foreground.clone() {
            let revision = self.tasks[&id].revision;
            self.on_turn_for(&id, revision);
        }
    }
    /// Advance a turn belonging to the captured task/revision, even if another
    /// task is now speaking. Stale and terminal owners are ignored. While work
    /// is unresolved, events queue so its snapshot cannot overwrite repair writes.
    pub fn on_turn_for(&mut self, id: &TaskId, revision: u64) {
        if self
            .tasks
            .get(id)
            .is_some_and(|task| task.services.is_some())
        {
            let _ = self.observe(
                id,
                revision,
                TaskObservation::Turn {
                    user: String::new(),
                    model: String::new(),
                },
            );
            return;
        }
        let Some(task) = self.tasks.get_mut(id) else {
            return;
        };
        if task.revision != revision
            || !matches!(task.status, TaskStatus::Running | TaskStatus::Suspended)
        {
            return;
        }
        if is_executing(task, &self.operations) {
            task.deferred.push(DeferredEvent::Turn);
        } else if let Some(flow) = &mut task.flow {
            flow.on_turn(&task.state);
        }
    }
    /// Attribute an interruption to the current foreground task.
    pub fn on_interrupted(&mut self) {
        if let Some(id) = self.foreground.clone() {
            let revision = self.tasks[&id].revision;
            self.on_interrupted_for(&id, revision);
        }
    }
    /// Attribute an interruption to its captured owner without cancelling
    /// unrelated tasks or claiming an external commit has been undone.
    pub fn on_interrupted_for(&mut self, id: &TaskId, revision: u64) {
        if self
            .tasks
            .get(id)
            .is_some_and(|task| task.services.is_some())
        {
            let _ = self.observe(id, revision, TaskObservation::Interrupted);
            return;
        }
        let Some(task) = self.tasks.get_mut(id) else {
            return;
        };
        if task.revision != revision
            || !matches!(task.status, TaskStatus::Running | TaskStatus::Suspended)
        {
            return;
        }
        if is_executing(task, &self.operations) {
            task.deferred.push(DeferredEvent::Interrupted);
        } else if let Some(flow) = &mut task.flow {
            flow.on_interrupted(&task.state);
        }
    }
    /// Whether the foreground task has a detector requiring timer observations.
    pub fn needs_service_timer(&self) -> bool {
        self.foreground
            .as_ref()
            .and_then(|id| self.tasks.get(id))
            .and_then(|task| task.services.as_ref())
            .is_some_and(|services| {
                services
                    .plan
                    .patterns
                    .iter()
                    .any(|p| p.detector.needs_timer())
            })
    }
    /// Enqueue an observation captured for this task revision, never the current speaker by inference.
    pub fn observe(
        &mut self,
        id: &TaskId,
        revision: u64,
        event: TaskObservation,
    ) -> Result<(), TaskError> {
        let task = self.require_live(id)?;
        if task.revision != revision {
            return Err(err("task observation revision changed"));
        }
        if task.services.is_none() {
            match event {
                TaskObservation::Turn { .. } => self.on_turn_for(id, revision),
                TaskObservation::Interrupted => self.on_interrupted_for(id, revision),
                _ => {}
            }
            return Ok(());
        }
        let task = self.tasks.get_mut(id).expect("validated task");
        let services = task.services.as_mut().expect("services checked");
        if services.queue.len() >= 128 {
            return Err(err("task service observation queue is full"));
        }
        services.advance_clock(
            &task.state,
            task.status == TaskStatus::Running
                && services.pending.is_none()
                && task.pending.is_none(),
        );
        services.queue.push_back(ServiceEvent::Observe(event));
        self.drive_services_for(id);
        Ok(())
    }
    /// Take runnable service tickets prepared by the same reducer as tool invocations.
    pub fn take_ready_services(&mut self) -> Vec<OwnedTaskServiceWork> {
        std::mem::take(&mut self.ready_services)
    }
    /// Take accepted owner-labelled effects. Speaking delivery must recheck foreground ownership.
    pub fn take_service_effects(&mut self) -> Vec<AcceptedTaskEffect> {
        std::mem::take(&mut self.service_effects)
    }
    /// Accept a service result only once and only in its original live task revision.
    pub fn complete_services(&mut self, completion: TaskServiceCompletion) -> bool {
        let TaskServiceCompletion {
            owner,
            scope,
            result,
        } = completion;
        if !Arc::ptr_eq(&scope, &self.scope) {
            return false;
        }
        let Some(task) = self.tasks.get_mut(&owner.task) else {
            return false;
        };
        if task.revision != owner.revision
            || !matches!(task.status, TaskStatus::Running | TaskStatus::Suspended)
        {
            return false;
        }
        let Some(services) = &mut task.services else {
            return false;
        };
        if !services
            .pending
            .as_ref()
            .is_some_and(|pending| pending.owner == owner)
        {
            return false;
        }
        let pending = services.pending.take().expect("matched service ticket");
        services.advance_clock(&task.state, false);
        let mut errors = Vec::new();
        match result {
            ServiceResult::Extractions(results) => {
                for (extractor, result) in results {
                    match result {
                        Ok(value) => {
                            if extractor.promotion_rules().is_empty()
                                && value.as_object().is_some_and(|fields| {
                                    fields.keys().any(|key| reserved_service_key(key))
                                })
                            {
                                errors.push(format!(
                                    "extraction '{}' returned a reserved runtime field",
                                    extractor.name()
                                ));
                                continue;
                            }
                            let _ = task.state.set(extractor.name(), value.clone());
                            crate::live::extractor::promote_fields(
                                extractor.as_ref(),
                                extractor.name(),
                                &value,
                                &task.state,
                            );
                        }
                        Err(error) => {
                            errors.push(format!("extraction '{}': {error}", extractor.name()));
                        }
                    }
                }
            }
            ServiceResult::Memory(Ok(Value::Object(slots))) => {
                // A slot remains memory-owned only while no accepted local change has replaced it.
                services
                    .memory_values
                    .retain(|key, previous| task.state.get_raw(key).as_ref() == Some(previous));
                let removed: Vec<_> = services
                    .memory_values
                    .keys()
                    .filter(|key| slots.get(*key).is_none_or(Value::is_null))
                    .cloned()
                    .collect();
                for key in removed {
                    task.state.remove(&key);
                    services.memory_values.remove(&key);
                }
                for (key, value) in slots {
                    if reserved_service_key(&key) {
                        errors.push(format!("memory projection uses reserved field '{key}'"));
                    } else if !value.is_null() {
                        services.memory_keys.insert(key.clone());
                        if !task.state.contains(&key) || services.memory_values.contains_key(&key) {
                            match task.state.set(&key, value.clone()) {
                                Ok(()) => {
                                    services.memory_values.insert(key, value);
                                }
                                Err(error) => errors.push(error.to_string()),
                            }
                        }
                    }
                }
            }
            ServiceResult::Memory(Ok(_)) => {
                errors.push("task memory projection must be an object".into());
            }
            ServiceResult::Memory(Err(error)) | ServiceResult::Failed(error) => errors.push(error),
        }
        let (mut effects, changed) =
            settle_task_services(task, pending.before, Some(&pending.event), owner.clone());
        for error in errors {
            effects.extend(record_service_effect(
                task,
                owner.clone(),
                TaskEffect::Error(error),
            ));
        }
        self.service_effects.extend(effects);
        if changed {
            self.invalidate_approval(
                &owner.task,
                "approval invalidated by accepted service state",
            );
        }
        self.drive_services_for(&owner.task);
        true
    }
    fn drive_services_for(&mut self, id: &TaskId) {
        loop {
            let Some(task) = self.tasks.get(id) else {
                return;
            };
            if !matches!(task.status, TaskStatus::Running | TaskStatus::Suspended)
                || is_executing(task, &self.operations)
            {
                return;
            }
            let Some(services) = &task.services else {
                return;
            };
            if services.pending.is_some() || services.queue.is_empty() {
                return;
            }
            let task = self.tasks.get_mut(id).expect("task checked");
            let services = task.services.as_mut().expect("services checked");
            let event = services.queue.pop_front().expect("queue checked");
            let owner = TaskOwner {
                task: id.clone(),
                revision: task.revision,
                operation: OperationId(format!("service-{}", self.next_service)),
            };
            self.next_service += 1;
            let before = task.state.to_hashmap();
            if let Some(work) = services.prepare(
                &event,
                owner.clone(),
                independent_state(&task.state),
                self.scope.clone(),
            ) {
                self.ready_services.push(work);
                return;
            }
            let (effects, changed) = settle_task_services(task, before, Some(&event), owner);
            self.service_effects.extend(effects);
            if changed {
                self.invalidate_approval(id, "approval invalidated by accepted service state");
            }
        }
    }

    /// Cancel workers on session shutdown. Dispatched commit outcomes become
    /// unknown until an actual receipt or trusted reconciliation resolves them.
    pub fn shutdown(&mut self) {
        let ids: Vec<_> = self.tasks.keys().cloned().collect();
        for id in ids {
            if self.require_live(&id).is_ok() {
                let _ = self.cancel(&id);
            }
        }
        self.foreground = None;
        for operation in self.operations.values_mut() {
            if operation.snapshot.status == OperationStatus::Running {
                operation.control.cancel.cancel();
                operation.snapshot.cancellation_requested = true;
                operation.snapshot.status = if operation.commit {
                    OperationStatus::Unknown
                } else {
                    OperationStatus::Cancelled
                };
                operation.snapshot.error =
                    Some("session ended before an operation receipt was accepted".into());
            }
        }
    }
}

fn settle_task_services(
    task: &mut Task,
    before: HashMap<String, Value>,
    event: Option<&ServiceEvent>,
    owner: TaskOwner,
) -> (Vec<AcceptedTaskEffect>, bool) {
    let Some(services) = &mut task.services else {
        return (Vec::new(), false);
    };
    services
        .memory_values
        .retain(|key, previous| task.state.get_raw(key).as_ref() == Some(previous));
    let effects = services.stabilize(&task.state, before.clone(), event);
    services
        .memory_values
        .retain(|key, previous| task.state.get_raw(key).as_ref() == Some(previous));
    let after = task.state.to_hashmap();
    let changed = before
        .keys()
        .chain(after.keys())
        .any(|key| !key.starts_with("state_meta:") && before.get(key) != after.get(key));
    if let Some(ServiceEvent::Observe(TaskObservation::Turn { user, .. })) = event
        && !user.is_empty()
        && services.plan.memory.is_some()
    {
        services
            .queue
            .push_back(ServiceEvent::Ingest { user: user.clone() });
    }
    if let Some(flow) = &mut task.flow {
        match event {
            Some(ServiceEvent::Observe(TaskObservation::Turn { .. })) => flow.on_turn(&task.state),
            Some(ServiceEvent::Observe(TaskObservation::Interrupted)) => {
                flow.on_interrupted(&task.state);
            }
            _ => flow.relatch(&task.state),
        }
    }
    (
        effects
            .into_iter()
            .flat_map(|effect| record_service_effect(task, owner.clone(), effect))
            .collect(),
        changed,
    )
}
fn record_service_effect(
    task: &mut Task,
    owner: TaskOwner,
    effect: TaskEffect,
) -> Vec<AcceptedTaskEffect> {
    let services = task.services.as_mut().expect("task has services");
    match &effect {
        TaskEffect::Remember(note) => {
            if services.plan.memory.is_some() {
                services
                    .queue
                    .push_back(ServiceEvent::Remember { note: note.clone() });
            } else {
                return record_service_effect(
                    task,
                    owner,
                    TaskEffect::Error("remember effect requires task memory".into()),
                );
            }
            Vec::new()
        }
        TaskEffect::Context(text) => {
            services.contexts.push(text.clone());
            if services.contexts.len() > 32 {
                services.contexts.remove(0);
            }
            vec![AcceptedTaskEffect { owner, effect }]
        }
        TaskEffect::Error(error) => {
            services.errors.push(error.clone());
            if services.errors.len() > 16 {
                services.errors.remove(0);
            }
            vec![AcceptedTaskEffect { owner, effect }]
        }
        _ => vec![AcceptedTaskEffect { owner, effect }],
    }
}

fn err(message: impl Into<String>) -> TaskError {
    TaskError(message.into())
}
fn independent_state(source: &State) -> State {
    let state = State::new();
    state.from_hashmap(source.to_hashmap());
    state.redact_keys(source.redacted_keys());
    state.set_clock(source.clock());
    state
}
fn instantiate(
    skill: &CompiledSkill,
    input: Value,
) -> Result<(State, Option<FlowStack>), TaskError> {
    (skill.validate_input)(&input).map_err(err)?;
    let state = State::new();
    state.from_hashmap(skill.initial_state.clone());
    if let Value::Object(fields) = &input {
        for (key, value) in fields {
            state
                .set(key, value.clone())
                .map_err(|error| err(error.to_string()))?;
        }
    }
    state
        .set("task:input", input)
        .map_err(|error| err(error.to_string()))?;
    let mut flow = skill
        .flow_factory
        .as_ref()
        .map(|factory| factory())
        .transpose()
        .map_err(err)?;
    if let Some(flow) = &mut flow {
        flow.relatch(&state);
    }
    Ok((state, flow))
}
fn make_invocation(
    task: &Task,
    owner: TaskOwner,
    tool: &str,
    args: Value,
    control: ExecutionControl,
    cached: Option<Receipt>,
    scope: Arc<()>,
) -> Result<OwnedInvocation, TaskError> {
    let state = independent_state(&task.state);
    let dispatcher = (task.skill.tool_factory)(state.clone()).map_err(err)?;
    if dispatcher.has_confirmation_provider() {
        return Err(err(
            "task factories cannot install a blocking confirmation provider; use task Decide",
        ));
    }
    if !dispatcher.names().any(|name| name == tool) {
        return Err(err(format!("task factory did not bind tool '{tool}'")));
    }
    if dispatcher.gated_tools().any(|name| name == tool)
        && task.skill.tools.iter().any(|declared| {
            declared.name == tool && matches!(declared.effect, TaskToolEffect::Read)
        })
    {
        return Err(err(
            "confirmation-gated tools must declare a commit policy in tasks",
        ));
    }
    Ok(OwnedInvocation {
        owner,
        tool: tool.into(),
        args,
        state,
        dispatcher,
        control,
        cached,
        scope,
    })
}
fn is_executing(task: &Task, operations: &BTreeMap<OperationId, Operation>) -> bool {
    task.pending
        .as_ref()
        .and_then(|id| operations.get(id))
        .is_some_and(|operation| {
            operation.snapshot.status == OperationStatus::Running
                || (operation.snapshot.status == OperationStatus::Unknown
                    && !operation.receipt_observed)
        })
}
fn drain_turns(task: &mut Task) {
    for event in std::mem::take(&mut task.deferred) {
        if let Some(flow) = &mut task.flow {
            match event {
                DeferredEvent::Turn => flow.on_turn(&task.state),
                DeferredEvent::Interrupted => flow.on_interrupted(&task.state),
            }
        }
    }
}
fn state_diff(
    before: &HashMap<String, Value>,
    after: &HashMap<String, Value>,
) -> BTreeMap<String, Option<Value>> {
    before
        .keys()
        .chain(after.keys())
        .filter(|key| before.get(*key) != after.get(*key))
        .map(|key| (key.clone(), after.get(key).cloned()))
        .collect()
}

impl Drop for TaskRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}
