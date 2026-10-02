//! Task-local memory projections over one shared logical memory session.
//!
//! Projection is read-only. Ingestion and remember are accepted runtime effects:
//! cancellation can revoke them before execution, but cannot roll back an effect
//! that has started. Their returned slots still require task revision acceptance.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::tasks::TaskMemoryService;
use gemini_adk_rs::tool::{ContextTool, ToolContext, ToolFunction};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use super::tools::{self, ManageArgs, RecallArgs};
use super::turn_extractor::MemorySlot;
use crate::core::{MutationIntent, TurnId};
use crate::engine::MemorySession;

#[derive(Clone, PartialEq)]
enum AcceptedEffect {
    Ingest {
        turn: u64,
        user: String,
    },
    Remember {
        turn: u64,
        note: String,
    },
    Manage {
        intent: MutationIntent,
        statement: String,
    },
}

#[derive(Default)]
struct SessionWork {
    completed: HashMap<String, (AcceptedEffect, Result<Value, String>)>,
    latest_turn: u64,
    finish_result: Option<Result<(), String>>,
}

/// One shared coordinator per binding, never one per activation.
pub(crate) struct TaskMemorySession {
    session: Arc<MemorySession>,
    work: Mutex<SessionWork>,
}

impl TaskMemorySession {
    pub(crate) fn new(session: Arc<MemorySession>) -> Arc<Self> {
        Arc::new(Self {
            session,
            work: Mutex::new(SessionWork::default()),
        })
    }

    pub(crate) fn view(self: &Arc<Self>, slots: Vec<MemorySlot>) -> Arc<dyn TaskMemoryService> {
        Arc::new(TaskMemoryView {
            shared: self.clone(),
            slots,
        })
    }

