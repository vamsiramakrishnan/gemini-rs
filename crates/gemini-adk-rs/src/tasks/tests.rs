use super::*;
use crate::{
    error::ToolError,
    flow::{Enforcement, Flow, Guard},
    tool::{ContextTool, ToolDispatcher},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

fn skill(counter: Arc<AtomicUsize>, commit: bool, fail: bool) -> CompiledSkill {
    CompiledSkill {
        services: None,
        name: "support".into(),
        version: "1.2.3".into(),
        description: "Support tasks".into(),
        instruction: "Help with the current task".into(),
        input_schema: json!({"type":"object"}),
        output_schema: json!({"type":"object"}),
        validate_input: Arc::new(|value| {
            if value.is_object() {
                Ok(())
            } else {
                Err("input must be an object".into())
            }
        }),
        validate_output: Arc::new(|value| {
            if value.get("value").is_some_and(Value::is_number) {
                Ok(())
            } else {
                Err("output requires a numeric value".into())
            }
        }),
        initial_state: HashMap::from([("value".into(), json!(0))]),
        flow_factory: None,
        tool_factory: Arc::new(move |state| {
            let mut dispatcher = ToolDispatcher::new();
            let count = counter.clone();
            dispatcher.register(ContextTool::new(
                "save",
                "Save a value",
                None,
                move |args, ctx| {
                    let count = count.clone();
                    let state = state.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        assert!(
                            ctx.task.is_some(),
                            "ownership reaches the contextual implementation"
                        );
                        let value = args.get("value").cloned().unwrap_or(json!(1));
                        state.set("value", value.clone()).unwrap();
                        ctx.state
                            .set("context_owner", ctx.task.as_ref().unwrap().task.0.clone())
                            .unwrap();
                        if fail {
                            Err(ToolError::ExecutionFailed(
                                "connection lost after submission".into(),
                            ))
                        } else {
                            Ok(json!({"value": value}))
                        }
                    }
                },
            ));
            Ok(dispatcher)
        }),
        tools: vec![TaskTool {
            name: "save".into(),
            description: "Save".into(),
            parameters: None,
            effect: if commit {
                TaskToolEffect::Commit {
                    idempotency_argument: "request_id".into(),
                }
            } else {
                TaskToolEffect::Read
            },
        }],
        receipt_applier: Some(Arc::new(|_, result, state| {
            state
                .set("value", result.get("value").cloned().unwrap_or(Value::Null))
                .map_err(|error| error.to_string())
        })),
        project_output: Some(Arc::new(|state| {
            Ok(json!({"value": state.get_raw("value")}))
        })),
    }
}
fn runtime(counter: Arc<AtomicUsize>, commit: bool, fail: bool) -> TaskRuntime {
    TaskRuntime::new(vec![skill(counter, commit, fail)]).unwrap()
}
fn start(runtime: &mut TaskRuntime, value: i64, parent: Option<TaskId>) -> TaskId {
    runtime
        .command(TaskCommand::Start {
            skill: "support".into(),
            input: json!({"value": value}),
            parent,
        })
        .unwrap();
    runtime.snapshot().foreground.unwrap()
}
fn invoke(
    runtime: &mut TaskRuntime,
    task: &TaskId,
    value: i64,
    key: Option<&str>,
) -> Option<OwnedInvocation> {
    runtime
        .command(TaskCommand::Invoke {
            task: task.clone(),
            tool: "save".into(),
            args: json!({"value":value}),
            idempotency_key: key.map(str::to_owned),
        })
        .unwrap()
}
fn task<'a>(snapshot: &'a TaskSessionSnapshot, id: &TaskId) -> &'a TaskSnapshot {
    snapshot.tasks.iter().find(|task| &task.id == id).unwrap()
}

#[tokio::test]
async fn suspended_completions_update_only_the_original_activation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut runtime = runtime(calls.clone(), false, false);
    let first = start(&mut runtime, 10, None);
    let invocation = invoke(&mut runtime, &first, 42, None).unwrap();
    let second = start(&mut runtime, 20, None);
    let delivery = runtime.complete(invocation.execute().await);
    assert!(!delivery.stale);
    assert!(!delivery.foreground);
    assert_eq!(delivery.task_id, first);
    runtime
        .command(TaskCommand::Complete {
            task: first.clone(),
            output: Value::Null,
        })
        .unwrap();
    runtime
        .command(TaskCommand::Complete {
            task: second.clone(),
            output: Value::Null,
        })
        .unwrap();
    let snapshot = runtime.snapshot();
    assert_eq!(task(&snapshot, &first).output, Some(json!({"value":42})));
    assert_eq!(task(&snapshot, &second).output, Some(json!({"value":20})));
    assert_eq!(task(&snapshot, &first).skill.version, "1.2.3");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn revised_workers_cannot_overwrite_current_state_and_duplicate_completion_is_inert() {
    let mut runtime = runtime(Arc::new(AtomicUsize::new(0)), false, false);
    let id = start(&mut runtime, 1, None);
    let invocation = invoke(&mut runtime, &id, 99, None).unwrap();
    let completed = invocation.execute().await;
    runtime
        .command(TaskCommand::Revise {
            task: id.clone(),
            expected_revision: 1,
            input: json!({"value":7}),
        })
        .unwrap();
    assert!(runtime.complete(completed).stale);
    let next = invoke(&mut runtime, &id, 8, None).unwrap().execute().await;
    assert!(!runtime.complete(next.clone()).stale);
    assert!(runtime.complete(next).stale);
    runtime
        .command(TaskCommand::Complete {
            task: id.clone(),
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &id).output,
        Some(json!({"value":8}))
    );
}

