//! Task-owned service plans, isolated work, and accepted effects.
use super::{TaskOwner, TaskServicesFactory};
use crate::{
    live::{
        computed::ComputedRegistry,
        extractor::{ExtractionTrigger, TurnExtractor},
        temporal::{PatternDetector, pattern_fires},
        transcript::{TranscriptBuffer, TranscriptTurn},
        watcher::WatchPredicate,
    },
    state::State,
};
use async_trait::async_trait;
use futures_util::FutureExt;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

/// Declarative effects rendered against accepted task state.
pub type TaskEffects = Arc<dyn Fn(&State) -> Vec<TaskEffect> + Send + Sync>;
/// An effect requested by task-local declarative services.
#[derive(Clone, Debug, serde::Serialize)]
pub enum TaskEffect {
    /// Change one declared task-local field.
    Set {
        /// Field name.
        key: String,
        /// New value.
        value: Value,
    },
    /// Add task-owned context without requesting a response.
    Context(String),
    /// Request a response, only while this owner remains foreground.
    Prompt(String),
    /// Persist an accepted note through the configured memory service.
    Remember(String),
    /// Observable service execution or stabilization failure.
    Error(String),
}
/// A state predicate and its declarative task-local actions.
pub struct TaskWatcher {
    /// Observed key, including a bare computed variable name.
    pub key: String,
    /// Existing shared old/new value predicate.
    pub predicate: WatchPredicate,
    /// Effects rendered after the change is accepted.
    pub effects: TaskEffects,
}
/// A task-local temporal detector and its declarative actions.
pub struct TaskPattern {
    /// Diagnostic name.
    pub name: String,
    /// Fresh detector for this activation.
    pub detector: Box<dyn PatternDetector>,
    /// Minimum active-task time between firings.
    pub cooldown: Option<Duration>,
    /// Effects rendered after the detector fires.
    pub effects: TaskEffects,
}
/// Memory operations bound to a trusted session subject and declared task slots.
/// Ingestion and remember are accepted effects. Cancellation after execution starts
/// cannot undo storage; returned slot projections still require current ownership.
/// Each result is a complete projection: omitted or null slots remove a previous
/// memory-owned value, while explicit inputs and accepted local changes remain.
#[async_trait]
pub trait TaskMemoryService: Send + Sync {
    /// Read declared slot projections without ingesting or persisting new data.
    async fn project(&self) -> Result<Value, String>;
    /// Ingest an accepted user turn. The effect ID is stable for deduplication.
    async fn ingest(&self, effect_id: String, turn: u64, user: String) -> Result<Value, String>;
    /// Remember an accepted note, then return declared slot projections.
    async fn remember(&self, effect_id: String, turn: u64, note: String) -> Result<Value, String>;
}
/// Fresh per-activation services. Workers never receive session transport.
#[derive(Default)]
pub struct TaskServices {
    /// Extractors using immutable windows and isolated state snapshots.
    pub extractors: Vec<Arc<dyn TurnExtractor>>,
    /// Fresh dependency-ordered computed registry.
    pub computed: ComputedRegistry,
    /// Declarative state watchers.
    pub watchers: Vec<TaskWatcher>,
    /// Fresh temporal detectors.
    pub patterns: Vec<TaskPattern>,
    /// Trusted memory projection and accepted-effect adapter.
    pub memory: Option<Arc<dyn TaskMemoryService>>,
}
/// An observation whose task/revision was captured before routing or async work.
#[derive(Clone, Debug)]
pub enum TaskObservation {
    /// Finalized user and heard model text for one owned turn.
    Turn {
        /// User utterance.
        user: String,
        /// Heard model response.
        model: String,
    },
    /// Generated text before any interruption truncation.
    GenerationComplete {
        /// Current user utterance.
        user: String,
        /// Generated model response.
        model: String,
    },
    /// An interruption belonging to this task.
    Interrupted,
    /// A timer tick for the foreground task.
    Timer,
}
/// A service effect accepted by the reducer, retaining its original owner.
#[derive(Clone, Debug, serde::Serialize)]
pub struct AcceptedTaskEffect {
    /// Task, revision and service event identity.
    pub owner: TaskOwner,
    /// Accepted effect. Hosts must recheck ownership before delivering speech.
    pub effect: TaskEffect,
}
#[derive(Clone)]
pub(super) enum ServiceEvent {
    Initialize,
    Observe(TaskObservation),
    AfterTool {
        tool: String,
        args: Value,
        result: Value,
    },
    Project,
    Ingest {
        user: String,
    },
    Remember {
        note: String,
    },
}
struct ExtractRequest {
    extractor: Arc<dyn TurnExtractor>,
    window: Vec<TranscriptTurn>,
}
enum ServiceWork {
    Extract(Vec<ExtractRequest>),
    Memory {
        provider: Arc<dyn TaskMemoryService>,
        event: ServiceEvent,
    },
}
/// Runtime-scoped service work. The private ticket cannot grant another task writes.
pub struct OwnedTaskServiceWork {
    pub(super) owner: TaskOwner,
    pub(super) scope: Arc<()>,
    pub(super) cancel: CancellationToken,
    state: State,
    work: ServiceWork,
}
/// Private completion of a runtime-prepared service job.
pub struct TaskServiceCompletion {
    pub(super) owner: TaskOwner,
    pub(super) scope: Arc<()>,
    pub(super) result: ServiceResult,
}
pub(super) enum ServiceResult {
    Extractions(Vec<(Arc<dyn TurnExtractor>, Result<Value, String>)>),
    Memory(Result<Value, String>),
    Failed(String),
}
impl OwnedTaskServiceWork {
    /// Captured task ownership, also used for correlation in controlled replay.
    pub fn owner(&self) -> &TaskOwner {
        &self.owner
    }
    /// Names selected by this job's canonical extraction trigger and window checks.
    pub fn extractor_names(&self) -> Vec<&str> {
        match &self.work {
            ServiceWork::Extract(requests) => requests.iter().map(|r| r.extractor.name()).collect(),
            _ => Vec::new(),
        }
    }
    /// Execute configured service providers without reaching canonical task state.
    pub async fn execute(self) -> TaskServiceCompletion {
        self.run(None).await
    }
    /// Execute due extraction work with explicit provider fixtures. Missing fixtures
    /// become service errors; this never calls the extraction provider.
    pub async fn execute_controlled(
        self,
        values: BTreeMap<String, Result<Value, String>>,
    ) -> TaskServiceCompletion {
        self.run(Some(values)).await
    }
    async fn run(
        self,
        mut fixtures: Option<BTreeMap<String, Result<Value, String>>>,
    ) -> TaskServiceCompletion {
        let owner = self.owner.clone();
        let result = if self.cancel.is_cancelled() {
            ServiceResult::Failed("service cancelled before execution".into())
        } else {
            let future = async {
                match self.work {
                    ServiceWork::Extract(requests) => {
                        let mut results = Vec::new();
                        for request in requests {
                            if self.cancel.is_cancelled() {
                                return ServiceResult::Failed("service cancelled".into());
                            }
                            let result = if let Some(values) = fixtures.as_mut() {
                                values.remove(request.extractor.name()).unwrap_or_else(|| {
                                    Err(format!(
                                        "missing controlled extraction '{}'",
                                        request.extractor.name()
                                    ))
                                })
                            } else {
                                request
                                    .extractor
                                    .extract_with_state(&request.window, &self.state)
                                    .await
                                    .map_err(|error| error.to_string())
                            };
                            results.push((request.extractor, result));
                        }
                        if fixtures.as_ref().is_some_and(|values| !values.is_empty()) {
                            return ServiceResult::Failed(
                                "controlled extraction supplied unused fixtures".into(),
                            );
                        }
                        ServiceResult::Extractions(results)
                    }
                    ServiceWork::Memory { provider, event } => {
                        let effect_id = format!(
                            "{}/revision-{}/{}",
                            owner.task.0, owner.revision, owner.operation.0
                        );
                        let sequence = owner
                            .operation
                            .0
                            .strip_prefix("service-")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let result = match event {
                            ServiceEvent::Project => provider.project().await,
                            ServiceEvent::Ingest { user, .. } => {
                                provider.ingest(effect_id, sequence, user).await
                            }
                            ServiceEvent::Remember { note, .. } => {
                                provider.remember(effect_id, sequence, note).await
                            }
                            _ => unreachable!("only memory work reaches memory provider"),
                        };
                        ServiceResult::Memory(result)
                    }
                }
            };
            match std::panic::AssertUnwindSafe(future).catch_unwind().await {
                Ok(result) => result,
                Err(_) => ServiceResult::Failed("task service panicked".into()),
            }
        };
        TaskServiceCompletion {
            owner: self.owner,
            scope: self.scope,
            result,
        }
    }
}
pub(super) struct PendingService {
    pub owner: TaskOwner,
    pub cancel: CancellationToken,
    pub event: ServiceEvent,
    pub before: HashMap<String, Value>,
}
pub(super) struct ServiceState {
    pub plan: TaskServices,
    pub queue: VecDeque<ServiceEvent>,
    pub pending: Option<PendingService>,
    pub transcript: TranscriptBuffer,
    pub turns: u64,
    pub errors: Vec<String>,
    pub contexts: Vec<String>,
    pub memory_keys: BTreeSet<String>,
    pub memory_values: BTreeMap<String, Value>,
    triggered: Vec<Option<Instant>>,
    logical_now: Instant,
    last_wall: Instant,
}
impl ServiceState {
    pub(super) fn create(
        factory: &Option<TaskServicesFactory>,
        state: &State,
    ) -> Result<Option<Self>, String> {
        let Some(factory) = factory else {
            return Ok(None);
        };
        let plan = factory()?;
        for extractor in &plan.extractors {
            if reserved_service_key(extractor.name())
                || extractor
                    .promotion_rules()
                    .iter()
                    .any(|rule| reserved_service_key(&rule.state_key))
            {
                return Err(format!(
                    "task extractor '{}' writes a reserved runtime field",
                    extractor.name()
                ));
            }
            if extractor.on_complete().is_some()
                || extractor.trigger() == ExtractionTrigger::OnPhaseChange
            {
                return Err(format!(
                    "task extractor '{}' has unsupported detached completion or phase trigger",
                    extractor.name()
                ));
            }
        }
        plan.computed
            .validate()
            .map_err(|error| error.to_string())?;
        let mut queue = VecDeque::from([ServiceEvent::Initialize]);
        if plan.memory.is_some() {
            queue.push_back(ServiceEvent::Project);
        }
        let triggered = vec![None; plan.patterns.len()];
        let now = state.clock().now();
        Ok(Some(Self {
            plan,
            queue,
            pending: None,
            transcript: TranscriptBuffer::new(),
            turns: 0,
            errors: Vec::new(),
            contexts: Vec::new(),
            memory_keys: BTreeSet::new(),
            memory_values: BTreeMap::new(),
            triggered,
            logical_now: now,
            last_wall: now,
        }))
    }
    pub(super) fn refreshing_memory(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.event, ServiceEvent::Project))
            || self
                .queue
                .iter()
                .any(|event| matches!(event, ServiceEvent::Project))
    }
    pub(super) fn advance_clock(&mut self, state: &State, active: bool) {
        let now = state.clock().now();
        if active {
            self.logical_now += now.saturating_duration_since(self.last_wall);
        }
        self.last_wall = now;
    }
    pub(super) fn prepare(
        &mut self,
        event: &ServiceEvent,
        owner: TaskOwner,
        state: State,
        scope: Arc<()>,
    ) -> Option<OwnedTaskServiceWork> {
        let memory_event = matches!(
            event,
            ServiceEvent::Project | ServiceEvent::Ingest { .. } | ServiceEvent::Remember { .. }
        );
        let work = if memory_event {
            ServiceWork::Memory {
                provider: self.plan.memory.clone()?,
                event: event.clone(),
            }
        } else {
            let trigger = match event {
                ServiceEvent::Observe(TaskObservation::Turn { user, model }) => {
                    self.turns += 1;
                    self.transcript.push_input(user);
                    self.transcript.push_output(model);
                    self.transcript.end_turn();
                    ExtractionTrigger::EveryTurn
                }
                ServiceEvent::Observe(TaskObservation::GenerationComplete { .. }) => {
                    ExtractionTrigger::OnGenerationComplete
                }
                ServiceEvent::AfterTool { tool, args, result } => {
                    self.transcript.push_tool_call(tool.clone(), args, result);
                    ExtractionTrigger::AfterToolCall
                }
                _ => return None,
            };
            let mut requests = Vec::new();
            for extractor in &self.plan.extractors {
                let due = match extractor.trigger() {
                    ExtractionTrigger::Interval(n) => {
                        trigger == ExtractionTrigger::EveryTurn
                            && n > 0
                            && self.turns.is_multiple_of(u64::from(n))
                    }
                    value => value == trigger,
                };
                if !due {
                    continue;
                }
                let mut window = if trigger == ExtractionTrigger::AfterToolCall {
                    self.transcript
                        .snapshot_window_with_current(extractor.window_size())
                        .turns()
                        .to_vec()
                } else {
                    self.transcript.window(extractor.window_size()).to_vec()
                };
                if let ServiceEvent::Observe(TaskObservation::GenerationComplete { user, model }) =
                    event
                {
                    window.push(TranscriptTurn {
                        turn_number: self.turns as u32,
                        user: user.clone(),
                        model: model.clone(),
                        tool_calls: Vec::new(),
                        timestamp: self.logical_now,
                    });
                    let discard = window.len().saturating_sub(extractor.window_size());
                    window.drain(..discard);
                }
                if !window.is_empty() && extractor.should_extract(&window) {
                    requests.push(ExtractRequest {
                        extractor: extractor.clone(),
                        window,
                    });
                }
            }
            if requests.is_empty() {
                return None;
            }
            ServiceWork::Extract(requests)
        };
        let cancel = CancellationToken::new();
        self.pending = Some(PendingService {
            owner: owner.clone(),
            cancel: cancel.clone(),
            event: event.clone(),
            before: state.to_hashmap(),
        });
        Some(OwnedTaskServiceWork {
            owner,
            scope,
            cancel,
            state,
            work,
        })
    }
    pub(super) fn stabilize(
        &mut self,
        state: &State,
        before: HashMap<String, Value>,
        event: Option<&ServiceEvent>,
    ) -> Vec<TaskEffect> {
        let mut emitted = Vec::new();
        let mut previous = before;
        for pass in 0..32 {
            self.plan.computed.recompute(state);
            let current = state.to_hashmap();
            let mut effects = Vec::new();
            for watcher in &self.plan.watchers {
                let key = if current.contains_key(&format!("derived:{}", watcher.key)) {
                    format!("derived:{}", watcher.key)
                } else {
                    watcher.key.clone()
                };
                let old = previous.get(&key).unwrap_or(&Value::Null);
                let new = current.get(&key).unwrap_or(&Value::Null);
                if old != new && watcher.predicate.matches(old, new) {
                    effects.extend((watcher.effects)(state));
                }
            }
            previous = current;
            let changed = Self::apply_effects(state, effects, &mut emitted);
            if !changed {
                break;
            }
            if pass == 31 {
                return vec![TaskEffect::Error(
                    "task watcher effects did not converge after 32 passes".into(),
                )];
            }
        }
        let turn = matches!(
            event,
            Some(ServiceEvent::Observe(TaskObservation::Turn { .. }))
        );
        let timer = matches!(event, Some(ServiceEvent::Observe(TaskObservation::Timer)));
        let turn_event = turn.then_some(gemini_genai_rs::session::SessionEvent::TurnComplete);
        let mut effects = Vec::new();
        for (index, pattern) in self.plan.patterns.iter().enumerate() {
            if !(turn || (timer && pattern.detector.needs_timer())) {
                continue;
            }
            if pattern_fires(
                pattern.detector.as_ref(),
                state,
                turn_event.as_ref(),
                self.logical_now,
                pattern.cooldown,
                &mut self.triggered[index],
            ) {
                effects.extend((pattern.effects)(state));
            }
        }
        if Self::apply_effects(state, effects, &mut emitted) {
            // Temporal Set effects enter the same computed/watch stabilization without counting another turn.
            emitted.extend(self.stabilize(state, previous, None));
        }
        emitted
    }
    fn apply_effects(
        state: &State,
        effects: Vec<TaskEffect>,
        emitted: &mut Vec<TaskEffect>,
    ) -> bool {
        let mut changed = false;
        for effect in effects {
            if let TaskEffect::Set { key, value } = effect {
                if reserved_service_key(&key) {
                    emitted.push(TaskEffect::Error(format!(
                        "task service effect uses reserved field '{key}'"
                    )));
                    continue;
                }
                if state.get_raw(&key).as_ref() != Some(&value) {
                    match state.set(&key, value) {
                        Ok(()) => changed = true,
                        Err(error) => emitted.push(TaskEffect::Error(error.to_string())),
                    }
                }
            } else {
                emitted.push(effect);
            }
        }
        changed
    }
    pub(super) fn invalidate(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.cancel.cancel();
        }
        self.queue.clear();
    }
}

pub(super) fn reserved_service_key(key: &str) -> bool {
    [
        "task:",
        "flow:",
        "derived:",
        "repair:",
        "correction:",
        "state_meta:",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
}