    pub(crate) fn tools(self: &Arc<Self>) -> Vec<Arc<dyn ToolFunction>> {
        let shared = self.clone();
        let recall = ContextTool::new(
            tools::RECALL_TOOL,
            tools::RECALL_DESCRIPTION,
            tools::recall_context_tool(self.session.clone()).parameters(),
            move |args, ctx| {
                let shared = shared.clone();
                async move {
                    let args: RecallArgs = serde_json::from_value(args)
                        .map_err(|error| ToolError::InvalidArgs(error.to_string()))?;
                    let work = shared.work.lock().await;
                    check_tool_context(&ctx, work.finish_result.is_some())?;
                    if args.query.trim().is_empty() {
                        return Ok(json!({"status": "not_found", "facts": []}));
                    }
                    Ok(shared
                        .session
                        .recall_scoped(
                            &args.query,
                            shared.session.current_turn(),
                            args.scope,
                            args.about,
                            args.attribute,
                        )
                        .await)
                }
            },
        );
        let shared = self.clone();
        let manage = ContextTool::new(
            tools::MANAGE_TOOL,
            tools::MANAGE_DESCRIPTION,
            tools::manage_memory_tool(self.session.clone()).parameters(),
            move |args, ctx| {
                let shared = shared.clone();
                async move {
                    let request_id = args
                        .get("request_id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                        .ok_or_else(|| {
                            ToolError::InvalidArgs("manage_memory requires request_id".into())
                        })?
                        .to_owned();
                    let args: ManageArgs = serde_json::from_value(args)
                        .map_err(|error| ToolError::InvalidArgs(error.to_string()))?;
                    shared
                        .apply(
                            format!("manage:{request_id}"),
                            AcceptedEffect::Manage {
                                intent: args.operation,
                                statement: args.statement.unwrap_or_default().trim().to_owned(),
                            },
                            Some(&ctx),
                        )
                        .await
                        .map_err(ToolError::ExecutionFailed)
                }
            },
        );
        vec![Arc::new(recall), Arc::new(manage)]
    }

    pub(crate) async fn finish(&self) -> Result<(), String> {
        let mut work = self.work.lock().await;
        if let Some(result) = &work.finish_result {
            return result.clone();
        }
        let result = self
            .session
            .finish()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string());
        // Sealing may have succeeded before reconciliation failed. Close the
        // session to new writes in either case and retain the actual outcome.
        work.finish_result = Some(result.clone());
        result
    }

    async fn apply(
        &self,
        id: String,
        effect: AcceptedEffect,
        context: Option<&ToolContext>,
    ) -> Result<Value, String> {
        if id.trim().is_empty() {
            return Err("an accepted memory effect needs a stable identifier".into());
        }
        let mut work = self.work.lock().await;
        if let Some(context) = context {
            check_tool_context(context, work.finish_result.is_some())
                .map_err(|error| error.to_string())?;
        }
        if let Some((previous, result)) = work.completed.get(&id) {
            return if previous == &effect {
                result.clone()
            } else {
                Err("memory effect identifier was reused with different arguments".into())
            };
        }
        if work.finish_result.is_some() {
            return Err("memory session has already finished".into());
        }
        let result = match &effect {
            AcceptedEffect::Ingest { turn, user } => {
                let turn_id = TurnId(*turn);
                let result = async {
                    self.session.observe_final_transcript(turn_id, user).await?;
                    self.session.on_turn_complete(turn_id).await?;
                    // This is runtime service scheduling order, not a speech
                    // index. Workers can execute out of order but cannot rewind
                    // the session's prepared/active retrieval turn.
                    if *turn >= work.latest_turn {
                        let next = TurnId(turn.saturating_add(1));
                        let _ = self.session.prepare(next, user).await;
                        self.session.begin_turn(next);
                        work.latest_turn = *turn;
                    }
                    Ok::<Value, crate::core::MemoryError>(Value::Null)
                }
                .await;
                result.map_err(|error| error.to_string())
            }
            AcceptedEffect::Remember { turn, note } => self
                .session
                .apply_explicit_command(MutationIntent::Remember, note, TurnId(*turn))
                .await
                .map_err(|error| error.to_string()),
            AcceptedEffect::Manage { intent, statement } => {
                if statement.is_empty() && *intent != MutationIntent::List {
                    Ok(json!({
                        "status": "needs_clarification",
                        "operation": intent,
                        "message": "Ask the user what specifically to act on.",
                    }))
                } else {
                    // Tool provenance is its accepted execution turn. It is not
                    // attributed to whichever task most recently spoke.
                    work.latest_turn = work
                        .latest_turn
                        .max(self.session.current_turn().0)
                        .saturating_add(1);
                    let turn = TurnId(work.latest_turn);
                    self.session.begin_turn(turn);
                    self.session
                        .apply_explicit_command(*intent, statement, turn)
                        .await
                        .map_err(|error| error.to_string())
                }
            }
        };
        // An error can follow a partially accepted backend write. Repeating the
        // effect must expose that result, not blindly execute it a second time.
        work.completed.insert(id, (effect, result.clone()));
        result
    }

    fn project(&self, slots: &[MemorySlot]) -> Value {
        let values = self.session.known_values();
        let fields: Map<String, Value> = slots
            .iter()
            .filter_map(|slot| {
                values
                    .iter()
                    .find(|(predicate, _)| predicate == &slot.predicate)
                    .map(|(_, value)| (slot.state_key.clone(), value.clone()))
            })
            .collect();
        Value::Object(fields)
    }
}

fn check_tool_context(context: &ToolContext, finished: bool) -> Result<(), ToolError> {
    if context.task.is_none() {
        return Err(ToolError::ExecutionFailed(
            "task memory tools require task ownership".into(),
        ));
    }
    if context.cancel.is_cancelled() || finished {
        return Err(ToolError::ExecutionFailed(
            "task memory execution is no longer active".into(),
        ));
    }
    Ok(())
}

struct TaskMemoryView {
    shared: Arc<TaskMemorySession>,
    slots: Vec<MemorySlot>,
}

#[async_trait]
impl TaskMemoryService for TaskMemoryView {
    async fn project(&self) -> Result<Value, String> {
        let _work = self.shared.work.lock().await;
        Ok(self.shared.project(&self.slots))
    }

    async fn ingest(&self, effect_id: String, turn: u64, user: String) -> Result<Value, String> {
        self.shared
            .apply(
                format!("service:{effect_id}"),
                AcceptedEffect::Ingest { turn, user },
                None,
            )
            .await?;
        self.project().await
    }

