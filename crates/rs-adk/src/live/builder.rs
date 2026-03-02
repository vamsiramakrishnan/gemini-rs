//! LiveSessionBuilder — combines SessionConfig + callbacks + tools into one setup.

use std::sync::Arc;

use rs_genai::prelude::{ConnectBuilder, SessionConfig, SessionPhase};
use rs_genai::session::SessionWriter;

use crate::error::AgentError;
use crate::state::State;
use crate::tool::ToolDispatcher;

use super::callbacks::EventCallbacks;
use super::extractor::TurnExtractor;
use super::handle::LiveHandle;
use super::processor::spawn_event_processor;
use super::transcript::TranscriptBuffer;

/// Builder for a callback-driven Live session.
pub struct LiveSessionBuilder {
    config: SessionConfig,
    callbacks: EventCallbacks,
    dispatcher: Option<Arc<ToolDispatcher>>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
}

impl LiveSessionBuilder {
    /// Create a new builder with the given session config.
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            callbacks: EventCallbacks::default(),
            dispatcher: None,
            extractors: Vec::new(),
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

    /// Connect to Gemini and start the event processor.
    pub async fn connect(self) -> Result<LiveHandle, AgentError> {
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

        // Create transcript buffer if extractors are registered
        let transcript_buffer = if !self.extractors.is_empty() {
            Some(Arc::new(parking_lot::Mutex::new(TranscriptBuffer::new())))
        } else {
            None
        };

        // Spawn two-lane processor
        let (fast_handle, ctrl_handle) = spawn_event_processor(
            event_rx,
            callbacks,
            self.dispatcher,
            writer,
            transcript_buffer,
            self.extractors,
            state.clone(),
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
    }
}