#[tokio::test]
async fn approval_is_exact_and_invalidated_by_switch_or_revision() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut runtime = runtime(count.clone(), true, false);
    let first = start(&mut runtime, 1, None);
    assert!(invoke(&mut runtime, &first, 12, Some("purchase-1")).is_none());
    let proposal = runtime.snapshot().operations[0].clone();
    assert_eq!(proposal.args, json!({"value":12,"request_id":"purchase-1"}));
    assert_eq!(proposal.status, OperationStatus::AwaitingApproval);
    let _second = start(&mut runtime, 2, None);
    assert!(
        runtime
            .command(TaskCommand::Decide {
                operation: proposal.id,
                approve: true
            })
            .is_err()
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    runtime
        .command(TaskCommand::Resume {
            task: first.clone(),
        })
        .unwrap();
    assert!(invoke(&mut runtime, &first, 12, Some("purchase-1")).is_none());
    let pending = task(&runtime.snapshot(), &first).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: pending,
            approve: true,
        })
        .unwrap()
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(!runtime.complete(invocation.execute().await).stale);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_commits_preserve_receipts_without_merging_state() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut runtime = runtime(count.clone(), true, false);
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 99, Some("charge-1"));
    let pending = task(&runtime.snapshot(), &id).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: pending.clone(),
            approve: true,
        })
        .unwrap()
        .unwrap();
    let completion = invocation.execute().await;
    runtime
        .command(TaskCommand::Cancel { task: id.clone() })
        .unwrap();
    let delivery = runtime.complete(completion);
    assert!(delivery.stale);
    let snapshot = runtime.snapshot();
    assert_eq!(task(&snapshot, &id).status, TaskStatus::Cancelled);
    let operation = snapshot
        .operations
        .iter()
        .find(|op| op.id == pending)
        .unwrap();
    assert_eq!(operation.status, OperationStatus::Succeeded);
    assert!(operation.cancellation_requested);
    assert_eq!(operation.result, Some(json!({"value":99})));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unknown_commits_require_reconciliation_and_never_retry_the_external_call() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut runtime = runtime(count.clone(), true, true);
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 5, Some("charge-1"));
    let pending = task(&runtime.snapshot(), &id).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: pending.clone(),
            approve: true,
        })
        .unwrap()
        .unwrap();
    runtime.complete(invocation.execute().await);
    assert_eq!(
        runtime.snapshot().operations[0].status,
        OperationStatus::Unknown
    );
    assert!(
        runtime
            .command(TaskCommand::Invoke {
                task: id.clone(),
                tool: "save".into(),
                args: json!({"value":5}),
                idempotency_key: Some("charge-1".into())
            })
            .is_err()
    );
    assert!(
        runtime
            .command(TaskCommand::Complete {
                task: id.clone(),
                output: Value::Null
            })
            .is_err()
    );
    runtime
        .command(TaskCommand::Reconcile {
            operation: pending,
            outcome: ReconciledOutcome::Succeeded {
                result: json!({"value":5}),
            },
        })
        .unwrap();
    let cached = invoke(&mut runtime, &id, 5, Some("charge-1")).unwrap();
    assert_eq!(
        runtime.complete(cached.execute().await).result.unwrap(),
        json!({"value":5})
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(
        runtime
            .command(TaskCommand::Invoke {
                task: id,
                tool: "save".into(),
                args: json!({"value":6}),
                idempotency_key: Some("charge-1".into())
            })
            .is_err()
    );
}

#[tokio::test]
async fn parent_progress_survives_a_child_task_and_resumes_on_child_completion() {
    let mut runtime = runtime(Arc::new(AtomicUsize::new(0)), false, false);
    let parent = start(&mut runtime, 0, None);
    let invocation = invoke(&mut runtime, &parent, 7, None).unwrap();
    runtime.complete(invocation.execute().await);
    let child = start(&mut runtime, 10, Some(parent.clone()));
    runtime
        .command(TaskCommand::Complete {
            task: child,
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(runtime.snapshot().foreground, Some(parent.clone()));
    runtime
        .command(TaskCommand::Complete {
            task: parent.clone(),
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &parent).output,
        Some(json!({"value":7}))
    );
}

#[test]
fn invalid_input_and_missing_commit_key_have_no_effects() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut runtime = runtime(count.clone(), true, false);
    assert!(
        runtime
            .command(TaskCommand::Start {
                skill: "support".into(),
                input: json!(42),
                parent: None
            })
            .is_err()
    );
    assert!(runtime.snapshot().tasks.is_empty());
    let id = start(&mut runtime, 0, None);
    assert!(
        runtime
            .command(TaskCommand::Invoke {
                task: id,
                tool: "save".into(),
                args: json!({}),
                idempotency_key: None
            })
            .is_err()
    );
    assert!(runtime.snapshot().operations.is_empty());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn governance_admission_and_output_validation_are_independent_of_approval() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut governed = skill(count.clone(), true, false);
    governed.flow_factory = Some(Arc::new(|| {
        Ok(Flow::new()
            .step("wait")
            .allow(["save"])
            .done(Guard::called_ok("save"))
            .step("done")
            .after("wait")
            .terminal()
            .require(["done"])
            .never("save")
            .until(Guard::is_true("ready"))
            .build()
            .unwrap()
            .compile()
            .map(|flow| crate::flow::FlowStack::new(flow, Enforcement::Enforce))
            .unwrap())
    }));
    let mut runtime = TaskRuntime::new(vec![governed]).unwrap();
    let id = start(&mut runtime, 0, None);
    assert!(
        runtime
            .command(TaskCommand::Invoke {
                task: id.clone(),
                tool: "save".into(),
                args: json!({}),
                idempotency_key: Some("x".into())
            })
            .is_err()
    );
    assert!(
        runtime
            .command(TaskCommand::Complete {
                task: id.clone(),
                output: Value::Null
            })
            .is_err()
    );
    runtime
        .command(TaskCommand::Revise {
            task: id.clone(),
            expected_revision: 1,
            input: json!({"value":0,"ready":true}),
        })
        .unwrap();
    invoke(&mut runtime, &id, 3, Some("x"));
    let pending = task(&runtime.snapshot(), &id).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: pending,
            approve: false,
        })
        .unwrap();
    assert!(invocation.is_none());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn task_command_wire_contract_and_snapshot_do_not_expose_raw_state() {
    let command: TaskCommand =
        serde_json::from_value(json!({"action":"start","skill":"support","input":{"value":1}}))
            .unwrap();
    let mut runtime = runtime(Arc::new(AtomicUsize::new(0)), false, false);
    runtime.command(command).unwrap();
    let wire = serde_json::to_value(runtime.snapshot()).unwrap();
    assert_eq!(wire["foreground"], "task-1");
    assert_eq!(wire["tasks"][0]["skill"]["version"], "1.2.3");
    assert_eq!(wire["tasks"][0]["status"], "running");
    assert!(wire["tasks"][0].get("state").is_none());
    assert!(wire["tasks"][0].get("input").is_none());
}

#[tokio::test]
async fn revising_an_approved_but_unpolled_commit_revokes_its_execution_permit() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut runtime = runtime(count.clone(), true, false);
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 12, Some("purchase-1"));
    let pending = task(&runtime.snapshot(), &id).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: pending,
            approve: true,
        })
        .unwrap()
        .unwrap();
    runtime
        .command(TaskCommand::Revise {
            task: id.clone(),
            expected_revision: 1,
            input: json!({"value":4}),
        })
        .unwrap();
    assert!(runtime.complete(invocation.execute().await).stale);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(
        runtime.snapshot().operations[0].status,
        OperationStatus::Cancelled
    );
    runtime
        .command(TaskCommand::Complete {
            task: id.clone(),
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &id).output,
        Some(json!({"value":4}))
    );
}