    async fn remember(&self, effect_id: String, turn: u64, note: String) -> Result<Value, String> {
        self.shared
            .apply(
                format!("service:{effect_id}"),
                AcceptedEffect::Remember { turn, note },
                None,
            )
            .await?;
        self.project().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{InMemoryEventLog, MemoryRuntimeConfig, SessionId, UserId};
    use crate::engine::MemoryEngine;
    use crate::ingestion::{
        MemoryObservationExtractor, ObservationExtractionContext, RuleBasedObservationExtractor,
    };
    use crate::okf::OkfRepository;
    use gemini_adk_rs::tasks::{
        CompiledSkill, OperationId, TaskCommand, TaskId, TaskObservation, TaskOwner, TaskRuntime,
        TaskServices,
    };
    use gemini_adk_rs::tool::ToolDispatcher;
    use tokio::sync::Notify;

    fn engine() -> (MemoryEngine, Arc<InMemoryEventLog>) {
        let log = Arc::new(InMemoryEventLog::new());
        let engine = MemoryEngine::new(
            UserId::new("task-user"),
            Arc::new(OkfRepository::in_memory()),
            log.clone(),
            MemoryRuntimeConfig::default(),
        );
        (engine, log)
    }

    fn shared(engine: &MemoryEngine) -> Arc<TaskMemorySession> {
        TaskMemorySession::new(Arc::new(
            engine.begin_session(SessionId::new("task-session")),
        ))
    }

    fn diet(shared: &Arc<TaskMemorySession>) -> Arc<dyn TaskMemoryService> {
        shared.view(vec![MemorySlot::new("dietary_identity", "user:diet")])
    }

    fn context(task: &str) -> ToolContext {
        ToolContext::detached().with_task(TaskOwner {
            task: TaskId(task.into()),
            revision: 1,
            operation: OperationId("operation-1".into()),
        })
    }

    #[tokio::test]
    async fn projection_is_read_only_and_slots_belong_to_each_view() {
        let (engine, log) = engine();
        let shared = shared(&engine);
        let diet = diet(&shared);
        let venue = shared.view(vec![MemorySlot::new("venue_preference", "user:venue")]);
        assert_eq!(diet.project().await.unwrap(), json!({}));
        assert!(log.is_empty());
        assert_eq!(
            diet.ingest(
                "task-1/revision-0/service-4".into(),
                4,
                "I am pescatarian".into()
            )
            .await
            .unwrap(),
            json!({"user:diet":"pescatarian"})
        );
        let events = log.len();
        assert_eq!(venue.project().await.unwrap(), json!({}));
        assert_eq!(
            diet.project().await.unwrap(),
            json!({"user:diet":"pescatarian"})
        );
        assert_eq!(log.len(), events);
    }

    #[tokio::test]
    async fn accepted_ingestion_is_idempotent_and_does_not_rewind_session_turns() {
        let (engine, log) = engine();
        let shared = shared(&engine);
        let diet = diet(&shared);
        diet.ingest("new".into(), 9, "I am pescatarian".into())
            .await
            .unwrap();
        let events = log.len();
        diet.ingest("new".into(), 9, "I am pescatarian".into())
            .await
            .unwrap();
        assert_eq!(log.len(), events);
        assert!(
            diet.ingest("new".into(), 9, "I am vegetarian".into())
                .await
                .is_err()
        );
        diet.ingest("older".into(), 3, "I prefer quiet restaurants".into())
            .await
            .unwrap();
        assert_eq!(shared.session.current_turn(), TurnId(10));
    }

    #[tokio::test]
    async fn management_requires_owner_and_deduplicates_the_business_key() {
        let (engine, log) = engine();
        let shared = shared(&engine);
        let tools = shared.tools();
        let manage = tools
            .iter()
            .find(|tool| tool.name() == tools::MANAGE_TOOL)
            .unwrap();
        let args = json!({"operation":"remember","statement":"I am pescatarian","request_id":"remember-42"});
        assert!(manage.call(args.clone()).await.is_err());
        assert!(log.is_empty());
        let first = manage
            .call_with_context(args.clone(), context("task-1"))
            .await
            .unwrap();
        assert_eq!(first["status"], "accepted");
        assert_eq!(first["durable_commit"], "pending");
        let events = log.len();
        let retry = manage
            .call_with_context(args, context("task-2"))
            .await
            .unwrap();
        assert_eq!(retry, first);
        assert_eq!(log.len(), events);
        assert!(manage.call_with_context(json!({"operation":"forget","statement":"I am pescatarian","request_id":"remember-42"}), context("task-1")).await.is_err());
        let recall = tools
            .iter()
            .find(|tool| tool.name() == tools::RECALL_TOOL)
            .unwrap();
        let recalled = recall
            .call_with_context(
                json!({"query":"dietary preference pescatarian"}),
                context("task-2"),
            )
            .await
            .unwrap();
        assert_eq!(recalled["status"], "found");
        assert!(recalled.to_string().contains("pescatarian"));
    }

    #[tokio::test]
    async fn teardown_commits_once_and_rejects_later_writes() {
        let (engine, log) = engine();
        let shared = shared(&engine);
        diet(&shared)
            .ingest("first".into(), 1, "I am pescatarian".into())
            .await
            .unwrap();
        shared.finish().await.unwrap();
        let events = log.len();
        shared.finish().await.unwrap();
        assert_eq!(log.len(), events);
        assert!(
            diet(&shared)
                .remember("late".into(), 2, "I am vegetarian".into())
                .await
                .is_err()
        );
        assert_eq!(log.len(), events);
        engine.compile_index().await.unwrap();
        let next = engine.begin_session(SessionId::new("next-session"));
        let recalled = next
            .recall("dietary preference pescatarian", TurnId(1))
            .await;
        assert_eq!(recalled["status"], "found");
        assert!(recalled.to_string().contains("pescatarian"));
    }

    struct FailedSeal(Arc<InMemoryEventLog>);
    #[async_trait]
    impl crate::core::MemoryEventLog for FailedSeal {
        async fn append(
            &self,
            event: crate::core::MemoryEventEnvelope,
        ) -> Result<(), crate::core::MemoryError> {
            if matches!(
                event.payload,
                crate::core::MemoryEvent::SessionSealed { .. }
            ) {
                return Err(crate::core::MemoryError::EventLog(
                    "test seal failure".into(),
                ));
            }
            self.0.append(event).await
        }
        async fn replay_session(
            &self,
            session: &SessionId,
        ) -> Result<Vec<crate::core::MemoryEventEnvelope>, crate::core::MemoryError> {
            self.0.replay_session(session).await
        }
    }

    #[tokio::test]
    async fn failed_teardown_retains_failure_and_closes_session_to_writes() {
        let log = Arc::new(InMemoryEventLog::new());
        let engine = MemoryEngine::new(
            UserId::new("task-user"),
            Arc::new(OkfRepository::in_memory()),
            Arc::new(FailedSeal(log.clone())),
            MemoryRuntimeConfig::default(),
        );
        let shared = shared(&engine);
        diet(&shared)
            .ingest("first".into(), 1, "I am pescatarian".into())
            .await
            .unwrap();
        let failure = shared.finish().await.unwrap_err();
        assert!(failure.contains("test seal failure"));
        assert_eq!(shared.finish().await.unwrap_err(), failure);
        let count = log.len();
        assert!(
            diet(&shared)
                .remember("late".into(), 2, "I am vegetarian".into())
                .await
                .is_err()
        );
        assert_eq!(log.len(), count);
    }

    struct BlockingExtractor {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    #[async_trait]
    impl MemoryObservationExtractor for BlockingExtractor {
        async fn extract(
            &self,
            context: ObservationExtractionContext,
        ) -> Result<Vec<crate::core::MemoryObservation>, crate::core::MemoryError> {
            self.entered.notify_one();
            self.release.notified().await;
            RuleBasedObservationExtractor::new().extract(context).await
        }
    }

    #[tokio::test]
    async fn teardown_waits_for_started_ingestion() {
        let (engine, _) = engine();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let engine = engine.with_observation_extractor(Arc::new(BlockingExtractor {
            entered: entered.clone(),
            release: release.clone(),
        }));
        let shared = shared(&engine);
        let view = diet(&shared);
        let ingestion = tokio::spawn(async move {
            view.ingest("accepted".into(), 1, "I am pescatarian".into())
                .await
        });
        entered.notified().await;
        let finish = shared.finish();
        tokio::pin!(finish);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut finish)
                .await
                .is_err()
        );
        release.notify_one();
        let (ingestion, finished) = tokio::join!(ingestion, finish);
        ingestion.unwrap().unwrap();
        finished.unwrap();
        assert_eq!(
            engine.repository().all(engine.user()).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn cancelled_tool_waiting_for_memory_lock_never_mutates() {
        let (engine, log) = engine();
        let shared = shared(&engine);
        let lock = shared.work.lock().await;
        let tools = shared.tools();
        let manage = tools
            .iter()
            .find(|tool| tool.name() == tools::MANAGE_TOOL)
            .unwrap();
        let context = context("task-1");
        let cancel = context.cancel.clone();
        let pending = manage.call_with_context(
            json!({"operation":"remember","statement":"I am pescatarian","request_id":"cancelled"}),
            context,
        );
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut pending)
                .await
                .is_err()
        );
        cancel.cancel();
        drop(lock);
        assert!(pending.await.is_err());
        assert!(log.is_empty());
    }

