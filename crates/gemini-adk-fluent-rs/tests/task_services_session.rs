//! Task services through Live with scripted transport and a controlled extraction model.

use std::{sync::Arc, time::Duration};

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::spec::{SessionSpec, SpecResources};
use gemini_adk_fluent_rs::tasks::{TaskCommand, TaskId};
use gemini_adk_fluent_rs::testing::{LlmRequest, LlmResponse, MockLlm, ScriptedServer};
use gemini_adk_rs::llm::{BaseLlm, LlmError};
use serde_json::json;
use tokio::sync::Notify;

fn service_spec() -> SessionSpec {
    SessionSpec::from_value(json!({
        "name":"owned-services",
        "skills":[
            {"name":"screen","version":"1","instruction":"Screen this request.",
             "state":{"urgent":{"type":"boolean","default":false}},
             "outputs":{"urgent":{"type":"boolean"}},
             "extract":[{"name":"triage","instruction":"Extract urgency.",
                 "schema":{"type":"object","properties":{"urgent":{"type":"boolean"}}},
                 "promote":[{"field":"urgent","policy":"true_only"}]}],
             "watch":[{"key":"urgent","condition":"became_true", "effects":[{"context":"OWNED_URGENCY_CONTEXT"}]}]},
            {"name":"faq","version":"1","instruction":"Answer policy questions."}
        ]
    })).unwrap()
}

struct HeldLlm {
    entered: Arc<Notify>,
    finish: Arc<Notify>,
    model: MockLlm,
}

#[async_trait::async_trait]
impl BaseLlm for HeldLlm {
    fn model_id(&self) -> &str {
        "controlled-extractor"
    }

    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.entered.notify_one();
        self.finish.notified().await;
        self.model.generate(request).await
    }
}

#[tokio::test]
async fn input_before_a_switch_is_extracted_for_its_owner_without_blocking_live_commands() {
    let entered = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let model = MockLlm::text(r#"{"urgent":true}"#);
    let resources = SpecResources {
        extraction_llm: Some(Arc::new(HeldLlm {
            entered: entered.clone(),
            finish: finish.clone(),
            model: model.clone(),
        })),
        ..Default::default()
    };
    let live = service_spec()
        .apply(Live::builder(), &State::new(), &resources)
        .unwrap();
    let (transport, control) = ScriptedServer::new()
        .hears("This is an urgent request about my appointment.")
        .calls(
            "task_control",
            json!({"action":"start","skill":"faq","input":{},"parent":"task-1"}),
        )
        .speaks("Here is our office policy.")
        .turn_complete()
        .into_transport();
    let handle = live.connect_with_transport(transport).await.unwrap();
    handle
        .task_command(TaskCommand::Start {
            skill: "screen".into(),
            input: json!({}),
            parent: None,
        })
        .await
        .unwrap();
    control.release();
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        handle.task_snapshot().unwrap().foreground,
        Some(TaskId("task-2".into()))
    );
    // Exercise the control lane while extraction is waiting on I/O.
    tokio::time::timeout(
        Duration::from_secs(1),
        handle.task_command(TaskCommand::Suspend {
            task: TaskId("task-2".into()),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    finish.notify_one();
    let mut events = handle.events();
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.task_snapshot().unwrap().tasks[0].services_pending {
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    let request = serde_json::to_string(&model.last_request().unwrap().contents).unwrap();
    assert!(request.contains("urgent request"));
    assert!(!request.contains("office policy"));
    assert!(
        !control
            .outbound_frames()
            .iter()
            .any(|frame| String::from_utf8_lossy(frame).contains("OWNED_URGENCY_CONTEXT"))
    );
    handle
        .task_command(TaskCommand::Resume {
            task: TaskId("task-1".into()),
        })
        .await
        .unwrap();
    assert!(
        control
            .outbound_frames()
            .iter()
            .any(|frame| String::from_utf8_lossy(frame).contains("OWNED_URGENCY_CONTEXT"))
    );
    let done = handle
        .task_command(TaskCommand::Complete {
            task: TaskId("task-1".into()),
            output: json!({}),
        })
        .await
        .unwrap();
    assert_eq!(done.tasks[0].output.as_ref().unwrap()["urgent"], true);
    handle.disconnect().await.unwrap();
}

#[tokio::test]
async fn model_routing_claims_unowned_input_for_the_new_task_once() {
    let model = MockLlm::text(r#"{"urgent":true}"#);
    let resources = SpecResources {
        extraction_llm: Some(Arc::new(model.clone())),
        ..Default::default()
    };
    let live = service_spec()
        .apply(Live::builder(), &State::new(), &resources)
        .unwrap();
    let run = ScriptedServer::new()
        .hears("I need help with an urgent request.")
        .calls(
            "task_control",
            json!({"action":"start","skill":"screen","input":{}}),
        )
        .speaks("Let me check the request.")
        .calls(
            "task_control",
            json!({"action":"complete","task":"task-1","output":{}}),
        )
        .turn_complete()
        .play(live)
        .await
        .unwrap();
    let mut events = run.handle().events();
    tokio::time::timeout(Duration::from_secs(5), async {
        while run.handle().task_snapshot().unwrap().tasks[0].services_pending {
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(model.call_count(), 1);
    let done = run.handle().task_snapshot().unwrap();
    assert_eq!(
        done.tasks[0].status,
        gemini_adk_fluent_rs::tasks::TaskStatus::Completed
    );
    assert_eq!(done.tasks[0].output.as_ref().unwrap()["urgent"], true);
    run.disconnect().await;
}

#[tokio::test]
async fn approval_waits_for_unfinalized_input_but_denial_remains_available() {
    let spec = SessionSpec::from_value(json!({
        "name":"approval-input-fence",
        "skills":[{"name":"booking","version":"1", "tools":[{
            "name":"book","description":"Controlled booking",
            "effect":{"kind":"commit","idempotency_argument":"request_id"},
            "response":{"booked":true}
        }]}]
    }))
    .unwrap();
    let live = spec
        .apply(Live::builder(), &State::new(), &SpecResources::default())
        .unwrap();
    let (transport, control) = ScriptedServer::new()
        .hears("Wait, I need to change the request.")
        .into_transport();
    let handle = live.connect_with_transport(transport).await.unwrap();
    handle
        .task_command(TaskCommand::Start {
            skill: "booking".into(),
            input: json!({}),
            parent: None,
        })
        .await
        .unwrap();
    let proposed = handle
        .task_command(TaskCommand::Invoke {
            task: TaskId("task-1".into()),
            tool: "book".into(),
            args: json!({"request_id":"controlled-booking"}),
            idempotency_key: None,
        })
        .await
        .unwrap();
    let operation = proposed.operations[0].id.clone();
    control.release();
    control.drained().await;
    let error = handle
        .task_command(TaskCommand::Decide {
            operation: operation.clone(),
            approve: true,
        })
        .await
        .unwrap_err();
    assert!(error.contains("current spoken turn"), "{error}");
    let declined = handle
        .task_command(TaskCommand::Decide {
            operation,
            approve: false,
        })
        .await
        .unwrap();
    assert_eq!(
        declined.operations[0].status,
        gemini_adk_fluent_rs::tasks::OperationStatus::Declined
    );
    handle.disconnect().await.unwrap();
}
