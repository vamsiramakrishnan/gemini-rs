//! LiveSessionBuilder — combines SessionConfig + callbacks + tools into one setup.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use rs_genai::prelude::{ConnectBuilder, SessionConfig, SessionPhase};
use rs_genai::session::SessionWriter;

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
/// For ergonomic usage, prefer the L2 `Live` builder from `adk-rs-fluent`
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
    repair_config: Option<RepairConfig>,
    persistence: Option<Arc<dyn SessionPersistence>>,
    session_id: Option<String>,
    tool_advisory: bool,
    telemetry_interval: Option<std::time::Duration>,
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
            repair_config: None,
            persistence: None,
            session_id: None,
            tool_advisory: true,
            telemetry_interval: None,
        }
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
    pub async fn connect(self) -> Result<LiveHandle, AgentError> {
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
                                decl.behavior =
                                    Some(rs_genai::prelude::FunctionCallingBehavior::NonBlocking);
                            }
                        }
                    }
                }
            }
        }

        // Connect via L0
        let session = ConnectBuilder::new(config)
            .build()
            .await
            .map_err(AgentError::Session)?;

        // Wait for Active phase
        session.wait_for_phase(SessionPhase::Active).await;

        let mut callbacks = self.callbacks;
        let on_usage_cb = callbacks.on_usage.take();
        let callbacks = Arc::new(callbacks);
        let raw_writer: Arc<dyn SessionWriter> = Arc::new(session.clone());
        let state = self.state.unwrap_or_default();

        // Subscribe twice: one for router → fast/ctrl, one for telemetry lane
        let event_rx = session.subscribe();
        let telem_rx = session.subscribe();

        // Store initial phase's `needs` metadata for ContextBuilder.
        if let Some(ref pm) = self.phase_machine {
            if let Some(phase) = pm.current_phase() {
                if !phase.needs.is_empty() {
                    state.set("session:phase_needs", phase.needs.clone());
                }
            }
        }

        let phase_machine_mutex = self.phase_machine.map(tokio::sync::Mutex::new);
        let temporal_arc = self.temporal.map(Arc::new);
        let background_tracker = Arc::new(BackgroundToolTracker::new());

        // Create telemetry (auto-collected by the telemetry lane)
        let telemetry = Arc::new(SessionTelemetry::new());
        let telem_cancel = CancellationToken::new();

        // Spawn telemetry lane (SessionSignals + SessionTelemetry on own broadcast rx)
        let session_signals = SessionSignals::new(state.clone());
        let _telem_handle = spawn_telemetry_lane(
            telem_rx,
            session_signals,
            telemetry.clone(),
            telem_cancel.clone(),
            on_usage_cb,
        );

        // Build control plane config
        let mut control_plane = ControlPlaneConfig {
            soft_turn: self.soft_turn_timeout.map(SoftTurnDetector::new),
            steering_mode: self.steering_mode,
            needs_fulfillment: self.repair_config.map(NeedsFulfillment::new),
            persistence: self.persistence,
            session_id: self.session_id,
            tool_advisory: self.tool_advisory,
            pending_context: None, // set after PendingContext is created below
        };

        // Create shared PendingContext for deferred delivery.
        // The SAME Arc is given to both the DeferredWriter (which drains it before
        // user sends) and the ControlPlaneConfig (which the processor uses to push
        // context turns from the control lane).
        let pending_context = if self.context_delivery == ContextDelivery::Deferred {
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
        control_plane.pending_context = pending_context;

        // Create LiveEvent broadcast channel
        use super::events::LiveEvent;
        use tokio::sync::broadcast;
        let (live_event_tx, _) = broadcast::channel::<LiveEvent>(4096);

        // Spawn fast + control lanes (no session_signals, no transcript mutex)
        let greeting_writer = user_writer.clone();
        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            self.dispatcher,
            writer,
            self.extractors,
            state.clone(),
            self.computed,
            phase_machine_mutex,
            self.watchers,
            temporal_arc,
            Some(background_tracker),
            self.execution_modes,
            control_plane,
            live_event_tx.clone(),
        );

        // Spawn periodic telemetry emitter if interval is set
        if let Some(interval) = self.telemetry_interval {
            let telem_tx = live_event_tx.clone();
            let telem_ref = telemetry.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(interval);
                let mut prev_turns = 0u64;
                loop {
                    tick.tick().await;
                    let snap = telem_ref.snapshot();
                    if let Some(obj) = snap.as_object() {
                        let tc = obj.get("turn_count").and_then(|v| v.as_u64()).unwrap_or(0);
                        if tc > prev_turns {
                            let latency = obj
                                .get("last_latency_ms")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32;
                            let prompt = obj
                                .get("prompt_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32;
                            let response = obj
                                .get("response_tokens")
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
        if let Some(greeting) = self.greeting {
            greeting_writer
                .send_text(greeting)
                .await
                .map_err(AgentError::Session)?;
        }

        Ok(LiveHandle::new(
            session,
            user_writer,
            fast_handle,
            ctrl_handle,
            state,
            telemetry,
            live_event_tx,
        ))
    }
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
}
