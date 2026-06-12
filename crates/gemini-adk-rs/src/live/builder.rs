//! LiveSessionBuilder — combines SessionConfig + callbacks + tools into one setup.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use gemini_genai_rs::prelude::{ConnectBuilder, SessionConfig, SessionPhase};
use gemini_genai_rs::session::{SessionHandle, SessionWriter};

use crate::error::AgentError;
use crate::state::State;
use crate::tool::ToolDispatcher;

use super::background_tool::{BackgroundToolTracker, ToolExecutionMode};
use super::callbacks::EventCallbacks;
use super::computed::ComputedRegistry;
use super::context_writer::{DeferredWriter, PendingContext};
use super::extractor::TurnExtractor;
use super::handle::LiveHandle;
use super::needs::{NeedsFulfillment, RepairConfig};
use super::persistence::SessionPersistence;
use super::phase::PhaseMachine;
use super::processor::{spawn_event_processor, spawn_telemetry_lane, ControlPlaneConfig};
use super::session_signals::SessionSignals;
use super::soft_turn::SoftTurnDetector;
use super::steering::{ContextDelivery, SteeringMode};
use super::telemetry::SessionTelemetry;
use super::temporal::TemporalRegistry;
use super::watcher::WatcherRegistry;

/// Builder for a callback-driven Live session.
///
/// Combines [`SessionConfig`], [`EventCallbacks`], tool dispatching, extractors,
/// computed state, phase machines, watchers, and temporal patterns into a
/// single connection setup. Call [`connect()`](Self::connect) to establish
/// the WebSocket connection and start the three-lane event processor.
///
/// For ergonomic usage, prefer the L2 `Live` builder from `gemini-adk-fluent-rs`
/// which wraps this with a fluent API.
pub struct LiveSessionBuilder {
    config: SessionConfig,
    callbacks: EventCallbacks,
    dispatcher: Option<Arc<ToolDispatcher>>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<PhaseMachine>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<TemporalRegistry>,
    greeting: Option<String>,
    state: Option<State>,
    execution_modes: HashMap<String, ToolExecutionMode>,
    // Control plane configuration
    soft_turn_timeout: Option<std::time::Duration>,
    steering_mode: SteeringMode,
    context_delivery: ContextDelivery,
    delivery: super::processor::DeliveryConfig,
    repair_config: Option<RepairConfig>,
    persistence: Option<Arc<dyn SessionPersistence>>,
    session_id: Option<String>,
    tool_advisory: bool,
    telemetry_interval: Option<std::time::Duration>,
    middleware: Vec<Arc<dyn crate::middleware::Middleware>>,
    flow: Option<crate::flow::FlowMonitor>,
}