#[tokio::test]
async fn cached_commit_receipts_replay_only_the_original_write_set_into_a_fresh_task() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut runtime = runtime(count.clone(), true, false);
    let first = start(&mut runtime, 0, None);
    invoke(&mut runtime, &first, 17, Some("same-purchase"));
    let pending = task(&runtime.snapshot(), &first).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: pending,
            approve: true,
        })
        .unwrap()
        .unwrap();
    runtime.complete(invocation.execute().await);
    let second = start(&mut runtime, 17, None);
    let cached = invoke(&mut runtime, &second, 17, Some("same-purchase")).unwrap();
    runtime.complete(cached.execute().await);
    runtime
        .command(TaskCommand::Complete {
            task: second.clone(),
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &second).output,
        Some(json!({"value":17}))
    );
    let third = start(&mut runtime, 0, None);
    let cached = invoke(&mut runtime, &third, 17, Some("same-purchase")).unwrap();
    runtime.complete(cached.execute().await);
    runtime
        .command(TaskCommand::Complete {
            task: third.clone(),
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &third).output,
        Some(json!({"value":17}))
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resuming_projects_only_that_tasks_inputs_and_accepted_results() {
    let mut runtime = runtime(Arc::new(AtomicUsize::new(0)), false, false);
    let first = start(&mut runtime, 111, None);
    let invocation = invoke(&mut runtime, &first, 222, None).unwrap();
    let second = start(&mut runtime, 333, None);
    runtime.complete(invocation.execute().await);
    let second_prompt = runtime.foreground_instruction().unwrap();
    assert!(second_prompt.contains("333"));
    assert!(!second_prompt.contains("222"));
    runtime
        .command(TaskCommand::Resume { task: first })
        .unwrap();
    let first_prompt = runtime.foreground_instruction().unwrap();
    assert!(first_prompt.contains("111"));
    assert!(first_prompt.contains("222"));
    assert!(!first_prompt.contains("333"));
    assert_eq!(
        task(&runtime.snapshot(), &second).status,
        TaskStatus::Suspended
    );
}

#[test]
fn installed_names_cannot_collide_in_a_qualified_provider_catalog() {
    for name in ["bad__name", "bad-name", "1bad", ""] {
        let mut definition = skill(Arc::new(AtomicUsize::new(0)), false, false);
        definition.name = name.into();
        assert!(TaskRuntime::new(vec![definition]).is_err());
    }
    let mut definition = skill(Arc::new(AtomicUsize::new(0)), false, false);
    definition.tools[0].name = "tool__name".into();
    assert!(TaskRuntime::new(vec![definition]).is_err());
    let mut definition = skill(Arc::new(AtomicUsize::new(0)), false, false);
    definition.name = "a".repeat(64);
    assert!(TaskRuntime::new(vec![definition]).is_err());
}

#[tokio::test]
async fn dropping_the_runtime_revokes_queued_invocations() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut runtime = runtime(count.clone(), false, false);
    let id = start(&mut runtime, 0, None);
    let invocation = invoke(&mut runtime, &id, 1, None).unwrap();
    drop(runtime);
    let _completion = invocation.execute().await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn write_requested_task_status_fixture() {
    let Some(path) = std::env::var_os("TASK_STATUS_FIXTURE") else {
        return;
    };
    let mut runtime = runtime(Arc::new(AtomicUsize::new(0)), true, false);
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 42, Some("example-purchase"));
    std::fs::write(
        path,
        serde_json::to_string_pretty(&runtime.snapshot()).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn a_completion_from_another_runtime_cannot_match_session_local_ids() {
    let mut first = runtime(Arc::new(AtomicUsize::new(0)), false, false);
    let mut second = runtime(Arc::new(AtomicUsize::new(0)), false, false);
    let first_task = start(&mut first, 0, None);
    let second_task = start(&mut second, 0, None);
    let foreign = invoke(&mut first, &first_task, 5, None)
        .unwrap()
        .execute()
        .await;
    let local = invoke(&mut second, &second_task, 9, None).unwrap();
    assert!(second.complete(foreign).stale);
    assert!(task(&second.snapshot(), &second_task).pending.is_some());
    second.complete(local.execute().await);
    second
        .command(TaskCommand::Complete {
            task: second_task.clone(),
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(
        task(&second.snapshot(), &second_task).output,
        Some(json!({"value":9}))
    );
}

#[tokio::test]
async fn a_panicking_tool_returns_a_receipt_and_does_not_strand_the_pending_operation() {
    let mut definition = skill(Arc::new(AtomicUsize::new(0)), false, false);
    definition.tool_factory = Arc::new(|_| {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register(ContextTool::new("save", "Panics", None, |_, _| async {
            panic!("controlled test tool panic");
            #[allow(unreachable_code)]
            Ok(Value::Null)
        }));
        Ok(dispatcher)
    });
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let id = start(&mut runtime, 0, None);
    let invocation = invoke(&mut runtime, &id, 1, None).unwrap();
    let delivery = runtime.complete(invocation.execute().await);
    assert!(delivery.result.is_err());
    assert!(!delivery.stale);
    assert!(task(&runtime.snapshot(), &id).pending.is_none());
    assert_eq!(
        runtime.snapshot().operations[0].status,
        OperationStatus::Failed
    );
}

#[tokio::test]
async fn verified_reconciliation_reconstructs_declared_outputs_without_retrying_a_tool() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut definition = skill(count.clone(), true, true);
    definition.project_output = Some(Arc::new(|state| {
        state
            .get_raw("receipt")
            .ok_or_else(|| "missing saved response".into())
    }));
    definition.receipt_applier = Some(Arc::new(|_, result, state| {
        state
            .set("receipt", result.clone())
            .map_err(|error| error.to_string())
    }));
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 15, Some("verified-charge"));
    let operation = task(&runtime.snapshot(), &id).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: operation.clone(),
            approve: true,
        })
        .unwrap()
        .unwrap();
    runtime.complete(invocation.execute().await);
    assert_eq!(
        task(&runtime.snapshot(), &id).pending,
        Some(operation.clone())
    );
    runtime
        .command(TaskCommand::Reconcile {
            operation,
            outcome: ReconciledOutcome::Succeeded {
                result: json!({"value":15}),
            },
        })
        .unwrap();
    runtime
        .command(TaskCommand::Complete {
            task: id.clone(),
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &id).output,
        Some(json!({"value":15}))
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reconciling_an_old_revision_preserves_receipt_without_updating_new_inputs() {
    let mut runtime = runtime(Arc::new(AtomicUsize::new(0)), true, true);
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 55, Some("old-charge"));
    let operation = task(&runtime.snapshot(), &id).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: operation.clone(),
            approve: true,
        })
        .unwrap()
        .unwrap();
    runtime.complete(invocation.execute().await);
    runtime
        .command(TaskCommand::Revise {
            task: id.clone(),
            expected_revision: 1,
            input: json!({"value":9}),
        })
        .unwrap();
    runtime
        .command(TaskCommand::Reconcile {
            operation,
            outcome: ReconciledOutcome::Succeeded {
                result: json!({"value":55}),
            },
        })
        .unwrap();
    runtime
        .command(TaskCommand::Complete {
            task: id.clone(),
            output: Value::Null,
        })
        .unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &id).output,
        Some(json!({"value":9}))
    );
    assert_eq!(
        runtime.snapshot().operations[0].result,
        Some(json!({"value":55}))
    );
}

