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
use super::extractor::TurnExtractor;
use super::handle::LiveHandle;
use super::phase::PhaseMachine;
use super::processor::{spawn_event_processor, spawn_telemetry_lane};
use super::session_signals::SessionSignals;
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

    /// Connect to Gemini and start the three-lane event processor.
    pub async fn connect(self) -> Result<LiveHandle, AgentError> {
        // Build-time validations
        if let Some(ref pm) = self.phase_machine {
            pm.validate().map_err(AgentError::Config)?;
        }
        if let Some(ref computed) = self.computed {
            computed.validate().map_err(AgentError::Config)?;
        }

        // Connect via L0
        let session = ConnectBuilder::new(self.config)
            .build()
            .await
            .map_err(AgentError::Session)?;

        // Wait for Active phase
        session.wait_for_phase(SessionPhase::Active).await;

        let callbacks = Arc::new(self.callbacks);
        let writer: Arc<dyn SessionWriter> = Arc::new(session.clone());
        let state = self.state.unwrap_or_else(State::new);

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
        );

        // Spawn fast + control lanes (no session_signals, no transcript mutex)
        let greeting_writer = writer.clone();
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
        );

        // Send greeting prompt to trigger model-initiated conversation
        if let Some(greeting) = self.greeting {
            greeting_writer.send_text(greeting).await.map_err(AgentError::Session)?;
        }

        Ok(LiveHandle::new(session, fast_handle, ctrl_handle, state, telemetry))
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
