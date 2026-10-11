//! Live adapter for the task reducer. Workers cannot reach the session writer.

use std::{collections::HashMap, sync::Arc};

use gemini_genai_rs::prelude::{
    Content, FunctionCall, FunctionResponse, FunctionResponseScheduling,
};
use gemini_genai_rs::session::SessionWriter;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::live::tool_scope::{DECLARED_TOOLS_KEY, ToolScope, compose_instruction};
use crate::live::{LiveEvent, processor::ControlEvent, task_tools};
use crate::state::State;
use crate::tasks::{
    InvocationCompletion, OperationId, OwnedInvocation, TaskCommand, TaskEffect, TaskId,
    TaskObservation, TaskRuntime, TaskSessionSnapshot,
};

use super::task_transcript::{TaskTranscript, foreground};

/// The context turn sent when no task is foreground.
const NO_FOREGROUND: &str = "No task is foreground. Ask what the caller needs, then start or resume an installed skill with task_control.";

#[derive(Default)]
pub(super) struct TaskLane {
    /// Every task tool declaration and what is declared now, on a model that
    /// accepts `contextUpdate`. `None`: every skill's tools stay declared.
    scope: Option<Arc<ToolScope>>,
    /// The session state, where the declared names are published.
    state: State,
    calls: HashMap<OperationId, FunctionCall>,
    last_instruction: Option<String>,
    last_status: Option<Value>,
    transcript: TaskTranscript,
    completion_requests: HashMap<TaskId, (u64, Value)>,
}

impl TaskLane {
    pub(super) fn new(scope: Option<Arc<ToolScope>>, state: State) -> Self {
        Self {
            scope,
            state,
            ..Self::default()
        }
    }

    /// Bring what the model has declared in line with the foreground task:
    /// `task_control` and every skill's `start_{skill}`, the tools the
    /// foreground skill offers now, and any
    /// tool whose call still waits for its response; and the system
    /// instruction with the foreground skill's brief after the connect-time
    /// instruction. Call it before a tool response: updates are processed in
    /// order with the rest of the input, so the model reads the response with
    /// the tools it makes available. Does nothing without a scope.
    async fn sync_context(&self, runtime: &TaskRuntime, writer: &Arc<dyn SessionWriter>) {
        let Some(scope) = &self.scope else {
            return;
        };
        let mut names = task_tools::entry_names(runtime);
        if let Some((skill, tools)) = runtime.foreground_offer() {
            names.extend(tools.iter().map(|tool| format!("{skill}__{tool}")));
        }
        // Withdrawing a tool whose call is still open would leave its
        // response answering a function the model no longer has.
        names.extend(self.calls.values().map(|call| call.name.clone()));
        let brief = runtime.foreground_brief().unwrap_or_default();
        let instruction = compose_instruction(scope.base_instruction(), &brief);
        if let Some(update) = scope.sync(&names, Some(instruction)) {
            if let Err(error) = writer.update_context(update).await {
                tracing::warn!(%error, "re-declaring the task tools failed");
            }
            let _ = self
                .state
                .session()
                .set(DECLARED_TOOLS_KEY, scope.declared());
        }
    }

    /// The task context sent as a context turn: under a scope only what
    /// changes as the task runs, its brief riding the system instruction;
    /// otherwise the whole foreground instruction.
    fn task_context(&self, runtime: &TaskRuntime) -> String {
        if self.scope.is_some() {
            match runtime.foreground_context() {
                Some(context) if context.is_empty() => "No values or results yet.".into(),
                Some(context) => context,
                None => NO_FOREGROUND.into(),
            }
        } else {
            runtime
                .foreground_instruction()
                .unwrap_or_else(|| NO_FOREGROUND.into())
        }
    }