#[tokio::test]
async fn receipt_projection_failure_never_turns_verified_success_into_service_failure() {
    let mut definition = skill(Arc::new(AtomicUsize::new(0)), true, true);
    definition.receipt_applier = Some(Arc::new(|_, _, _| Err("response cannot be mapped".into())));
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 55, Some("charge"));
    let operation = task(&runtime.snapshot(), &id).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation: operation.clone(),
            approve: true,
        })
        .unwrap()
        .unwrap();
    runtime.complete(invocation.execute().await);
    assert!(
        runtime
            .command(TaskCommand::Reconcile {
                operation,
                outcome: ReconciledOutcome::Succeeded {
                    result: json!({"value":55})
                }
            })
            .is_err()
    );
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.operations[0].status, OperationStatus::Succeeded);
    assert_eq!(snapshot.operations[0].result, Some(json!({"value":55})));
    assert!(
        snapshot.operations[0]
            .error
            .as_ref()
            .unwrap()
            .contains("state reconstruction")
    );
    assert_eq!(task(&snapshot, &id).status, TaskStatus::Failed);
}

#[test]
fn old_turns_advance_their_original_task_and_ignore_superseded_revisions() {
    let mut definition = skill(Arc::new(AtomicUsize::new(0)), false, false);
    definition.flow_factory = Some(Arc::new(|| {
        let flow = Flow::new()
            .step("wait")
            .done(Guard::is_true("repair:wait:reprompt"))
            .step("done")
            .after("wait")
            .terminal()
            .require(["done"])
            .build()
            .unwrap()
            .compile()
            .unwrap();
        Ok(crate::flow::FlowStack::new(flow, Enforcement::Enforce)
            .with_repair("wait", crate::flow::RepairPolicy::new(1, 2)))
    }));
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let first = start(&mut runtime, 0, None);
    let second = start(&mut runtime, 0, None);
    runtime.on_turn_for(&first, 1);
    assert!(
        task(&runtime.snapshot(), &first)
            .flow
            .as_ref()
            .unwrap()
            .complete
    );
    assert!(
        !task(&runtime.snapshot(), &second)
            .flow
            .as_ref()
            .unwrap()
            .complete
    );
    runtime
        .command(TaskCommand::Revise {
            task: first.clone(),
            expected_revision: 1,
            input: json!({"value":0}),
        })
        .unwrap();
    runtime.on_turn_for(&first, 1);
    runtime.on_interrupted_for(&first, 1);
    assert_eq!(runtime.snapshot().foreground, Some(second));
    assert_eq!(task(&runtime.snapshot(), &first).revision, 2);
    assert!(
        !task(&runtime.snapshot(), &first)
            .flow
            .as_ref()
            .unwrap()
            .complete
    );
}

#[tokio::test]
async fn an_unknown_commit_receipt_is_observed_only_once() {
    let mut runtime = runtime(Arc::new(AtomicUsize::new(0)), true, true);
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 11, Some("ambiguous-charge"));
    let operation = task(&runtime.snapshot(), &id).pending.clone().unwrap();
    let invocation = runtime
        .command(TaskCommand::Decide {
            operation,
            approve: true,
        })
        .unwrap()
        .unwrap();
    let completion = invocation.execute().await;
    assert!(!runtime.complete(completion.clone()).stale);
    assert!(runtime.complete(completion).stale);
    assert_eq!(
        runtime.snapshot().operations[0].status,
        OperationStatus::Unknown
    );
    assert!(task(&runtime.snapshot(), &id).pending.is_some());
}

#[test]
fn omitted_start_input_and_projected_completion_output_default_to_empty_objects() {
    let mut runtime = runtime(Arc::new(AtomicUsize::new(0)), false, false);
    let start: TaskCommand =
        serde_json::from_value(json!({"action":"start","skill":"support"})).unwrap();
    assert!(matches!(&start, TaskCommand::Start { input, .. } if input == &json!({})));
    runtime.command(start).unwrap();
    let id = runtime.snapshot().foreground.unwrap();
    let complete: TaskCommand =
        serde_json::from_value(json!({"action":"complete","task":id})).unwrap();
    assert!(matches!(&complete, TaskCommand::Complete { output, .. } if output == &json!({})));
    runtime.command(complete).unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &id).output,
        Some(json!({"value":0}))
    );
}

#[test]
fn omitted_generic_output_still_must_pass_its_contract_and_explicit_null_input_is_rejected() {
    let mut definition = skill(Arc::new(AtomicUsize::new(0)), false, false);
    definition.project_output = None;
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let null_input: TaskCommand =
        serde_json::from_value(json!({"action":"start","skill":"support","input":null})).unwrap();
    assert!(runtime.command(null_input).is_err());
    let id = start(&mut runtime, 0, None);
    let missing_output: TaskCommand =
        serde_json::from_value(json!({"action":"complete","task":id})).unwrap();
    assert!(runtime.command(missing_output).is_err());
    assert_eq!(task(&runtime.snapshot(), &id).status, TaskStatus::Running);
    runtime
        .command(TaskCommand::Complete {
            task: id.clone(),
            output: json!({"value":4}),
        })
        .unwrap();
    assert_eq!(
        task(&runtime.snapshot(), &id).output,
        Some(json!({"value":4}))
    );
}

