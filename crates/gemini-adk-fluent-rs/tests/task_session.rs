//! Task ownership through a real Live session with only the provider transport scripted.

use std::{sync::Arc, time::Duration};

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::spec::{SessionSpec, SpecResources};
use gemini_adk_fluent_rs::tasks::{OperationStatus, TaskCommand, TaskId, TaskStatus};
use gemini_adk_fluent_rs::testing::ScriptedServer;
use gemini_adk_fluent_rs::tools::ContextTool;
use serde_json::{Value, json};
use tokio::sync::Notify;

fn support_spec() -> SessionSpec {
    SessionSpec::from_value(json!({
        "name":"task-session-test",
        "skills":[
            {"name":"billing","version":"1.0","instruction":"Help with this invoice.",
             "inputs":{"invoice":{"type":"string"}},
             "outputs":{"receipt":{"type":"object"}},
             "tools":[{"name":"lookup","description":"Read invoice","effect":{"kind":"read"},"save_response_as":"receipt"},
                       {"name":"adjust","description":"Submit adjustment","effect":{"kind":"commit","idempotency_argument":"request_id"},"save_response_as":"receipt"}]},
            {"name":"faq","version":"1.0","instruction":"Answer the policy question.","tools":[]}
        ]
    })).unwrap()
}

async fn wait_for_operation(handle: &LiveHandle, status: OperationStatus) {
    let mut events = handle.events();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if handle
                .task_snapshot()
                .unwrap()
                .operations
                .iter()
                .any(|op| op.status == status)
            {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("operation settled on the control lane");
}

#[tokio::test]
async fn task_catalog_and_qualified_tools_reach_the_provider_setup() {
    let live = support_spec()
        .apply(Live::builder(), &State::new(), &SpecResources::default())
        .unwrap();
    let run = ScriptedServer::new()
        .says("Hello.")
        .play(live)
        .await
        .unwrap();
    let setup = run.setup().expect("provider setup");
    let declarations = setup["tools"][0]["functionDeclarations"]
        .as_array()
        .unwrap();
    let names: Vec<_> = declarations
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "task_control",
            "start_billing",
            "start_faq",
            "billing__lookup",
            "billing__adjust"
        ]
    );
    // Each skill's entry point takes its inputs as typed parameters.
    let start = &declarations[1]["parameters"];
    assert_eq!(start["properties"]["invoice"]["type"], "string");
    assert_eq!(start["required"], json!(["invoice"]));
    assert!(start.get("additionalProperties").is_none());
    assert_eq!(declarations[3]["behavior"], "NON_BLOCKING");
    assert!(
        declarations[4]["description"]
            .as_str()
            .unwrap()
            .contains("creates an approval proposal")
    );
    assert_eq!(
        declarations[4]["parameters"]["properties"]["request_id"]["type"],
        "string"
    );
    let instruction = setup["systemInstruction"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(instruction.contains("Installed skills:"));
    assert!(instruction.contains("billing") && instruction.contains("faq"));
    assert!(instruction.contains("never approve your own action"));
    run.disconnect().await;
}