impl LiveSessionBuilder {
    /// Create a new builder with the given session config.
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            callbacks: EventCallbacks::default(),
            dispatcher: None,
            extractors: Vec::new(),
            computed: None,
            phase_machine: None,
            watchers: None,
            temporal: None,
            greeting: None,
            state: None,
            execution_modes: HashMap::new(),
            soft_turn_timeout: None,
            steering_mode: SteeringMode::default(),
            context_delivery: ContextDelivery::default(),
            delivery: super::processor::DeliveryConfig::default(),
            repair_config: None,
            persistence: None,
            session_id: None,
            tool_advisory: true,
            telemetry_interval: None,
            middleware: Vec::new(),
            flow: None,
        }
    }

    /// Add a middleware layer.
    ///
    /// Layers run around tool dispatch in the control lane: `before_tool`
    /// (a returned error vetoes the call), `after_tool`, and `on_tool_error`.
    /// Multiple calls accumulate in order.
    pub fn middleware(mut self, layer: Arc<dyn crate::middleware::Middleware>) -> Self {
        self.middleware.push(layer);
        self
    }

    /// Attach a governed-flow monitor (built from a `Flow` + `Mode`).
    pub fn flow_monitor(mut self, monitor: crate::flow::FlowMonitor) -> Self {
        self.flow = Some(monitor);
        self
    }

    /// Provide a pre-created State to use for this session.
    ///
    /// If not set, a new State is created at connect time. Use this when
    /// the State needs to be shared with tools or other components before
    /// the session connects.
    pub fn with_state(mut self, state: State) -> Self {
        self.state = Some(state);
        self
    }

    /// Set a greeting prompt sent on connect to trigger the model to speak first.
    pub fn greeting(mut self, prompt: impl Into<String>) -> Self {
        self.greeting = Some(prompt.into());
        self
    }

    /// Set the tool dispatcher for auto-dispatch of tool calls.
    pub fn dispatcher(mut self, dispatcher: ToolDispatcher) -> Self {
        // Add tool declarations to session config
        for tool in dispatcher.to_tool_declarations() {
            self.config = self.config.add_tool(tool);
        }
        self.dispatcher = Some(Arc::new(dispatcher));
        self
    }

    /// Set the event callbacks.
    pub fn callbacks(mut self, callbacks: EventCallbacks) -> Self {
        self.callbacks = callbacks;
        self
    }

    /// Add a turn extractor that runs between turns.
    pub fn extractor(mut self, extractor: Arc<dyn TurnExtractor>) -> Self {
        self.extractors.push(extractor);
        self
    }

    /// Set the computed variable registry for derived state.
    pub fn computed(mut self, registry: ComputedRegistry) -> Self {
        self.computed = Some(registry);
        self
    }

    /// Set the phase machine for declarative conversation phase management.
    pub fn phase_machine(mut self, machine: PhaseMachine) -> Self {
        self.phase_machine = Some(machine);
        self
    }

    /// Set the watcher registry for state change watchers.
    pub fn watchers(mut self, registry: WatcherRegistry) -> Self {
        self.watchers = Some(registry);
        self
    }

    /// Set the temporal pattern registry.
    pub fn temporal(mut self, registry: TemporalRegistry) -> Self {
        self.temporal = Some(registry);
        self
    }

    /// Set the execution mode for a named tool.
    ///
    /// Tools default to [`ToolExecutionMode::Standard`]. Set to
    /// [`ToolExecutionMode::Background`] for zero-dead-air execution.
    pub fn tool_execution_mode(
        mut self,
        tool_name: impl Into<String>,
        mode: ToolExecutionMode,
    ) -> Self {
        self.execution_modes.insert(tool_name.into(), mode);
        self
    }

    /// Enable soft turn detection for proactive silence awareness.
    ///
    /// When `proactiveAudio` is enabled, the model may choose not to respond.
    /// This sets a timeout after VAD end — if the model stays silent, a
    /// lightweight "soft turn" fires to keep state updated without forcing
    /// the model to speak.
    pub fn soft_turn_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.soft_turn_timeout = Some(timeout);
        self
    }

    /// Set the steering mode for how the phase machine delivers instructions.
    pub fn steering_mode(mut self, mode: SteeringMode) -> Self {
        self.steering_mode = mode;
        self
    }

    /// Set the context delivery timing.
    ///
    /// - `Immediate` (default): send batched context during TurnComplete.
    /// - `Deferred`: queue context and flush with next user send.
    pub fn context_delivery(mut self, mode: ContextDelivery) -> Self {
        self.context_delivery = mode;
        self
    }

    /// Set the fast-lane delivery (backpressure) policy per event class.
    ///
    /// Defaults to all-[`Lossless`](super::processor::Delivery::Lossless), which
    /// preserves the historical `send().await` routing behavior. Opt classes
    /// into [`LossyDropNewest`](super::processor::Delivery::LossyDropNewest) to
    /// keep the router from stalling when a fast-lane consumer falls behind.
    pub fn delivery(mut self, delivery: super::processor::DeliveryConfig) -> Self {
        self.delivery = delivery;
        self
    }

    /// Enable the conversation repair protocol.
    ///
    /// Tracks need fulfillment per phase and nudges the model when the
    /// conversation stalls on gathering required information.
    pub fn repair(mut self, config: RepairConfig) -> Self {
        self.repair_config = Some(config);
        self
    }

    /// Set a session persistence backend for surviving process restarts.
    pub fn persistence(mut self, backend: Arc<dyn SessionPersistence>) -> Self {
        self.persistence = Some(backend);
        self
    }

    /// Set the session ID for persistence.
    pub fn session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    /// Enable or disable tool availability advisory on phase transitions.
    pub fn tool_advisory(mut self, enabled: bool) -> Self {
        self.tool_advisory = enabled;
        self
    }

    /// Set the periodic telemetry emission interval.
    ///
    /// When set, the processor periodically emits `LiveEvent::Telemetry`
    /// and `LiveEvent::TurnMetrics` to the event stream.
    pub fn telemetry_interval(mut self, interval: std::time::Duration) -> Self {
        self.telemetry_interval = Some(interval);
        self
    }

    /// Connect to Gemini and start the three-lane event processor.
    ///
    /// This is a thin orchestrator over three explicit, behavior-preserving
    /// stages:
    ///
    /// 1. `into_plan` — pure derivation/validation of the resolved startup
    ///    configuration (`SessionPlan`); no I/O, no spawning.
    /// 2. Connect the L0 transport using the plan's resolved [`SessionConfig`].
    /// 3. `build_runtime` — assemble the runtime wiring (channels, shared
    ///    state, dispatcher, control plane) from the plan + connected session.
    /// 4. `spawn_lanes` — spawn the telemetry/event/tool lanes and return the
    ///    assembled [`LiveHandle`].
    pub async fn connect(self) -> Result<LiveHandle, AgentError> {
        let mut plan = self.into_plan()?;

        // Connect via L0 using the resolved config (taken out of the plan so
        // the rest of the plan can be moved into the runtime stage).
        let config = plan.config.take().expect("plan always carries a config");
        let session = ConnectBuilder::new(config)
            .build()
            .await
            .map_err(AgentError::Session)?;

        // Wait for Active phase
        session.wait_for_phase(SessionPhase::Active).await;

        let runtime = build_runtime(plan, session);
        spawn_lanes(runtime).await
    }

    /// Derive the resolved [`SessionPlan`] from this builder.
    ///
    /// This is a pure transformation: it runs build-time validations and
    /// resolves the startup configuration (notably applying `NonBlocking`
    /// behavior to background-tool declarations) without performing any I/O or
    /// spawning any tasks. It is unit-testable without a live connection.
    pub(crate) fn into_plan(self) -> Result<SessionPlan, AgentError> {
        // Build-time validations
        if let Some(ref pm) = self.phase_machine {
            pm.validate().map_err(AgentError::Config)?;
        }
        if let Some(ref computed) = self.computed {
            computed.validate().map_err(AgentError::Config)?;
        }

        // Apply NON_BLOCKING behavior to tool declarations for background tools
        let mut config = self.config;
        for (tool_name, mode) in &self.execution_modes {
            if matches!(
                mode,
                super::background_tool::ToolExecutionMode::Background { .. }
            ) {
                for tool in &mut config.tools {
                    if let Some(ref mut decls) = tool.function_declarations {
                        for decl in decls {
                            if decl.name == *tool_name {
                                decl.behavior = Some(
                                    gemini_genai_rs::prelude::FunctionCallingBehavior::NonBlocking,
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(SessionPlan {
            config: Some(config),
            callbacks: self.callbacks,
            dispatcher: self.dispatcher,
            extractors: self.extractors,
            computed: self.computed,
            phase_machine: self.phase_machine,
            watchers: self.watchers,
            temporal: self.temporal,
            greeting: self.greeting,
            state: self.state,
            execution_modes: self.execution_modes,
            soft_turn_timeout: self.soft_turn_timeout,
            steering_mode: self.steering_mode,
            context_delivery: self.context_delivery,
            delivery: self.delivery,
            repair_config: self.repair_config,
            persistence: self.persistence,
            session_id: self.session_id,
            tool_advisory: self.tool_advisory,
            telemetry_interval: self.telemetry_interval,
            middleware: self.middleware,
            flow: self.flow,
        })
    }
}

/// The resolved startup configuration for a Live session.
///
/// Produced purely from a [`LiveSessionBuilder`] via
/// [`into_plan`](LiveSessionBuilder::into_plan) — no I/O, no task spawning. The
/// `config` is held in an `Option` so [`connect`](LiveSessionBuilder::connect)
/// can take it out to open the transport while moving the remaining plan into
/// [`build_runtime`].
pub(crate) struct SessionPlan {
    /// Resolved session config (background-tool `NonBlocking` already applied).
    /// `Some` until the transport is opened; `connect` takes it out.
    config: Option<SessionConfig>,
    callbacks: EventCallbacks,
    dispatcher: Option<Arc<ToolDispatcher>>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<PhaseMachine>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<TemporalRegistry>,
    greeting: Option<String>,
    state: Option<State>,
    execution_modes: HashMap<String, ToolExecutionMode>,
    soft_turn_timeout: Option<std::time::Duration>,
    steering_mode: SteeringMode,
    context_delivery: ContextDelivery,
    delivery: super::processor::DeliveryConfig,
    repair_config: Option<RepairConfig>,
    persistence: Option<Arc<dyn SessionPersistence>>,
    session_id: Option<String>,
    tool_advisory: bool,
    telemetry_interval: Option<std::time::Duration>,
    middleware: Vec<Arc<dyn crate::middleware::Middleware>>,
    flow: Option<crate::flow::FlowMonitor>,
}

/// Fully wired runtime for a connected Live session, ready for lane spawning.
///
/// Produced by [`build_runtime`] from a [`SessionPlan`] plus the connected
/// [`SessionHandle`]. Holds the channels, shared atomics/state, dispatcher,
/// control-plane config, and the resolved writers — but spawns nothing. The
/// final stage [`spawn_lanes`] consumes this to start the lanes and assemble
/// the [`LiveHandle`].
pub(crate) struct SessionRuntime {
    session: SessionHandle,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<Arc<TemporalRegistry>>,
    greeting: Option<String>,
    state: State,
    execution_modes: HashMap<String, ToolExecutionMode>,
    background_tracker: Arc<BackgroundToolTracker>,
    telemetry: Arc<SessionTelemetry>,
    telemetry_interval: Option<std::time::Duration>,
    control_plane: ControlPlaneConfig,
    pending_context: Option<Arc<PendingContext>>,
    /// Writer used by the processor for internal sends.
    writer: Arc<dyn SessionWriter>,
    /// User-facing writer handed to the `LiveHandle` (and used for greeting).
    user_writer: Arc<dyn SessionWriter>,
    event_rx: tokio::sync::broadcast::Receiver<gemini_genai_rs::prelude::SessionEvent>,
    telem_rx: tokio::sync::broadcast::Receiver<gemini_genai_rs::prelude::SessionEvent>,
    on_usage_cb: Option<super::callbacks::UsageCallback>,
    live_event_tx: tokio::sync::broadcast::Sender<super::events::LiveEvent>,
    telem_cancel: CancellationToken,
    flow_monitor: Option<crate::flow::SharedFlowMonitor>,
}

/// Stage 3 input: construct the runtime wiring from a resolved plan and the
/// connected session. Builds channels, shared state, the dispatcher set, the
/// control-plane config (including deferred-context writer wrapping), and the
/// telemetry handle — but does not spawn any lanes.
pub(crate) fn build_runtime(plan: SessionPlan, session: SessionHandle) -> SessionRuntime {
    // Share the governed-flow monitor between the control lane (which
    // advances it) and the LiveHandle (which snapshots explain/why_blocked).
    let flow_monitor = plan.flow.map(crate::flow::FlowMonitor::into_shared);
    let mut callbacks = plan.callbacks;
    let on_usage_cb = callbacks.on_usage.take();
    let callbacks = Arc::new(callbacks);
    let raw_writer: Arc<dyn SessionWriter> = Arc::new(session.clone());
    let state = plan.state.unwrap_or_default();

    // Subscribe twice: one for router → fast/ctrl, one for telemetry lane
    let event_rx = session.subscribe();
    let telem_rx = session.subscribe();

    // Store initial phase's `needs` metadata for ContextBuilder.
    if let Some(ref pm) = plan.phase_machine {
        let _ = state.session().set("phase", pm.current());
        if let Some(phase) = pm.current_phase() {
            if !phase.needs.is_empty() {
                let _ = state.set("session:phase_needs", phase.needs.clone());
            }
        }
    }

    let phase_machine_mutex = plan.phase_machine.map(tokio::sync::Mutex::new);
    let temporal_arc = plan.temporal.map(Arc::new);
    let background_tracker = Arc::new(BackgroundToolTracker::new());

    // Create telemetry (auto-collected by the telemetry lane)
    let telemetry = Arc::new(SessionTelemetry::new());
    let telem_cancel = CancellationToken::new();

    // Build control plane config
    let mut control_plane = ControlPlaneConfig {
        soft_turn: plan.soft_turn_timeout.map(SoftTurnDetector::new),
        steering_mode: plan.steering_mode,
        context_delivery: plan.context_delivery,
        delivery: plan.delivery,
        needs_fulfillment: plan.repair_config.map(NeedsFulfillment::new),
        persistence: plan.persistence,
        session_id: plan.session_id,
        tool_advisory: plan.tool_advisory,
        pending_context: None, // set after PendingContext is created below
        middleware: {
            let mut chain = crate::middleware::MiddlewareChain::new();
            for layer in plan.middleware {
                chain.add(layer);
            }
            Arc::new(chain)
        },
        flow: flow_monitor.clone(),
    };

    // Create shared PendingContext for deferred delivery.
    // The SAME Arc is given to both the DeferredWriter (which drains it before
    // user sends) and the ControlPlaneConfig (which the processor uses to push
    // context turns from the control lane).
    let pending_context = if plan.context_delivery == ContextDelivery::Deferred {
        Some(Arc::new(PendingContext::new()))
    } else {
        None
    };

    // Wrap writer in DeferredWriter if deferred context delivery is enabled.
    let (writer, user_writer) = if let Some(ref pending) = pending_context {
        let deferred: Arc<dyn SessionWriter> =
            Arc::new(DeferredWriter::new(raw_writer.clone(), pending.clone()));
        // Processor uses raw_writer for internal sends (lifecycle context
        // goes through PendingContext, not through the writer directly).
        // User-facing LiveHandle uses the DeferredWriter.
        (raw_writer, deferred)
    } else {
        (raw_writer.clone(), raw_writer)
    };

    // Pass shared pending context to control plane config
    control_plane.pending_context = pending_context.clone();

    // Create LiveEvent broadcast channel
    use super::events::LiveEvent;
    use tokio::sync::broadcast;
    let (live_event_tx, _) = broadcast::channel::<LiveEvent>(4096);

    SessionRuntime {
        session,
        callbacks,
        dispatcher: plan.dispatcher,
        extractors: plan.extractors,
        computed: plan.computed,
        phase_machine: phase_machine_mutex,
        watchers: plan.watchers,
        temporal: temporal_arc,
        greeting: plan.greeting,
        state,
        execution_modes: plan.execution_modes,
        background_tracker,
        telemetry,
        telemetry_interval: plan.telemetry_interval,
        control_plane,
        pending_context,
        writer,
        user_writer,
        event_rx,
        telem_rx,
        on_usage_cb,
        live_event_tx,
        telem_cancel,
        flow_monitor,
    }
}

/// Stage 4: spawn the telemetry lane, event processor, and periodic telemetry
/// emitter, send any greeting, and assemble the [`LiveHandle`].
pub(crate) async fn spawn_lanes(rt: SessionRuntime) -> Result<LiveHandle, AgentError> {
    use super::events::LiveEvent;

    // Spawn telemetry lane (SessionSignals + SessionTelemetry on own broadcast rx)
    let session_signals = SessionSignals::new(rt.state.clone());
    let _telem_handle = spawn_telemetry_lane(
        rt.telem_rx,
        session_signals,
        rt.telemetry.clone(),
        rt.telem_cancel.clone(),
        rt.on_usage_cb,
    );

    // Spawn fast + control lanes (no session_signals, no transcript mutex)
    let greeting_writer = rt.user_writer.clone();
    let (fast_handle, ctrl_handle) = spawn_event_processor(
        rt.event_rx,
        rt.callbacks,
        rt.dispatcher,
        rt.writer,
        rt.extractors,
        rt.state.clone(),
        rt.computed,
        rt.phase_machine,
        rt.watchers,
        rt.temporal,
        Some(rt.background_tracker.clone()),
        rt.execution_modes,
        rt.control_plane,
        rt.live_event_tx.clone(),
    );

    // Spawn periodic telemetry emitter if interval is set
    if let Some(interval) = rt.telemetry_interval {
        let telem_tx = rt.live_event_tx.clone();
        let telem_ref = rt.telemetry.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            let mut prev_turns = 0u64;
            loop {
                tick.tick().await;
                let snap = telem_ref.snapshot();
                if let Some(obj) = snap.as_object() {
                    let tc = obj
                        .get("turn_count")
                        .or_else(|| obj.get("response_count"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if tc > prev_turns {
                        let latency = obj
                            .get("last_response_latency_ms")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        let prompt = obj
                            .get("prompt_token_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        let response = obj
                            .get("response_token_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        let _ = telem_tx.send(LiveEvent::TurnMetrics {
                            turn: tc as u32,
                            latency_ms: latency,
                            prompt_tokens: prompt,
                            response_tokens: response,
                        });
                        prev_turns = tc;
                    }
                }
                if telem_tx.send(LiveEvent::Telemetry(snap)).is_err() {
                    break;
                }
            }
        });
    }

    // Send greeting prompt to trigger model-initiated conversation
    if let Some(greeting) = rt.greeting {
        greeting_writer
            .send_text(greeting)
            .await
            .map_err(AgentError::Session)?;
    }

    Ok(LiveHandle::new(
        rt.session,
        rt.user_writer,
        fast_handle,
        ctrl_handle,
        rt.state,
        rt.telemetry,
        rt.live_event_tx,
        rt.pending_context,
        rt.flow_monitor,
        rt.background_tracker,
        rt.telem_cancel,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_with_defaults() {
        let config = SessionConfig::new("test-key");
        let builder = LiveSessionBuilder::new(config);
        assert!(builder.dispatcher.is_none());
        assert!(builder.computed.is_none());
        assert!(builder.phase_machine.is_none());
        assert!(builder.watchers.is_none());
        assert!(builder.temporal.is_none());
    }

    #[test]
    fn into_plan_derives_defaults() {
        let config = SessionConfig::new("test-key");
        let plan = LiveSessionBuilder::new(config)
            .into_plan()
            .expect("default builder should produce a plan");

        // Config is carried (taken out only at connect time).
        assert!(plan.config.is_some());
        // Defaults preserved.
        assert!(plan.dispatcher.is_none());
        assert!(plan.phase_machine.is_none());
        assert!(plan.persistence.is_none());
        assert!(plan.session_id.is_none());
        assert!(plan.greeting.is_none());
        assert!(plan.soft_turn_timeout.is_none());
        assert!(plan.telemetry_interval.is_none());
        assert!(plan.repair_config.is_none());
        assert!(plan.flow.is_none());
        assert!(plan.execution_modes.is_empty());
        assert!(plan.middleware.is_empty());
        assert_eq!(plan.steering_mode, SteeringMode::default());
        assert_eq!(plan.context_delivery, ContextDelivery::default());
        // Default builder enables tool advisory.
        assert!(plan.tool_advisory);
    }

    #[test]
    fn into_plan_carries_persistence_and_session_id() {
        let config = SessionConfig::new("test-key");
        let plan = LiveSessionBuilder::new(config)
            .session_id("user-123-session-456")
            .into_plan()
            .expect("plan derivation should succeed");

        assert_eq!(plan.session_id.as_deref(), Some("user-123-session-456"));
    }

    #[test]
    fn into_plan_carries_steering_and_context_delivery() {
        let config = SessionConfig::new("test-key");
        let plan = LiveSessionBuilder::new(config)
            .steering_mode(SteeringMode::ContextInjection)
            .context_delivery(ContextDelivery::Deferred)
            .tool_advisory(false)
            .into_plan()
            .expect("plan derivation should succeed");

        assert_eq!(plan.steering_mode, SteeringMode::ContextInjection);
        assert_eq!(plan.context_delivery, ContextDelivery::Deferred);
        assert!(!plan.tool_advisory);
    }

    #[test]
    fn into_plan_carries_greeting_and_telemetry_interval() {
        let config = SessionConfig::new("test-key");
        let plan = LiveSessionBuilder::new(config)
            .greeting("Hello there")
            .telemetry_interval(std::time::Duration::from_secs(5))
            .soft_turn_timeout(std::time::Duration::from_secs(2))
            .into_plan()
            .expect("plan derivation should succeed");

        assert_eq!(plan.greeting.as_deref(), Some("Hello there"));
        assert_eq!(
            plan.telemetry_interval,
            Some(std::time::Duration::from_secs(5))
        );
        assert_eq!(
            plan.soft_turn_timeout,
            Some(std::time::Duration::from_secs(2))
        );
    }

    #[test]
    fn into_plan_validates_phase_machine() {
        // A PhaseMachine whose initial phase doesn't exist must fail validation
        // during plan derivation (no connection required).
        let config = SessionConfig::new("test-key");
        let pm = PhaseMachine::new("nonexistent");
        let result = LiveSessionBuilder::new(config)
            .phase_machine(pm)
            .into_plan();
        assert!(result.is_err(), "invalid phase machine should fail to plan");
    }

    #[test]
    fn into_plan_carries_valid_phase_machine_and_seeds_nothing() {
        // A valid phase machine is carried into the plan; into_plan does NOT
        // seed state (that happens in build_runtime), so this stays I/O-free.
        let config = SessionConfig::new("test-key");
        let mut pm = PhaseMachine::new("start");
        pm.add_phase(crate::live::phase::Phase::new("start", "Start phase"));
        let plan = LiveSessionBuilder::new(config)
            .phase_machine(pm)
            .into_plan()
            .expect("valid phase machine should plan");

        assert!(plan.phase_machine.is_some());
    }

    #[test]
    fn into_plan_applies_non_blocking_to_background_tools() {
        use gemini_genai_rs::prelude::{FunctionCallingBehavior, FunctionDeclaration, Tool};

        let decl = FunctionDeclaration {
            name: "search_kb".into(),
            description: "Search".into(),
            parameters: None,
            behavior: None,
        };
        let config = SessionConfig::new("test-key").add_tool(Tool::functions(vec![decl]));

        let plan = LiveSessionBuilder::new(config)
            .tool_execution_mode(
                "search_kb",
                ToolExecutionMode::Background {
                    formatter: None,
                    scheduling: None,
                },
            )
            .into_plan()
            .expect("plan derivation should succeed");

        let cfg = plan.config.expect("config carried");
        let decl = cfg.tools[0]
            .function_declarations
            .as_ref()
            .unwrap()
            .iter()
            .find(|d| d.name == "search_kb")
            .unwrap();
        assert_eq!(decl.behavior, Some(FunctionCallingBehavior::NonBlocking));
    }
}