// Services use controlled provider outputs while the real task reducer applies
// promotions, computed values, watcher effects and lifecycle fencing.
struct FactsExtractor {
    promotions: Vec<crate::live::extractor::FieldPromotion>,
}
#[async_trait::async_trait]
impl crate::live::extractor::TurnExtractor for FactsExtractor {
    fn name(&self) -> &str {
        "facts"
    }
    fn window_size(&self) -> usize {
        4
    }
    fn promotion_rules(&self) -> &[crate::live::extractor::FieldPromotion] {
        &self.promotions
    }
    async fn extract(
        &self,
        _window: &[crate::live::transcript::TranscriptTurn],
    ) -> Result<Value, crate::llm::LlmError> {
        panic!("service regression tests must bind controlled extraction")
    }
}
fn service_skill(commit: bool, fail: bool) -> CompiledSkill {
    let mut definition = skill(Arc::new(AtomicUsize::new(0)), commit, fail);
    definition.project_output = Some(Arc::new(|state| {
        Ok(json!({
            "value":state.get_raw("value"), "flag":state.get_raw("flag"),
            "known":state.get_raw("known"), "derived":state.get_raw("derived:large"),
            "watch":state.get_raw("watched"), "context_owner":state.get_raw("context_owner")
        }))
    }));
    definition
        .initial_state
        .insert("known".into(), json!("original"));
    definition.services = Some(Arc::new(|| {
        use crate::live::{
            computed::ComputedVar, extractor::FieldPromotion, watcher::WatchPredicate,
        };
        let mut services = TaskServices::default();
        services.extractors.push(Arc::new(FactsExtractor {
            promotions: vec![
                FieldPromotion::overwrite("value"),
                FieldPromotion::true_only("flag"),
                FieldPromotion::keep_known("known"),
            ],
        }));
        services
            .computed
            .register(ComputedVar {
                key: "large".into(),
                dependencies: vec!["value".into()],
                compute: Arc::new(|state| {
                    Some(json!(state.get::<i64>("value").unwrap_or(0) >= 10))
                }),
            })
            .unwrap();
        services.watchers.push(TaskWatcher {
            key: "large".into(),
            predicate: WatchPredicate::BecameTrue,
            effects: Arc::new(|_| {
                vec![
                    TaskEffect::Set {
                        key: "watched".into(),
                        value: json!(true),
                    },
                    TaskEffect::Context("Large amount policy applies".into()),
                ]
            }),
        });
        Ok(services)
    }));
    definition
}
fn observe_turn(runtime: &mut TaskRuntime, id: &TaskId, user: &str) {
    runtime
        .observe(
            id,
            1,
            TaskObservation::Turn {
                user: user.into(),
                model: "Understood".into(),
            },
        )
        .unwrap();
}
async fn finish_facts(runtime: &mut TaskRuntime, value: Value) {
    let mut work = runtime.take_ready_services();
    assert_eq!(work.len(), 1);
    assert!(
        runtime.complete_services(
            work.remove(0)
                .execute_controlled(std::collections::BTreeMap::from([(
                    "facts".into(),
                    Ok(value)
                )]))
                .await
        )
    );
}
fn complete_output(runtime: &mut TaskRuntime, id: &TaskId) -> Value {
    runtime
        .command(TaskCommand::Complete {
            task: id.clone(),
            output: json!({}),
        })
        .unwrap();
    task(&runtime.snapshot(), id).output.clone().unwrap()
}
#[tokio::test]
async fn extraction_reuses_promotions_and_stabilizes_computed_watchers() {
    let mut runtime = TaskRuntime::new(vec![service_skill(false, false)]).unwrap();
    let id = start(&mut runtime, 0, None);
    observe_turn(&mut runtime, &id, "A large amount");
    finish_facts(
        &mut runtime,
        json!({"value":20,"flag":true,"known":"overwrite"}),
    )
    .await;
    let effects = runtime.take_service_effects();
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e.effect, TaskEffect::Context(_)))
            .count(),
        1
    );
    observe_turn(&mut runtime, &id, "Still a large amount");
    finish_facts(&mut runtime, json!({"value":25,"flag":false,"known":null})).await;
    assert!(runtime.take_service_effects().is_empty());
    let output = complete_output(&mut runtime, &id);
    assert_eq!(output["value"], 25);
    assert_eq!(output["flag"], true);
    assert_eq!(output["known"], "original");
    assert_eq!(output["derived"], true);
    assert_eq!(output["watch"], true);
}
#[tokio::test]
async fn services_wait_for_tool_snapshot_and_preserve_both_writes() {
    let mut runtime = TaskRuntime::new(vec![service_skill(false, false)]).unwrap();
    let id = start(&mut runtime, 0, None);
    let invocation = invoke(&mut runtime, &id, 7, None).unwrap();
    observe_turn(&mut runtime, &id, "Extract twenty after this tool");
    assert!(runtime.take_ready_services().is_empty());
    assert!(task(&runtime.snapshot(), &id).services_pending);
    runtime.complete(invocation.execute().await);
    finish_facts(&mut runtime, json!({"value":20})).await;
    let output = complete_output(&mut runtime, &id);
    assert_eq!(output["value"], 20);
    assert_eq!(output["context_owner"], id.0);
}
#[tokio::test]
async fn accepted_service_facts_revoke_waiting_approval() {
    let mut runtime = TaskRuntime::new(vec![service_skill(true, false)]).unwrap();
    let id = start(&mut runtime, 0, None);
    assert!(invoke(&mut runtime, &id, 100, Some("proposal")).is_none());
    observe_turn(&mut runtime, &id, "Actually use twenty");
    assert!(
        runtime
            .command(TaskCommand::Decide {
                operation: OperationId("operation-1".into()),
                approve: true
            })
            .err()
            .unwrap()
            .0
            .contains("pending services")
    );
    finish_facts(&mut runtime, json!({"value":20})).await;
    assert_eq!(
        runtime.snapshot().operations[0].status,
        OperationStatus::Declined
    );
    assert!(
        runtime
            .command(TaskCommand::Decide {
                operation: OperationId("operation-1".into()),
                approve: true
            })
            .err()
            .unwrap()
            .0
            .contains("no longer pending")
    );
}
#[tokio::test]
async fn cancelled_or_revised_extraction_cannot_write_or_emit_effects() {
    for cancelled in [true, false] {
        let mut runtime = TaskRuntime::new(vec![service_skill(false, false)]).unwrap();
        let id = start(&mut runtime, 0, None);
        observe_turn(&mut runtime, &id, "Old large amount");
        let work = runtime.take_ready_services().remove(0);
        let completion = work
            .execute_controlled(std::collections::BTreeMap::from([(
                "facts".into(),
                Ok(json!({"value":90})),
            )]))
            .await;
        if cancelled {
            runtime
                .command(TaskCommand::Cancel { task: id.clone() })
                .unwrap();
        } else {
            runtime
                .command(TaskCommand::Revise {
                    task: id.clone(),
                    expected_revision: 1,
                    input: json!({"value":3}),
                })
                .unwrap();
        }
        assert!(!runtime.complete_services(completion));
        assert!(runtime.take_service_effects().is_empty());
        if !cancelled {
            assert_eq!(complete_output(&mut runtime, &id)["value"], 3);
        }
    }
}
#[tokio::test]
async fn same_skill_tasks_accept_out_of_order_results_only_into_their_owner() {
    let mut runtime = TaskRuntime::new(vec![service_skill(false, false)]).unwrap();
    let first = start(&mut runtime, 0, None);
    observe_turn(&mut runtime, &first, "First twenty");
    let work_first = runtime.take_ready_services().remove(0);
    let second = start(&mut runtime, 0, Some(first.clone()));
    observe_turn(&mut runtime, &second, "Second five");
    finish_facts(&mut runtime, json!({"value":5})).await;
    assert!(
        runtime.complete_services(
            work_first
                .execute_controlled(std::collections::BTreeMap::from([(
                    "facts".into(),
                    Ok(json!({"value":20}))
                )]))
                .await
        )
    );
    assert!(
        !runtime
            .foreground_instruction()
            .unwrap()
            .contains("Large amount policy applies")
    );
    assert_eq!(complete_output(&mut runtime, &second)["value"], 5);
    assert!(
        runtime
            .foreground_instruction()
            .unwrap()
            .contains("Large amount policy applies")
    );
    assert_eq!(complete_output(&mut runtime, &first)["value"], 20);
}
#[tokio::test]
async fn completed_unknown_commit_allows_facts_but_serializes_reconciliation() {
    let mut runtime = TaskRuntime::new(vec![service_skill(true, true)]).unwrap();
    let id = start(&mut runtime, 0, None);
    invoke(&mut runtime, &id, 7, Some("unknown"));
    let work = runtime
        .command(TaskCommand::Decide {
            operation: OperationId("operation-1".into()),
            approve: true,
        })
        .unwrap()
        .unwrap();
    runtime.complete(work.execute().await);
    observe_turn(
        &mut runtime,
        &id,
        "New safety information while receipt remains unknown",
    );
    let reconcile = TaskCommand::Reconcile {
        operation: OperationId("operation-1".into()),
        outcome: ReconciledOutcome::Succeeded {
            result: json!({"value":7}),
        },
    };
    assert!(
        runtime
            .command(reconcile.clone())
            .err()
            .unwrap()
            .0
            .contains("pending services before reconciliation")
    );
    finish_facts(&mut runtime, json!({"value":20,"flag":true})).await;
    assert!(runtime.foreground_instruction().unwrap().contains("20"));
    assert!(
        runtime
            .command(TaskCommand::Complete {
                task: id.clone(),
                output: json!({})
            })
            .is_err()
    );
    assert!(
        runtime
            .command(TaskCommand::Invoke {
                task: id.clone(),
                tool: "save".into(),
                args: json!({"value":9}),
                idempotency_key: Some("different".into())
            })
            .is_err()
    );
    runtime.command(reconcile).unwrap();
    let output = complete_output(&mut runtime, &id);
    assert_eq!(output["value"], 7);
    assert_eq!(output["flag"], true);
}

