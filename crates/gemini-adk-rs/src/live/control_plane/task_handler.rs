//! Live adapter for the task reducer. Workers cannot reach the session writer.

use std::{collections::HashMap, sync::Arc};

use gemini_genai_rs::prelude::{
    Content, FunctionCall, FunctionResponse, FunctionResponseScheduling,
};
use gemini_genai_rs::session::SessionWriter;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::live::{LiveEvent, processor::ControlEvent, task_tools};
use crate::tasks::{
    InvocationCompletion, OperationId, OwnedInvocation, TaskCommand, TaskEffect, TaskId,
    TaskObservation, TaskRuntime, TaskSessionSnapshot,
};

use super::task_transcript::{TaskTranscript, foreground};

#[derive(Default)]
pub(super) struct TaskLane {
    calls: HashMap<OperationId, FunctionCall>,
    last_instruction: Option<String>,
    last_status: Option<Value>,
    transcript: TaskTranscript,
    completion_requests: HashMap<TaskId, (u64, Value)>,
}

impl TaskLane {
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
            let is_control = call.name == task_tools::CONTROL_TOOL;
            let command = if is_control {
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
                            model_view(runtime, &snapshot)
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
                            json!({"status":"settled", "context":model_view(runtime, &snapshot)})
                        }
                    }
                }
            };
            let scheduling = (!is_control).then_some(FunctionResponseScheduling::WhenIdle);
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
        let instruction = runtime.foreground_instruction().unwrap_or_else(|| {
            "No task is foreground. Ask what the caller needs, then start or resume an installed skill with task_control.".into()
        });
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

fn model_view(runtime: &TaskRuntime, snapshot: &TaskSessionSnapshot) -> Value {
    let tasks: Vec<_> = snapshot
        .tasks
        .iter()
        .map(|task| {
            json!({
                "id":task.id, "skill":task.skill, "revision":task.revision, "status":task.status,
            })
        })
        .collect();
    json!({"foreground":snapshot.foreground,"tasks":tasks,"instruction":runtime.foreground_instruction()})
}