#[tokio::test]
async fn a_late_result_and_speech_interruption_preserve_the_original_task() {
    let entered = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let tool_entered = entered.clone();
    let tool_finish = finish.clone();
    let resources = SpecResources::default().implement(ContextTool::new(
        "lookup", "Controlled invoice lookup", None,
        move |_args, ctx| {
            let entered = tool_entered.clone();
            let finish = tool_finish.clone();
            async move {
                entered.notify_one();
                finish.notified().await;
                Ok(json!({"invoice":ctx.state.get::<String>("invoice").unwrap(),"owner":ctx.task.unwrap().task}))
            }
        },
    ));
    let live = support_spec()
        .apply(Live::builder(), &State::new(), &resources)
        .unwrap();
    let (transport, control) = ScriptedServer::new()
        .calls("billing__lookup", json!({}))
        .interrupts()
        .into_transport();
    let handle = live.connect_with_transport(transport).await.unwrap();
    handle
        .task_command(TaskCommand::Start {
            skill: "billing".into(),
            input: json!({"invoice":"INV-42"}),
            parent: None,
        })
        .await
        .unwrap();
    control.release();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    // This must complete while the tool is waiting: long work never occupies the lane.
    tokio::time::timeout(
        Duration::from_secs(1),
        handle.task_command(TaskCommand::Start {
            skill: "faq".into(),
            input: json!({}),
            parent: Some(TaskId("task-1".into())),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    finish.notify_one();
    wait_for_operation(&handle, OperationStatus::Succeeded).await;
    let snapshot = handle.task_snapshot().unwrap();
    assert_eq!(snapshot.foreground, Some(TaskId("task-2".into())));
    assert_eq!(snapshot.tasks[0].status, TaskStatus::Suspended);
    assert_eq!(
        snapshot.operations[0].result.as_ref().unwrap()["invoice"],
        "INV-42"
    );
    assert_eq!(snapshot.operations[0].owner.task, TaskId("task-1".into()));
    let frames: Vec<Value> = control
        .outbound_frames()
        .iter()
        .map(|f| serde_json::from_slice(f).unwrap())
        .collect();
    let receipt = frames
        .iter()
        .filter_map(|f| f.pointer("/toolResponse/functionResponses"))
        .filter_map(Value::as_array)
        .flatten()
        .find(|r| r["response"]["status"] == "stored_for_task")
        .expect("quiet completion receipt");
    assert_eq!(receipt["scheduling"], "SILENT");
    assert!(receipt["response"].get("result").is_none());
    handle
        .task_command(TaskCommand::Complete {
            task: TaskId("task-2".into()),
            output: json!({}),
        })
        .await
        .unwrap();
    let done = handle
        .task_command(TaskCommand::Complete {
            task: TaskId("task-1".into()),
            output: json!({}),
        })
        .await
        .unwrap();
    assert_eq!(
        done.tasks[0].output.as_ref().unwrap()["receipt"]["invoice"],
        "INV-42"
    );
    handle.disconnect().await.unwrap();
}

#[tokio::test]
async fn model_requests_cannot_approve_their_own_effects() {
    let live = support_spec()
        .apply(Live::builder(), &State::new(), &SpecResources::default())
        .unwrap();
    let run = ScriptedServer::new()
        .calls(
            "task_control",
            json!({"action":"start","skill":"billing","input":{"invoice":"INV-42"}}),
        )
        .calls(
            "billing__adjust",
            json!({"request_id":"adjust-42","amount":10}),
        )
        .calls(
            "task_control",
            json!({"action":"decide","operation":"operation-1","approve":true}),
        )
        .play(live)
        .await
        .unwrap();
    let status = run.handle().task_snapshot().unwrap();
    assert_eq!(
        status.operations[0].status,
        OperationStatus::AwaitingApproval
    );
    assert!(run.tool_responses().iter().any(|r| {
        r["response"]["error"]
            .as_str()
            .is_some_and(|s| s.contains("trusted application"))
    }));
    // A revision change revokes the old proposal; a late user click cannot execute it.
    run.handle()
        .task_command(TaskCommand::Revise {
            task: TaskId("task-1".into()),
            expected_revision: 1,
            input: json!({"invoice":"INV-43"}),
        })
        .await
        .unwrap();
    assert!(
        run.handle()
            .task_command(TaskCommand::Decide {
                operation: status.operations[0].id.clone(),
                approve: true
            })
            .await
            .is_err()
    );
    assert_eq!(
        run.handle().task_snapshot().unwrap().operations[0].status,
        OperationStatus::Declined
    );
    run.disconnect().await;
}

#[tokio::test]
async fn task_mode_rejects_a_second_owner_for_governance() {
    let spec = support_spec();
    let runtime = spec.compile_tasks(&SpecResources::default()).unwrap();
    let flow = gemini_adk_fluent_rs::flow::Flow::new()
        .step("hello")
        .terminal()
        .build()
        .unwrap();
    let result = ScriptedServer::new()
        .play(Live::builder().tasks(runtime).govern(flow))
        .await;
    assert!(result.err().unwrap().to_string().contains("session-level"));
}