#[tokio::test]
async fn missing_controlled_extraction_is_an_observable_owned_error() {
    let mut runtime = TaskRuntime::new(vec![service_skill(false, false)]).unwrap();
    let id = start(&mut runtime, 0, None);
    observe_turn(&mut runtime, &id, "Something");
    let work = runtime.take_ready_services().remove(0);
    assert!(runtime.complete_services(work.execute_controlled(Default::default()).await));
    assert!(
        task(&runtime.snapshot(), &id).service_errors[0].contains("missing controlled extraction")
    );
    let effects = runtime.take_service_effects();
    assert!(
        effects
            .iter()
            .any(|effect| effect.owner.task == id && matches!(effect.effect, TaskEffect::Error(_)))
    );
}
#[test]
fn task_temporal_counters_and_active_time_do_not_leak_across_suspension() {
    use crate::live::temporal::{SustainedDetector, TurnCountDetector};
    let clock = Arc::new(crate::clock::ManualClock::new());
    let mut definition = service_skill(false, false);
    definition.services = Some(Arc::new(|| {
        Ok(TaskServices {
            patterns: vec![
                TaskPattern {
                    name: "two turns".into(),
                    detector: Box::new(TurnCountDetector::new(Arc::new(|_| true), 2)),
                    cooldown: None,
                    effects: Arc::new(|_| vec![TaskEffect::Prompt("two".into())]),
                },
                TaskPattern {
                    name: "five seconds".into(),
                    detector: Box::new(SustainedDetector::new(
                        Arc::new(|_| true),
                        std::time::Duration::from_secs(5),
                    )),
                    cooldown: None,
                    effects: Arc::new(|_| vec![TaskEffect::Context("five".into())]),
                },
            ],
            ..Default::default()
        })
    }));
    let mut runtime = TaskRuntime::new(vec![definition])
        .unwrap()
        .with_clock(clock.clone());
    let first = start(&mut runtime, 0, None);
    observe_turn(&mut runtime, &first, "One");
    runtime.take_service_effects();
    let second = start(&mut runtime, 0, Some(first.clone()));
    observe_turn(&mut runtime, &second, "Child one");
    clock.advance(std::time::Duration::from_secs(30));
    runtime
        .command(TaskCommand::Complete {
            task: second,
            output: json!({}),
        })
        .unwrap();
    runtime.take_service_effects();
    runtime.observe(&first, 1, TaskObservation::Timer).unwrap();
    assert!(runtime.take_service_effects().is_empty());
    observe_turn(&mut runtime, &first, "Two");
    assert!(
        runtime
            .take_service_effects()
            .iter()
            .any(|e| e.owner.task == first
                && matches!(&e.effect,TaskEffect::Prompt(text) if text=="two"))
    );
    clock.advance(std::time::Duration::from_secs(5));
    runtime.observe(&first, 1, TaskObservation::Timer).unwrap();
    assert!(
        runtime
            .take_service_effects()
            .iter()
            .any(|e| e.owner.task == first
                && matches!(&e.effect,TaskEffect::Context(text) if text=="five"))
    );
}

