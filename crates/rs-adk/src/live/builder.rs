//! LiveSessionBuilder — combines SessionConfig + callbacks + tools into one setup.

use std::sync::Arc;

use rs_genai::prelude::{ConnectBuilder, SessionConfig, SessionPhase};
use rs_genai::session::SessionWriter;

use crate::error::AgentError;
use crate::state::State;
use crate::tool::ToolDispatcher;

use super::background_tool::BackgroundToolTracker;
use super::callbacks::EventCallbacks;
use super::computed::ComputedRegistry;
use super::extractor::TurnExtractor;
use super::handle::LiveHandle;
use super::phase::PhaseMachine;
use super::processor::spawn_event_processor;
use super::session_signals::SessionSignals;
use super::temporal::TemporalRegistry;
use super::transcript::TranscriptBuffer;
use super::watcher::WatcherRegistry;

/// Builder for a callback-driven Live session.
pub struct LiveSessionBuilder {
    config: SessionConfig,
    callbacks: EventCallbacks,
    dispatcher: Option<Arc<ToolDispatcher>>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<PhaseMachine>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<TemporalRegistry>,
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
        }
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

    /// Connect to Gemini and start the event processor.
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
        let event_rx = session.subscribe();
        let state = State::new();

        let session_signals = SessionSignals::new(state.clone());

        // Always create transcript buffer (for server transcription support)
        let transcript_buffer =
            Some(Arc::new(parking_lot::Mutex::new(TranscriptBuffer::new())));

        let phase_machine_mutex = self.phase_machine.map(tokio::sync::Mutex::new);
        let temporal_arc = self.temporal.map(Arc::new);
        let background_tracker = Arc::new(BackgroundToolTracker::new());

        // Spawn two-lane processor
        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            self.dispatcher,
            writer,
            transcript_buffer,
            self.extractors,
            state.clone(),
            Some(session_signals),
            self.computed,
            phase_machine_mutex,
            self.watchers,
            temporal_arc,
            Some(background_tracker),
        );

        Ok(LiveHandle::new(session, fast_handle, ctrl_handle, state))
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
