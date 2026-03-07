//! Connection methods for `Live`.

use rs_adk::live::{LiveHandle, LiveSessionBuilder, PhaseMachine};
use rs_adk::State;
use rs_genai::prelude::*;

use super::Live;

impl Live {
    /// Connect using a Google AI API key.
    pub async fn connect_google_ai(
        mut self,
        api_key: impl Into<String>,
    ) -> Result<LiveHandle, rs_adk::error::AgentError> {
        self.config.endpoint = ApiEndpoint::google_ai(api_key);
        self.build_and_connect().await
    }

    /// Connect using Vertex AI credentials.
    pub async fn connect_vertex(
        mut self,
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Result<LiveHandle, rs_adk::error::AgentError> {
        self.config.endpoint = ApiEndpoint::vertex(project, location, access_token);
        self.build_and_connect().await
    }

    /// Connect using a pre-configured SessionConfig (advanced).
    pub async fn connect(
        mut self,
        config: SessionConfig,
    ) -> Result<LiveHandle, rs_adk::error::AgentError> {
        self.config = config;
        self.build_and_connect().await
    }

    async fn build_and_connect(self) -> Result<LiveHandle, rs_adk::error::AgentError> {
        let mut builder = LiveSessionBuilder::new(self.config);

        // Resolve deferred agent tools: create shared State, register TextAgentTools
        let mut dispatcher = self.dispatcher;
        if !self.deferred_agent_tools.is_empty() {
            let state = State::new();
            let d = dispatcher.get_or_insert_with(rs_adk::tool::ToolDispatcher::new);
            for deferred in self.deferred_agent_tools {
                d.register(rs_adk::TextAgentTool::from_arc(
                    deferred.name,
                    deferred.description,
                    deferred.agent,
                    state.clone(),
                ));
            }
            builder = builder.with_state(state);
        }

        if let Some(dispatcher) = dispatcher {
            builder = builder.dispatcher(dispatcher);
        }
        if let Some(greeting) = self.greeting {
            builder = builder.greeting(greeting);
        }
        builder = builder.callbacks(self.callbacks);
        for ext in self.extractors {
            builder = builder.extractor(ext);
        }

        // Pass L1 registries
        if !self.computed.is_empty() {
            builder = builder.computed(self.computed);
        }
        if let Some(initial) = self.initial_phase {
            let mut pm = PhaseMachine::new(&initial);
            for phase in self.phases {
                pm.add_phase(phase);
            }
            builder = builder.phase_machine(pm);
        }
        if !self.watchers.observed_keys().is_empty() {
            builder = builder.watchers(self.watchers);
        }
        builder = builder.temporal(self.temporal);

        // Pass tool execution modes
        for (name, mode) in self.tool_execution_modes {
            builder = builder.tool_execution_mode(name, mode);
        }

        // Pass control plane configuration
        if let Some(timeout) = self.soft_turn_timeout {
            builder = builder.soft_turn_timeout(timeout);
        }
        builder = builder.steering_mode(self.steering_mode);
        builder = builder.context_delivery(self.context_delivery);
        if let Some(config) = self.repair_config {
            builder = builder.repair(config);
        }
        if let Some(p) = self.persistence {
            builder = builder.persistence(p);
        }
        if let Some(id) = self.session_id {
            builder = builder.session_id(id);
        }
        builder = builder.tool_advisory(self.tool_advisory);
        if let Some(interval) = self.telemetry_interval {
            builder = builder.telemetry_interval(interval);
        }

        // Spawn fire-and-forget warm-up tasks for OOB LLMs
        // (pre-establishes TCP+TLS so first extract call is fast)
        for llm in self.warm_up_llms {
            tokio::spawn(async move {
                let _ = llm.warm_up().await;
            });
        }

        builder.connect().await
    }
}