#[tokio::test]
async fn unchanged_service_facts_do_not_revoke_waiting_approval() {
    let mut definition = service_skill(true, false);
    let flow: Flow = serde_json::from_value(
        json!({"steps":[{"id":"save","allow":["save"],"done":{"called_ok":"save"}}]}),
    )
    .unwrap();
    definition.flow_factory = Some(Arc::new(move || {
        Ok(
            crate::flow::FlowStack::new(flow.clone().compile().unwrap(), Enforcement::Enforce)
                .with_repair("save", crate::flow::RepairPolicy::new(1, 4)),
        )
    }));
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let id = start(&mut runtime, 0, None);
    observe_turn(&mut runtime, &id, "Value remains zero");
    finish_facts(&mut runtime, json!({"value":0})).await;
    invoke(&mut runtime, &id, 100, Some("proposal"));
    observe_turn(&mut runtime, &id, "Still zero");
    finish_facts(&mut runtime, json!({"value":0})).await;
    assert_eq!(
        runtime.snapshot().operations[0].status,
        OperationStatus::AwaitingApproval
    );
}
struct ServiceMemoryProbe {
    ingested: Arc<AtomicUsize>,
    remembered: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl TaskMemoryService for ServiceMemoryProbe {
    async fn project(&self) -> Result<Value, String> {
        Ok(json!({"known":"memory value","remembered":"yes"}))
    }
    async fn ingest(&self, _effect_id: String, _turn: u64, _user: String) -> Result<Value, String> {
        self.ingested.fetch_add(1, Ordering::SeqCst);
        self.project().await
    }
    async fn remember(
        &self,
        _effect_id: String,
        _turn: u64,
        _note: String,
    ) -> Result<Value, String> {
        self.remembered.fetch_add(1, Ordering::SeqCst);
        self.project().await
    }
}
#[tokio::test]
async fn task_memory_keeps_captured_fields_and_runs_only_after_accepted_turns() {
    let ingested = Arc::new(AtomicUsize::new(0));
    let remembered = Arc::new(AtomicUsize::new(0));
    let memory = Arc::new(ServiceMemoryProbe {
        ingested: ingested.clone(),
        remembered: remembered.clone(),
    });
    let mut definition = service_skill(false, false);
    let factory = definition.services.take().unwrap();
    definition.services = Some(Arc::new(move || {
        let mut services = factory()?;
        services.memory = Some(memory.clone());
        services.watchers.push(TaskWatcher {
            key: "value".into(),
            predicate: crate::live::watcher::WatchPredicate::Changed,
            effects: Arc::new(|_| vec![TaskEffect::Remember("accepted value".into())]),
        });
        Ok(services)
    }));
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let id = start(&mut runtime, 0, None);
    let initial = runtime.take_ready_services().remove(0);
    assert!(runtime.complete_services(initial.execute().await));
    assert!(
        runtime
            .foreground_instruction()
            .unwrap()
            .contains("remembered")
    );
    assert!(
        runtime
            .foreground_instruction()
            .unwrap()
            .contains("original")
    );
    assert!(
        !runtime
            .foreground_instruction()
            .unwrap()
            .contains("memory value")
    );
    assert_eq!(ingested.load(Ordering::SeqCst), 0);
    assert_eq!(remembered.load(Ordering::SeqCst), 0);
    observe_turn(&mut runtime, &id, "Value twenty");
    finish_facts(&mut runtime, json!({"value":20})).await;
    assert_eq!(ingested.load(Ordering::SeqCst), 0);
    assert_eq!(remembered.load(Ordering::SeqCst), 0);
    while let Some(work) = runtime.take_ready_services().pop() {
        assert!(runtime.complete_services(work.execute().await));
    }
    assert_eq!(ingested.load(Ordering::SeqCst), 1);
    assert_eq!(remembered.load(Ordering::SeqCst), 1);
    observe_turn(&mut runtime, &id, "Superseded value thirty");
    let work = runtime.take_ready_services().remove(0);
    let completion = work
        .execute_controlled(std::collections::BTreeMap::from([(
            "facts".into(),
            Ok(json!({"value":30})),
        )]))
        .await;
    runtime
        .command(TaskCommand::Revise {
            task: id.clone(),
            expected_revision: 1,
            input: json!({"value":3}),
        })
        .unwrap();
    assert!(!runtime.complete_services(completion));
    let project = runtime.take_ready_services().remove(0);
    runtime.complete_services(project.execute().await);
    assert_eq!(complete_output(&mut runtime, &id)["known"], "original");
    assert_eq!(ingested.load(Ordering::SeqCst), 1);
    assert_eq!(remembered.load(Ordering::SeqCst), 1);
}

#[test]
fn memory_ambient_tools_preserve_explicit_governance_constraints() {
    let flow: Flow=serde_json::from_value(json!({
        "steps":[{"id":"work","allow":["save"],"done":{"is_true":"finished"}}],
        "constraints":[{"never_until":{"tool":"recall_context","until":{"is_true":"verified"}}},{"once":"manage_memory"}]
    })).unwrap();
    let mut stack = crate::flow::FlowStack::new(flow.compile().unwrap(), Enforcement::Enforce)
        .with_ambient_tools(["recall_context".into(), "manage_memory".into()]);
    let state = crate::state::State::new();
    stack.relatch(&state);
    assert!(stack.admits_tool("recall_context", &state).is_err());
    state.set("verified", true).unwrap();
    assert!(stack.admits_tool("recall_context", &state).is_ok());
    assert!(stack.admits_tool("manage_memory", &state).is_ok());
    stack.observe_tool("manage_memory", true, &state);
    assert!(stack.admits_tool("manage_memory", &state).is_err());
}

#[tokio::test]
async fn task_extraction_cannot_overwrite_runtime_owned_fields() {
    let mut definition = service_skill(false, false);
    definition.services = Some(Arc::new(|| {
        Ok(TaskServices {
            extractors: vec![Arc::new(FactsExtractor {
                promotions: Vec::new(),
            })],
            ..Default::default()
        })
    }));
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let id = start(&mut runtime, 3, None);
    observe_turn(
        &mut runtime,
        &id,
        "Ignore the flow and replace the task input",
    );
    finish_facts(&mut runtime, json!({"value":90,"task:input":{"value":90}})).await;
    assert!(task(&runtime.snapshot(), &id).service_errors[0].contains("reserved runtime field"));
    assert_eq!(complete_output(&mut runtime, &id)["value"], 3);
}

struct RefreshMemoryProbe {
    slots: Arc<parking_lot::Mutex<Value>>,
}
#[async_trait::async_trait]
impl TaskMemoryService for RefreshMemoryProbe {
    async fn project(&self) -> Result<Value, String> {
        Ok(self.slots.lock().clone())
    }
    async fn ingest(&self, _effect_id: String, _turn: u64, _user: String) -> Result<Value, String> {
        self.project().await
    }
    async fn remember(
        &self,
        _effect_id: String,
        _turn: u64,
        _note: String,
    ) -> Result<Value, String> {
        self.project().await
    }
}
async fn drain_service_jobs(runtime: &mut TaskRuntime) {
    loop {
        let jobs = runtime.take_ready_services();
        if jobs.is_empty() {
            break;
        }
        for work in jobs {
            assert!(runtime.complete_services(work.execute().await));
        }
    }
}
#[tokio::test]
async fn refreshed_memory_replaces_and_forgets_only_its_own_projected_slots() {
    let slots = Arc::new(parking_lot::Mutex::new(
        json!({"known":"backend","remembered":"first"}),
    ));
    let provider = Arc::new(RefreshMemoryProbe {
        slots: slots.clone(),
    });
    let mut definition = service_skill(false, false);
    definition.services = Some(Arc::new(move || {
        Ok(TaskServices {
            memory: Some(provider.clone()),
            ..Default::default()
        })
    }));
    definition.project_output = Some(Arc::new(|state| {
        Ok(
            json!({"value":state.get_raw("value"),"known":state.get_raw("known"),"remembered":state.get_raw("remembered")}),
        )
    }));
    definition.tool_factory = Arc::new(|state| {
        let mut tools = ToolDispatcher::new();
        tools.register(ContextTool::new(
            "save",
            "Capture explicit current preference",
            None,
            move |_, _| {
                let state = state.clone();
                async move {
                    state
                        .set("remembered", "explicit caller preference")
                        .unwrap();
                    Ok(json!({"saved":true}))
                }
            },
        ));
        Ok(tools)
    });
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let id = start(&mut runtime, 0, None);
    drain_service_jobs(&mut runtime).await;
    assert!(runtime.foreground_instruction().unwrap().contains("first"));
    *slots.lock() = json!({"known":"changed backend","remembered":"second"});
    observe_turn(&mut runtime, &id, "Refresh preferences");
    drain_service_jobs(&mut runtime).await;
    assert!(runtime.foreground_instruction().unwrap().contains("second"));
    assert!(
        !runtime
            .foreground_instruction()
            .unwrap()
            .contains("changed backend")
    );
    *slots.lock() = json!({"known":"backend"});
    observe_turn(&mut runtime, &id, "Forget the stored preference");
    drain_service_jobs(&mut runtime).await;
    assert!(!runtime.foreground_instruction().unwrap().contains("second"));
    *slots.lock() = json!({"remembered":"third"});
    observe_turn(&mut runtime, &id, "Remember a new preference");
    drain_service_jobs(&mut runtime).await;
    let invocation = invoke(&mut runtime, &id, 0, None).unwrap();
    runtime.complete(invocation.execute().await);
    drain_service_jobs(&mut runtime).await;
    *slots.lock() = json!({});
    observe_turn(&mut runtime, &id, "Forget stored information again");
    drain_service_jobs(&mut runtime).await;
    let output = complete_output(&mut runtime, &id);
    assert_eq!(output["known"], "original");
    assert_eq!(output["remembered"], "explicit caller preference");
}

struct ResumeMemoryProbe {
    slots: Arc<parking_lot::Mutex<Value>>,
    ingestions: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl TaskMemoryService for ResumeMemoryProbe {
    async fn project(&self) -> Result<Value, String> {
        Ok(self.slots.lock().clone())
    }
    async fn ingest(&self, _effect_id: String, _turn: u64, _user: String) -> Result<Value, String> {
        self.ingestions.fetch_add(1, Ordering::SeqCst);
        self.project().await
    }
    async fn remember(
        &self,
        _effect_id: String,
        _turn: u64,
        _note: String,
    ) -> Result<Value, String> {
        panic!("resume must not remember anything")
    }
}
#[tokio::test]
async fn resuming_refreshes_parent_memory_after_child_update_and_forget() {
    let slots = Arc::new(parking_lot::Mutex::new(
        json!({"known":"backend", "remembered":"OLD-PREFERENCE"}),
    ));
    let ingestions = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(ResumeMemoryProbe {
        slots: slots.clone(),
        ingestions: ingestions.clone(),
    });
    let mut parent_definition = service_skill(true, false);
    parent_definition.services = Some(Arc::new(move || {
        let mut services = TaskServices {
            memory: Some(provider.clone()),
            ..Default::default()
        };
        services
            .computed
            .register(crate::live::computed::ComputedVar {
                key: "preference_description".into(),
                dependencies: vec!["remembered".into()],
                compute: Arc::new(|state| {
                    Some(state.get_raw("remembered").unwrap_or(json!("unavailable")))
                }),
            })
            .unwrap();
        Ok(services)
    }));
    parent_definition.project_output = Some(Arc::new(|state| {
        Ok(
            json!({"value":state.get_raw("value"),"known":state.get_raw("known"),"remembered":state.get_raw("remembered")}),
        )
    }));
    let mut child_definition = skill(Arc::new(AtomicUsize::new(0)), true, false);
    child_definition.name = "memory_editor".into();
    child_definition.tool_factory = Arc::new(move |state| {
        let mut dispatcher = ToolDispatcher::new();
        let slots = slots.clone();
        dispatcher.register(ContextTool::new(
            "save",
            "Change controlled shared memory",
            None,
            move |args, _| {
                let slots = slots.clone();
                let state = state.clone();
                async move {
                    let value = args.get("value").and_then(Value::as_i64).unwrap();
                    *slots.lock() = if value == 1 {
                        json!({"known":"backend changed", "remembered":"NEW-PREFERENCE"})
                    } else {
                        json!({})
                    };
                    state.set("value", value).unwrap();
                    Ok(json!({"value":value}))
                }
            },
        ));
        Ok(dispatcher)
    });
    let mut runtime = TaskRuntime::new(vec![parent_definition, child_definition]).unwrap();
    let parent = start(&mut runtime, 0, None);
    drain_service_jobs(&mut runtime).await;
    assert!(
        runtime
            .foreground_instruction()
            .unwrap()
            .contains("OLD-PREFERENCE")
    );
    invoke(&mut runtime, &parent, 9, Some("parent proposal"));
    for (value, automatic) in [(1, true), (2, false)] {
        runtime
            .command(TaskCommand::Start {
                skill: "memory_editor".into(),
                input: json!({"value":0}),
                parent: Some(parent.clone()),
            })
            .unwrap();
        let child = runtime.snapshot().foreground.unwrap();
        invoke(
            &mut runtime,
            &child,
            value,
            Some(if value == 1 { "update" } else { "forget" }),
        );
        let operation = task(&runtime.snapshot(), &child).pending.clone().unwrap();
        let work = runtime
            .command(TaskCommand::Decide {
                operation,
                approve: true,
            })
            .unwrap()
            .unwrap();
        runtime.complete(work.execute().await);
        if automatic {
            complete_output(&mut runtime, &child);
        } else {
            runtime
                .command(TaskCommand::Resume {
                    task: parent.clone(),
                })
                .unwrap();
        }
        assert!(task(&runtime.snapshot(), &parent).services_pending);
        let instruction = runtime.foreground_instruction().unwrap();
        assert!(!instruction.contains("OLD-PREFERENCE"));
        assert!(!instruction.contains("NEW-PREFERENCE"));
        assert!(instruction.contains("original"));
        assert!(
            runtime
                .command(TaskCommand::Complete {
                    task: parent.clone(),
                    output: json!({})
                })
                .err()
                .unwrap()
                .0
                .contains("pending services")
        );
        assert!(
            runtime
                .command(TaskCommand::Invoke {
                    task: parent.clone(),
                    tool: "save".into(),
                    args: json!({"value":9}),
                    idempotency_key: Some("new proposal".into())
                })
                .err()
                .unwrap()
                .0
                .contains("pending services")
        );
        assert!(
            runtime
                .command(TaskCommand::Decide {
                    operation: OperationId("operation-1".into()),
                    approve: true
                })
                .is_err()
        );
        drain_service_jobs(&mut runtime).await;
        let instruction = runtime.foreground_instruction().unwrap();
        assert!(!instruction.contains("OLD-PREFERENCE"));
        assert_eq!(instruction.contains("NEW-PREFERENCE"), value == 1);
        assert!(instruction.contains("original"));
        assert!(runtime.take_service_effects().is_empty());
    }
    assert_eq!(ingestions.load(Ordering::SeqCst), 0);
    let output = complete_output(&mut runtime, &parent);
    assert_eq!(output["remembered"], Value::Null);
    assert_eq!(output["known"], "original");
}

#[tokio::test]
async fn historical_memory_receipts_remain_observable_without_republishing_forgotten_facts() {
    let forgotten = "OLD-MEMORY-FACT";
    let slots = Arc::new(parking_lot::Mutex::new(json!({"remembered":forgotten})));
    let provider = Arc::new(RefreshMemoryProbe {
        slots: slots.clone(),
    });
    let mut definition = service_skill(false, false);
    definition.tools[0].name = "recall_context".into();
    definition.tool_factory = Arc::new(move |_| {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register(ContextTool::new(
            "recall_context",
            "Read controlled remembered facts",
            None,
            move |_, _| async move { Ok(json!({"facts":[forgotten]})) },
        ));
        Ok(dispatcher)
    });
    definition.services = Some(Arc::new(move || {
        Ok(TaskServices {
            memory: Some(provider.clone()),
            ..Default::default()
        })
    }));
    let mut runtime = TaskRuntime::new(vec![definition]).unwrap();
    let id = start(&mut runtime, 0, None);
    drain_service_jobs(&mut runtime).await;
    let invocation = runtime
        .command(TaskCommand::Invoke {
            task: id.clone(),
            tool: "recall_context".into(),
            args: json!({}),
            idempotency_key: None,
        })
        .unwrap()
        .unwrap();
    let delivery = runtime.complete(invocation.execute().await);
    assert_eq!(delivery.result.unwrap(), json!({"facts":[forgotten]}));
    drain_service_jobs(&mut runtime).await;
    assert!(
        runtime
            .foreground_instruction()
            .unwrap()
            .contains(forgotten)
    );
    runtime
        .command(TaskCommand::Suspend { task: id.clone() })
        .unwrap();
    *slots.lock() = json!({});
    runtime.command(TaskCommand::Resume { task: id }).unwrap();
    assert!(
        !runtime
            .foreground_instruction()
            .unwrap()
            .contains(forgotten)
    );
    drain_service_jobs(&mut runtime).await;
    assert!(
        !runtime
            .foreground_instruction()
            .unwrap()
            .contains(forgotten)
    );
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.operations[0].status, OperationStatus::Succeeded);
    assert_eq!(
        snapshot.operations[0].result,
        Some(json!({"facts":[forgotten]}))
    );
}