    fn skill(shared: &Arc<TaskMemorySession>) -> CompiledSkill {
        let memory = diet(shared);
        CompiledSkill {
            services: Some(Arc::new(move || {
                Ok(TaskServices {
                    memory: Some(memory.clone()),
                    ..Default::default()
                })
            })),
            name: "dining".into(),
            version: "1".into(),
            description: "Dining preferences".into(),
            instruction: String::new(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            validate_input: Arc::new(|_| Ok(())),
            validate_output: Arc::new(|_| Ok(())),
            initial_state: HashMap::new(),
            flow_factory: None,
            tools: Vec::new(),
            tool_factory: Arc::new(|_| Ok(ToolDispatcher::new())),
            receipt_applier: None,
            project_output: Some(Arc::new(|state| {
                Ok(json!({"diet":state.get_raw("user:diet")}))
            })),
        }
    }

    fn start(runtime: &mut TaskRuntime, input: Value) -> TaskId {
        runtime
            .command(TaskCommand::Start {
                skill: "dining".into(),
                input,
                parent: None,
            })
            .unwrap();
        runtime.snapshot().foreground.unwrap()
    }

    async fn drain(runtime: &mut TaskRuntime) {
        loop {
            let jobs = runtime.take_ready_services();
            if jobs.is_empty() {
                break;
            }
            for job in jobs {
                assert!(runtime.complete_services(job.execute().await));
            }
        }
    }

    fn output(runtime: &mut TaskRuntime, task: TaskId) -> Value {
        runtime
            .command(TaskCommand::Complete {
                task: task.clone(),
                output: json!({}),
            })
            .unwrap();
        runtime
            .snapshot()
            .tasks
            .into_iter()
            .find(|item| item.id == task)
            .unwrap()
            .output
            .unwrap()
    }

    #[tokio::test]
    async fn accepted_projection_preserves_explicit_task_inputs() {
        let (engine, _) = engine();
        let shared = shared(&engine);
        diet(&shared)
            .ingest("prior".into(), 1, "I am pescatarian".into())
            .await
            .unwrap();
        let mut runtime = TaskRuntime::new(vec![skill(&shared)]).unwrap();
        let task = start(&mut runtime, json!({"user:diet":"vegan"}));
        drain(&mut runtime).await;
        assert_eq!(output(&mut runtime, task), json!({"diet":"vegan"}));
    }

    #[tokio::test]
    async fn cancellation_before_execution_never_ingests() {
        let (engine, log) = engine();
        let shared = shared(&engine);
        let mut runtime = TaskRuntime::new(vec![skill(&shared)]).unwrap();
        let task = start(&mut runtime, json!({}));
        drain(&mut runtime).await;
        runtime
            .observe(
                &task,
                1,
                TaskObservation::Turn {
                    user: "I am pescatarian".into(),
                    model: String::new(),
                },
            )
            .unwrap();
        let jobs = runtime.take_ready_services();
        assert!(!jobs.is_empty());
        runtime.command(TaskCommand::Cancel { task }).unwrap();
        for job in jobs {
            assert!(!runtime.complete_services(job.execute().await));
        }
        assert!(log.is_empty());
    }

    #[tokio::test]
    async fn revision_rejects_finished_projection_without_rolling_back_accepted_memory() {
        let (engine, _) = engine();
        let shared = shared(&engine);
        let mut runtime = TaskRuntime::new(vec![skill(&shared)]).unwrap();
        let task = start(&mut runtime, json!({}));
        drain(&mut runtime).await;
        runtime
            .observe(
                &task,
                1,
                TaskObservation::Turn {
                    user: "I am pescatarian".into(),
                    model: String::new(),
                },
            )
            .unwrap();
        let jobs = runtime.take_ready_services();
        assert_eq!(jobs.len(), 1);
        let completion = jobs.into_iter().next().unwrap().execute().await;
        runtime
            .command(TaskCommand::Revise {
                task: task.clone(),
                expected_revision: 1,
                input: json!({"user:diet":"vegan"}),
            })
            .unwrap();
        assert!(!runtime.complete_services(completion));
        drain(&mut runtime).await;
        assert_eq!(output(&mut runtime, task), json!({"diet":"vegan"}));
        assert_eq!(
            diet(&shared).project().await.unwrap(),
            json!({"user:diet":"pescatarian"})
        );
    }

    #[tokio::test]
    async fn compiled_memory_tools_use_real_engine_only_after_approval() {
        use super::super::spec_binding::SessionMemoryBinding;
        use gemini_adk_fluent_rs::spec::{SkillSpec, SpecResources};

        let (engine, log) = engine();
        let session = Arc::new(engine.begin_session(SessionId::new("compiled-session")));
        let binding = Arc::new(SessionMemoryBinding::new(session));
        let definition: SkillSpec = serde_json::from_value(json!({
            "name":"dining", "version":"1",
            "state":{"user:diet":{"type":"string"}},
            "memory":{"slots":[{"predicate":"dietary_identity","to":"user:diet"}]}
        }))
        .unwrap();
        let resources = SpecResources {
            memory: Some(binding),
            ..Default::default()
        };
        let compiled = definition.compile(&resources).unwrap();
        let manage = compiled
            .tools
            .iter()
            .find(|tool| tool.name == tools::MANAGE_TOOL)
            .unwrap();
        assert!(
            manage.parameters.as_ref().unwrap()["required"]
                .as_array()
                .unwrap()
                .contains(&json!("request_id"))
        );
        let mut runtime = TaskRuntime::new(vec![compiled]).unwrap();
        let task = start(&mut runtime, json!({}));
        drain(&mut runtime).await;

        for approve in [false, true] {
            let key = if approve {
                "accepted-memory"
            } else {
                "declined-memory"
            };
            let invocation = runtime.command(TaskCommand::Invoke {
                task:task.clone(), tool:tools::MANAGE_TOOL.into(),
                args:json!({"operation":"remember","statement":"I prefer evening appointments","request_id":key}),
                idempotency_key:Some(key.into()),
            }).unwrap();
            assert!(invocation.is_none());
            assert!(log.is_empty(), "waiting approval cannot mutate memory");
            let operation = runtime.snapshot().tasks[0].pending.clone().unwrap();
            let work = runtime
                .command(TaskCommand::Decide { operation, approve })
                .unwrap();
            if !approve {
                assert!(work.is_none());
                assert!(log.is_empty());
            } else {
                let delivery = runtime.complete(work.unwrap().execute().await);
                assert!(!delivery.stale);
                assert_eq!(delivery.result.unwrap()["durable_commit"], "pending");
            }
        }
        drain(&mut runtime).await;
        let recall = runtime
            .command(TaskCommand::Invoke {
                task,
                tool: tools::RECALL_TOOL.into(),
                args: json!({"query":"evening appointments"}),
                idempotency_key: None,
            })
            .unwrap()
            .unwrap();
        let delivery = runtime.complete(recall.execute().await);
        let value = delivery.result.unwrap();
        assert_eq!(value["status"], "found");
        assert!(value.to_string().contains("evening"));
    }

    #[tokio::test]
    async fn computed_watcher_remembers_once_and_cancelled_jobs_do_not_write() {
        use super::super::spec_binding::SessionMemoryBinding;
        use gemini_adk_fluent_rs::spec::{SkillSpec, SpecResources};

        for cancel_before_work in [false, true] {
            let (engine, log) = engine();
            let session = Arc::new(engine.begin_session(SessionId::new("watch-session")));
            let resources = SpecResources {
                memory: Some(Arc::new(SessionMemoryBinding::new(session))),
                ..Default::default()
            };
            let definition: SkillSpec = serde_json::from_value(json!({
                "name":"dining", "version":"1",
                "inputs":{"party_size":{"type":"number"}},
                "computed":[{"key":"large_party","from":{"gte":[{"key":"party_size"},{"const":6}]}}],
                "watch":[{"key":"large_party","condition":"became_true","effects":[{"remember":"Books for large parties ({state.party_size} guests)"}]}],
                "memory":{"slots":[]}
            })).unwrap();
            let mut runtime =
                TaskRuntime::new(vec![definition.compile(&resources).unwrap()]).unwrap();
            let task = start(&mut runtime, json!({"party_size":7}));
            if cancel_before_work {
                runtime.command(TaskCommand::Cancel { task }).unwrap();
                for job in runtime.take_ready_services() {
                    assert!(!runtime.complete_services(job.execute().await));
                }
                assert!(log.is_empty());
            } else {
                drain(&mut runtime).await;
                assert_eq!(log.count_label("explicit_mutation_requested"), 1);
                assert!(log.entries().iter().any(|event| match &event.payload {
                    crate::core::MemoryEvent::ExplicitMutationRequested { statement, .. } =>
                        statement == "Books for large parties (7 guests)",
                    _ => false,
                }));
                runtime
                    .observe(
                        &task,
                        1,
                        TaskObservation::Turn {
                            user: "Could you repeat that please?".into(),
                            model: String::new(),
                        },
                    )
                    .unwrap();
                drain(&mut runtime).await;
                assert_eq!(log.count_label("explicit_mutation_requested"), 1);
            }
        }
    }

    #[tokio::test]
    async fn managed_correction_refreshes_task_slot_and_forget_removes_it_before_teardown() {
        use super::super::spec_binding::SessionMemoryBinding;
        use gemini_adk_fluent_rs::spec::{SkillSpec, SpecResources};

        let (engine, _) = engine();
        let first = engine.begin_session(SessionId::new("prior-session"));
        first
            .observe_final_transcript(TurnId(1), "I am pescatarian")
            .await
            .unwrap();
        first.finish().await.unwrap();
        let session = Arc::new(engine.begin_session(SessionId::new("change-session")));
        let binding = Arc::new(SessionMemoryBinding::new(session.clone()));
        let definition: SkillSpec = serde_json::from_value(json!({
            "name":"dining", "version":"1", "state":{"user:diet":{"type":"string"}},
            "outputs":{"user:diet":{"type":"string","default":""}},
            "memory":{"slots":[{"predicate":"dietary_identity","to":"user:diet"}]}
        }))
        .unwrap();
        let resources = SpecResources {
            memory: Some(binding.clone()),
            ..Default::default()
        };
        let mut runtime = TaskRuntime::new(vec![definition.compile(&resources).unwrap()]).unwrap();
        let task = start(&mut runtime, json!({}));
        drain(&mut runtime).await;
        assert!(
            runtime
                .foreground_instruction()
                .unwrap()
                .contains("\"user:diet\":\"pescatarian\"")
        );
        let correction = "I prefer vegetarian meals instead of pescatarian meals";
        for (operation, statement) in [("correct", correction), ("forget", "pescatarian")] {
            runtime.command(TaskCommand::Invoke {
                task:task.clone(), tool:tools::MANAGE_TOOL.into(),
                args:json!({"operation":operation,"statement":statement,"request_id":operation}),
                idempotency_key:Some(operation.into()),
            }).unwrap();
            let id = runtime.snapshot().tasks[0].pending.clone().unwrap();
            let invocation = runtime
                .command(TaskCommand::Decide {
                    operation: id,
                    approve: true,
                })
                .unwrap()
                .unwrap();
            assert!(runtime.complete(invocation.execute().await).result.is_ok());
            drain(&mut runtime).await;
            if operation == "correct" {
                assert!(
                    runtime
                        .foreground_instruction()
                        .unwrap()
                        .contains(correction)
                );
                assert_eq!(session.known_values()[0].1, correction);
            } else {
                assert!(
                    !runtime
                        .foreground_instruction()
                        .unwrap()
                        .contains("user:diet")
                );
                assert!(session.known_values().is_empty());
                assert!(session.known_statements().is_empty());
                assert_eq!(
                    session
                        .recall("dietary preference pescatarian", session.current_turn())
                        .await["status"],
                    "not_found"
                );
            }
        }
        assert_eq!(output(&mut runtime, task), json!({}));
        session.finish().await.unwrap();
        let next = engine.begin_session(SessionId::new("after-forget"));
        assert!(next.known_values().is_empty());
    }
}