    pub(super) fn check_settled_command(
        &self,
        command: &TaskCommand,
        runtime: &TaskRuntime,
    ) -> Result<(), crate::tasks::TaskError> {
        let snapshot = runtime.snapshot();
        let owner = match command {
            TaskCommand::Decide {
                operation,
                approve: true,
            } => snapshot
                .operations
                .iter()
                .find(|candidate| &candidate.id == operation)
                .map(|candidate| (&candidate.owner.task, candidate.owner.revision)),
            TaskCommand::Complete { task, .. } => snapshot
                .tasks
                .iter()
                .find(|candidate| &candidate.id == task)
                .map(|candidate| (&candidate.id, candidate.revision)),
            _ => None,
        };
        if owner.is_some_and(|(task, revision)| self.transcript.has_input_for(task, revision)) {
            return Err(crate::tasks::TaskError(
                "wait for the current spoken turn and task context to settle before approving or completing".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn input(&mut self, text: &str, runtime: &TaskRuntime) {
        self.transcript.input(text, foreground(runtime));
    }

    pub(super) fn output(&mut self, text: &str, runtime: &TaskRuntime) {
        self.transcript.output(text, foreground(runtime));
    }

    pub(super) fn observe_turn(
        &mut self,
        runtime: &mut TaskRuntime,
        generation: bool,
        event_tx: &broadcast::Sender<LiveEvent>,
    ) {
        for ((task, revision), observation) in self.transcript.observations(generation) {
            if let Err(error) = runtime.observe(&task, revision, observation) {
                let _ = event_tx.send(LiveEvent::Error(error.to_string()));
            }
        }
        if !generation {
            self.transcript = TaskTranscript::default();
        }
    }

    pub(super) fn interrupt(
        &mut self,
        runtime: &mut TaskRuntime,
        heard_chars: Option<usize>,
        event_tx: &broadcast::Sender<LiveEvent>,
    ) {
        if let Some((task, revision)) = self.transcript.interrupt(heard_chars)
            && let Err(error) = runtime.observe(&task, revision, TaskObservation::Interrupted)
        {
            let _ = event_tx.send(LiveEvent::Error(error.to_string()));
        }
    }

    pub(super) async fn drive_services(
        &mut self,
        runtime: &mut TaskRuntime,
        writer: &Arc<dyn SessionWriter>,
        completion_tx: &mpsc::WeakSender<ControlEvent>,
        event_tx: &broadcast::Sender<LiveEvent>,
    ) {
        for work in runtime.take_ready_services() {
            let completion_tx = completion_tx.clone();
            tokio::spawn(async move {
                let completion = work.execute().await;
                if let Some(sender) = completion_tx.upgrade() {
                    let _ = sender
                        .send(ControlEvent::TaskServicesCompleted(completion))
                        .await;
                }
            });
        }
        for accepted in runtime.take_service_effects() {
            let current = foreground(runtime).is_some_and(|(task, revision)| {
                task == accepted.owner.task && revision == accepted.owner.revision
            });
            let delivery = match accepted.effect {
                TaskEffect::Context(text) if current => Some((text, false)),
                TaskEffect::Prompt(text) if current => Some((text, true)),
                TaskEffect::Error(error) => {
                    let _ = event_tx.send(LiveEvent::Error(format!(
                        "task {} revision {}: {error}",
                        accepted.owner.task.0, accepted.owner.revision
                    )));
                    None
                }
                // State and memory effects are consumed by the reducer. Background
                // context remains in its task and is projected when it resumes.
                TaskEffect::Context(_)
                | TaskEffect::Prompt(_)
                | TaskEffect::Set { .. }
                | TaskEffect::Remember(_) => None,
            };
            if let Some((text, trigger_turn)) = delivery
                && let Err(error) = writer
                    .send_client_content(vec![Content::model(text)], trigger_turn)
                    .await
            {
                let _ = event_tx.send(LiveEvent::Error(error.to_string()));
            }
        }
    }

    pub(super) fn execute(
        invocation: OwnedInvocation,
        completion_tx: &mpsc::WeakSender<ControlEvent>,
    ) {
        let completion_tx = completion_tx.clone();
        tokio::spawn(async move {
            let completion = invocation.execute().await;
            if let Some(sender) = completion_tx.upgrade() {
                let _ = sender.send(ControlEvent::TaskCompleted(completion)).await;
            }
        });
    }

    pub(super) async fn calls(
        &mut self,
        calls: Vec<FunctionCall>,
        runtime: &mut TaskRuntime,
        writer: &Arc<dyn SessionWriter>,
        completion_tx: &mpsc::WeakSender<ControlEvent>,
    ) {
        for call in calls {
            let start = task_tools::start_command(&call.name, call.args.clone(), runtime);
            let is_control = call.name == task_tools::CONTROL_TOOL || start.is_some();
            let command = if let Some(start) = start {
                Ok(start)
            } else if is_control {
                task_tools::model_command(call.args.clone())
            } else {
                let snapshot = runtime.snapshot();
                let task = snapshot
                    .foreground
                    .as_ref()
                    .and_then(|id| snapshot.tasks.iter().find(|task| &task.id == id));
                match task {
                    Some(task) => {
                        let prefix = format!("{}__", task.skill.name);
                        match call.name.strip_prefix(&prefix) {
                            Some(tool) => Ok(TaskCommand::Invoke {
                                task: task.id.clone(),
                                tool: tool.into(),
                                args: call.args.clone(),
                                idempotency_key: None,
                            }),
                            None => Err(crate::tasks::TaskError(
                                "tool does not belong to the foreground skill".into(),
                            )),
                        }
                    }
                    None => Err(crate::tasks::TaskError(
                        "start or resume a task before calling its tools".into(),
                    )),
                }
            };
            let starts_task = matches!(&command, Ok(TaskCommand::Start { .. }));
            let mut deferred = false;
            let result = command.and_then(|command| {
                if let TaskCommand::Complete { task, output } = &command
                    && let Some((id, revision)) = foreground(runtime)
                    && id == *task
                    && self.transcript.has_input_for(task, revision)
                {
                    if self.completion_requests.contains_key(task) {
                        return Err(crate::tasks::TaskError(
                            "completion already waits for this turn's context".into(),
                        ));
                    }
                    self.completion_requests
                        .insert(task.clone(), (revision, output.clone()));
                    deferred = true;
                    return Ok(None);
                }
                self.check_settled_command(&command, runtime)?;
                runtime.command(command)
            });
            let response = match result {
                Err(error) => json!({"error":error.to_string()}),
                Ok(invocation) => {
                    if starts_task && let Some(owner) = foreground(runtime) {
                        self.transcript.claim_input(owner);
                    }
                    let snapshot = runtime.snapshot();
                    if is_control {
                        if let Some(invocation) = invocation {
                            Self::execute(invocation, completion_tx);
                        }
                        if deferred {
                            json!({"status":"completion_requested", "task":snapshot.foreground,
                                "instruction":"Finish the current response. Completion will be checked after this turn's extraction and memory work settles; it has not completed yet."})
                        } else {
                            model_view(runtime, &snapshot, self.scope.is_some())
                        }
                    } else {
                        let pending = snapshot
                            .foreground
                            .as_ref()
                            .and_then(|id| snapshot.tasks.iter().find(|task| &task.id == id))
                            .and_then(|task| task.pending.clone());
                        if let Some(operation) = pending {
                            self.calls.insert(operation.clone(), call.clone());
                            let status = snapshot
                                .operations
                                .iter()
                                .find(|op| op.id == operation)
                                .map(|op| op.status);
                            if let Some(invocation) = invocation {
                                Self::execute(invocation, completion_tx);
                            }
                            json!({"operation":operation,"status":status,"task":snapshot.foreground})
                        } else {
                            // Idempotent retries may resolve from a stored receipt without new work.
                            json!({"status":"settled", "context":model_view(runtime, &snapshot, self.scope.is_some())})
                        }
                    }
                }
            };
            let scheduling = (!is_control).then_some(FunctionResponseScheduling::WhenIdle);
            self.sync_context(runtime, writer).await;
            if let Err(error) = writer
                .send_tool_response(vec![FunctionResponse {
                    name: call.name,
                    id: call.id,
                    response,
                    scheduling,
                }])
                .await
            {
                tracing::warn!(%error, "task response delivery failed");
            }
        }
    }

    pub(super) async fn complete(
        &mut self,
        completion: InvocationCompletion,
        runtime: &mut TaskRuntime,
        writer: &Arc<dyn SessionWriter>,
    ) {
        let delivery = runtime.complete(completion);
        let Some(call) = self.calls.remove(&delivery.operation_id) else {
            self.sync_context(runtime, writer).await;
            return;
        };
        let response = if delivery.foreground && !delivery.stale {
            match delivery.result {
                Ok(result) => {
                    json!({"task":delivery.task_id,"operation":delivery.operation_id,"result":result})
                }
                Err(error) => {
                    json!({"task":delivery.task_id,"operation":delivery.operation_id,"error":error})
                }
            }
        } else {
            // Background receipts remain in their owner task. Do not inject their
            // payload into another skill's speaking context.
            json!({"task":delivery.task_id,"operation":delivery.operation_id,
                "status":if delivery.stale { "superseded" } else { "stored_for_task" }})
        };
        let scheduling = Some(if delivery.foreground && !delivery.stale {
            FunctionResponseScheduling::WhenIdle
        } else {
            FunctionResponseScheduling::Silent
        });
        // The result may have advanced the task's flow: declare what it
        // offers now, before the model reads the result. The completed call
        // is no longer open, so its tool may go.
        self.sync_context(runtime, writer).await;
        if let Err(error) = writer
            .send_tool_response(vec![FunctionResponse {
                name: call.name,
                id: call.id,
                response,
                scheduling,
            }])
            .await
        {
            tracing::warn!(%error, "task completion delivery failed");
        }
    }

    pub(super) async fn publish(
        &mut self,
        runtime: &mut TaskRuntime,
        cache: &Option<Arc<parking_lot::Mutex<TaskSessionSnapshot>>>,
        writer: &Arc<dyn SessionWriter>,
        completion_tx: &mpsc::WeakSender<ControlEvent>,
        event_tx: &broadcast::Sender<LiveEvent>,
    ) {
        self.drive_services(runtime, writer, completion_tx, event_tx)
            .await;
        let before = runtime.snapshot();
        let settled: Vec<_> = self
            .completion_requests
            .iter()
            .filter_map(|(id, (revision, _))| {
                let task = before.tasks.iter().find(|task| &task.id == id);
                let invalid = task.is_none_or(|task| task.revision != *revision)
                    || before.foreground.as_ref() != Some(id);
                let ready = task
                    .is_some_and(|task| !task.services_pending && task.pending.is_none())
                    && !self.transcript.has_input_for(id, *revision);
                (invalid || ready).then_some((id.clone(), invalid))
            })
            .collect();
        for (id, invalid) in settled {
            let (_, output) = self
                .completion_requests
                .remove(&id)
                .expect("request collected");
            if invalid {
                continue;
            }
            if let Err(error) = runtime.command(TaskCommand::Complete { task: id, output }) {
                let _ = event_tx.send(LiveEvent::Error(error.to_string()));
            }
        }
        let snapshot = runtime.snapshot();
        let revoked: Vec<_> = snapshot
            .operations
            .iter()
            .filter(|operation| {
                matches!(
                    operation.status,
                    crate::tasks::OperationStatus::Declined
                        | crate::tasks::OperationStatus::Cancelled
                ) && self.calls.contains_key(&operation.id)
            })
            .collect();
        for operation in revoked {
            if let Some(call) = self.calls.remove(&operation.id) {
                let foreground = snapshot.foreground.as_ref() == Some(&operation.owner.task)
                    && snapshot.tasks.iter().any(|task| {
                        task.id == operation.owner.task && task.revision == operation.owner.revision
                    });
                let response = FunctionResponse {
                    name: call.name,
                    id: call.id,
                    response: json!({"task":operation.owner.task,"operation":operation.id,"status":operation.status,"error":operation.error}),
                    scheduling: Some(if foreground {
                        FunctionResponseScheduling::WhenIdle
                    } else {
                        FunctionResponseScheduling::Silent
                    }),
                };
                if let Err(error) = writer.send_tool_response(vec![response]).await {
                    let _ = event_tx.send(LiveEvent::Error(error.to_string()));
                }
            }
        }
        if let Some(cache) = cache {
            *cache.lock() = snapshot.clone();
        }
        // A turn, a service result or a trusted command may have moved the
        // foreground task or its flow.
        self.sync_context(runtime, writer).await;
        let instruction = self.task_context(runtime);
        if self.last_instruction.as_ref() != Some(&instruction) {
            match writer
                .send_client_content(
                    vec![Content::model(format!(
                        "Current task context (replaces earlier task context):\n{instruction}"
                    ))],
                    false,
                )
                .await
            {
                Ok(()) => self.last_instruction = Some(instruction),
                Err(error) => {
                    let _ = event_tx.send(LiveEvent::Error(error.to_string()));
                }
            }
        }
        let value = serde_json::to_value(&snapshot).expect("task snapshots serialize");
        if self.last_status.as_ref() != Some(&value) {
            self.last_status = Some(value);
            let _ = event_tx.send(LiveEvent::TasksChanged(snapshot));
        }
    }

    pub(super) fn forget_cancelled_calls(&mut self, ids: &[String]) {
        // A provider speech cancellation is not business cancellation. Let the
        // owned operation finish, but stop answering its cancelled wire call.
        self.calls
            .retain(|_, call| !call.id.as_ref().is_some_and(|id| ids.contains(id)));
    }
}

/// The tasks as the model reads them in a `task_control` response. `scoped`:
/// the foreground skill's brief is in the system instruction, so only its
/// changing context is repeated here.
fn model_view(runtime: &TaskRuntime, snapshot: &TaskSessionSnapshot, scoped: bool) -> Value {
    let tasks: Vec<_> = snapshot
        .tasks
        .iter()
        .map(|task| {
            json!({
                "id":task.id, "skill":task.skill, "revision":task.revision, "status":task.status,
            })
        })
        .collect();
    let instruction = if scoped {
        runtime.foreground_context()
    } else {
        runtime.foreground_instruction()
    };
    json!({"foreground":snapshot.foreground,"tasks":tasks,"instruction":instruction})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::two_skill_runtime;
    use async_trait::async_trait;
    use gemini_genai_rs::prelude::{ContextUpdate, Part};
    use gemini_genai_rs::session::SessionError;
    use parking_lot::Mutex;

    /// What the lane sent, in order.
    #[derive(Debug, Clone, PartialEq)]
    enum Sent {
        Update {
            tools: Option<Vec<String>>,
            instruction: Option<String>,
        },
        Response(String),
        Context(String),
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Sent>>);

    impl Recorder {
        fn take(&self) -> Vec<Sent> {
            std::mem::take(&mut *self.0.lock())
        }
    }

    #[async_trait]
    impl SessionWriter for Recorder {
        async fn send_audio(&self, _: bytes::Bytes) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_text(&self, _: String) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_tool_response(
            &self,
            responses: Vec<FunctionResponse>,
        ) -> Result<(), SessionError> {
            let mut sent = self.0.lock();
            sent.extend(responses.into_iter().map(|r| Sent::Response(r.name)));
            Ok(())
        }
        async fn send_client_content(
            &self,
            turns: Vec<Content>,
            _: bool,
        ) -> Result<(), SessionError> {
            for turn in turns {
                for part in turn.parts {
                    if let Part::Text { text } = part {
                        self.0.lock().push(Sent::Context(text));
                    }
                }
            }
            Ok(())
        }
        async fn send_video(&self, _: bytes::Bytes) -> Result<(), SessionError> {
            Ok(())
        }
        async fn update_instruction(&self, _: String) -> Result<(), SessionError> {
            Ok(())
        }
        async fn update_context(&self, update: ContextUpdate) -> Result<(), SessionError> {
            let tools = update.tools.map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t.function_declarations.as_ref())
                    .flatten()
                    .map(|d| d.name.clone())
                    .collect()
            });
            let instruction = update.system_instruction.map(|c| {
                c.parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect()
            });
            self.0.lock().push(Sent::Update { tools, instruction });
            Ok(())
        }
        async fn signal_activity_start(&self) -> Result<(), SessionError> {
            Ok(())
        }
        async fn signal_activity_end(&self) -> Result<(), SessionError> {
            Ok(())
        }
        async fn disconnect(&self) -> Result<(), SessionError> {
            Ok(())
        }
    }

    struct Harness {
        runtime: TaskRuntime,
        lane: TaskLane,
        writer: Arc<Recorder>,
        completions: mpsc::Receiver<ControlEvent>,
        completion_tx: mpsc::Sender<ControlEvent>,
    }

    impl Harness {
        fn new(scoped: bool) -> Self {
            let runtime = two_skill_runtime();
            let scope = scoped.then(|| {
                Arc::new(ToolScope::new(
                    task_tools::declarations(&runtime),
                    Some("Base.".into()),
                    &task_tools::entry_names(&runtime),
                ))
            });
            let (completion_tx, completions) = mpsc::channel(8);
            Self {
                runtime,
                lane: TaskLane::new(scope, State::new()),
                writer: Arc::new(Recorder::default()),
                completions,
                completion_tx,
            }
        }

        async fn call(&mut self, name: &str, args: Value) {
            let writer: Arc<dyn SessionWriter> = self.writer.clone();
            let call = FunctionCall {
                name: name.into(),
                args,
                id: Some(format!("call-{name}")),
            };
            self.lane
                .calls(
                    vec![call],
                    &mut self.runtime,
                    &writer,
                    &self.completion_tx.downgrade(),
                )
                .await;
        }

        /// Deliver the next tool completion, as the control lane does.
        async fn complete_next(&mut self) {
            let writer: Arc<dyn SessionWriter> = self.writer.clone();
            match self.completions.recv().await {
                Some(ControlEvent::TaskCompleted(completion)) => {
                    self.lane
                        .complete(completion, &mut self.runtime, &writer)
                        .await;
                }
                _ => panic!("expected a task completion"),
            }
        }

        async fn publish(&mut self) {
            let writer: Arc<dyn SessionWriter> = self.writer.clone();
            let (event_tx, _) = broadcast::channel(8);
            self.lane
                .publish(
                    &mut self.runtime,
                    &None,
                    &writer,
                    &self.completion_tx.downgrade(),
                    &event_tx,
                )
                .await;
        }
    }

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[tokio::test]
    async fn starting_a_task_declares_its_offered_tools_before_the_response() {
        let mut h = Harness::new(true);
        h.call("task_control", json!({"action":"start","skill":"billing"}))
            .await;
        let sent = h.writer.take();
        let Sent::Update { tools, instruction } = &sent[0] else {
            panic!("the update goes first: {sent:?}");
        };
        // The flow offers `verify` only; `faq__answer` stays undeclared.
        // Declarations keep the catalog's order.
        assert_eq!(
            tools,
            &Some(self::tools(&[
                "task_control",
                "start_billing",
                "start_faq",
                "billing__verify"
            ]))
        );
        let instruction = instruction.as_deref().unwrap();
        assert!(instruction.starts_with("Base.\n\nTask task-1 / skill billing@1.0.0"));
        assert!(instruction.ends_with("You handle billing."));
        assert_eq!(sent[1], Sent::Response("task_control".into()));
    }

    #[tokio::test]
    async fn a_result_that_advances_the_flow_redeclares_before_the_model_reads_it() {
        let mut h = Harness::new(true);
        h.call("task_control", json!({"action":"start","skill":"billing"}))
            .await;
        h.call("billing__verify", json!({})).await;
        h.writer.take();
        h.complete_next().await;
        let sent = h.writer.take();
        assert_eq!(
            sent,
            vec![
                Sent::Update {
                    tools: Some(tools(&[
                        "task_control",
                        "start_billing",
                        "start_faq",
                        "billing__pay"
                    ])),
                    instruction: None,
                },
                Sent::Response("billing__verify".into()),
            ],
            "the instruction is unchanged, so only the tools go out"
        );
    }

    #[tokio::test]
    async fn switching_tasks_keeps_a_tool_whose_call_is_still_open() {
        let mut h = Harness::new(true);
        h.call("task_control", json!({"action":"start","skill":"billing"}))
            .await;
        h.call("billing__verify", json!({})).await;
        h.call("task_control", json!({"action":"start","skill":"faq"}))
            .await;
        let declared: Vec<_> = h
            .writer
            .take()
            .into_iter()
            .filter_map(|s| match s {
                Sent::Update { tools: Some(t), .. } => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(
            declared.last().unwrap(),
            &[
                "task_control",
                "start_billing",
                "start_faq",
                "billing__verify",
                "faq__answer"
            ],
            "billing's verify call has not been answered yet"
        );
        // Once its response is delivered, it goes.
        h.complete_next().await;
        let last = h
            .writer
            .take()
            .into_iter()
            .find_map(|s| match s {
                Sent::Update { tools: Some(t), .. } => Some(t),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            last,
            ["task_control", "start_billing", "start_faq", "faq__answer"]
        );
    }

    #[tokio::test]
    async fn a_typed_start_starts_the_skill_with_its_arguments_as_input() {
        let mut h = Harness::new(true);
        h.call("start_billing", json!({"account": "A-1"})).await;
        let snapshot = h.runtime.snapshot();
        let task = &snapshot.tasks[0];
        assert_eq!(task.skill.name, "billing");
        assert_eq!(snapshot.foreground.as_ref(), Some(&task.id));
        let sent = h.writer.take();
        assert!(
            matches!(&sent[0], Sent::Update { tools: Some(t), .. } if t.contains(&"billing__verify".to_string()))
        );
        assert_eq!(sent[1], Sent::Response("start_billing".into()));

        // `parent_task` names the parent; it is not part of the input.
        h.call(
            "start_faq",
            json!({"parent_task": task.id.0, "question": "fees?"}),
        )
        .await;
        let snapshot = h.runtime.snapshot();
        let child = snapshot
            .tasks
            .iter()
            .find(|t| t.skill.name == "faq")
            .unwrap();
        assert_eq!(child.parent.as_ref(), Some(&task.id));
    }

    #[tokio::test]
    async fn completing_the_task_withdraws_its_tools_and_brief() {
        let mut h = Harness::new(true);
        h.call("task_control", json!({"action":"start","skill":"faq"}))
            .await;
        h.writer.take();
        h.call("task_control", json!({"action":"complete","task":"task-1"}))
            .await;
        let sent = h.writer.take();
        assert_eq!(
            sent[0],
            Sent::Update {
                tools: Some(tools(&["task_control", "start_billing", "start_faq"])),
                instruction: Some("Base.".into()),
            }
        );
        assert_eq!(sent[1], Sent::Response("task_control".into()));
    }

    #[tokio::test]
    async fn the_context_turn_carries_only_what_changes_under_a_scope() {
        let mut scoped = Harness::new(true);
        scoped
            .call("task_control", json!({"action":"start","skill":"billing"}))
            .await;
        scoped.publish().await;
        let context = scoped
            .writer
            .take()
            .into_iter()
            .find_map(|s| match s {
                Sent::Context(text) => Some(text),
                _ => None,
            })
            .unwrap();
        assert!(!context.contains("You handle billing."), "{context}");

        // Without a scope nothing is re-declared and the whole instruction
        // rides the context turn, as before.
        let mut plain = Harness::new(false);
        plain
            .call("task_control", json!({"action":"start","skill":"billing"}))
            .await;
        plain.publish().await;
        let sent = plain.writer.take();
        assert!(!sent.iter().any(|s| matches!(s, Sent::Update { .. })));
        assert!(
            sent.iter()
                .any(|s| matches!(s, Sent::Context(text) if text.contains("You handle billing.")))
        );
    }
}
