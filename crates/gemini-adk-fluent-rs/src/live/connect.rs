//! Connection methods for `Live`.

use gemini_adk_rs::live::{LiveHandle, LiveSessionBuilder, PhaseMachine};
use gemini_adk_rs::State;
use gemini_genai_rs::prelude::*;

use super::Live;

impl Live {
    /// Connect using a Google AI API key.
    pub async fn connect_google_ai(
        mut self,
        api_key: impl Into<String>,
    ) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        self.config.endpoint = ApiEndpoint::google_ai(api_key);
        self.build_and_connect().await
    }

    /// Connect using Vertex AI credentials.
    pub async fn connect_vertex(
        mut self,
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        self.config.endpoint = ApiEndpoint::vertex(project, location, access_token);
        self.build_and_connect().await
    }

    /// Connect by resolving the platform and credentials from standard
    /// environment variables — the zero-ceremony entry point.
    ///
    /// Resolution (see [`ApiEndpoint::from_env`]):
    /// - `GOOGLE_GENAI_USE_VERTEXAI=true` → Vertex AI using
    ///   `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION` (default
    ///   `us-central1`), and a token from `GOOGLE_ACCESS_TOKEN`. If that
    ///   token is unset, this falls back to running
    ///   `gcloud auth print-access-token`.
    /// - otherwise → Google AI using `GEMINI_API_KEY` (or
    ///   `GOOGLE_GENAI_API_KEY` / `GOOGLE_API_KEY`).
    ///
    /// ```no_run
    /// # use gemini_adk_fluent_rs::prelude::*;
    /// # async fn run() -> Result<(), AgentError> {
    /// let handle = Live::builder()
    ///     .model(GeminiModel::Gemini2_0FlashLive)
    ///     .voice(Voice::Kore)
    ///     .connect_from_env()
    ///     .await?;
    /// # let _ = handle; Ok(())
    /// # }
    /// ```
    pub async fn connect_from_env(
        mut self,
    ) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        self.config.endpoint = resolve_endpoint_from_env()?;
        self.build_and_connect().await
    }

    /// Connect using a pre-configured SessionConfig for auth and model.
    ///
    /// Merges the provided config's `endpoint` and `model` into the builder's
    /// config, preserving system instruction, tools, voice, transcription, and
    /// all other settings configured via the fluent API.
    pub async fn connect(
        mut self,
        config: SessionConfig,
    ) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        // Merge auth/model from external config, keep everything else from builder.
        self.config.endpoint = config.endpoint;
        self.config.model = config.model;
        self.build_and_connect().await
    }

    async fn build_and_connect(mut self) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
        if uses_audio_output(&self.config) {
            self.config = self.config.voice_realtime_defaults();
        }

        let mut builder = LiveSessionBuilder::new(self.config);

        // Resolve deferred agent tools: create shared State, register TextAgentTools
        let mut dispatcher = self.dispatcher;
        if !self.deferred_agent_tools.is_empty() {
            let state = State::new();
            let d = dispatcher.get_or_insert_with(gemini_adk_rs::tool::ToolDispatcher::new);
            for deferred in self.deferred_agent_tools {
                d.register(gemini_adk_rs::TextAgentTool::from_arc(
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

/// Resolve an [`ApiEndpoint`] from the environment, with a `gcloud` token
/// fallback for Vertex AI when `GOOGLE_ACCESS_TOKEN` is not set.
fn resolve_endpoint_from_env() -> Result<ApiEndpoint, gemini_adk_rs::error::AgentError> {
    use gemini_adk_rs::error::AgentError;
    use gemini_genai_rs::protocol::types::EndpointEnvError;

    match ApiEndpoint::from_env() {
        Ok(endpoint) => Ok(endpoint),
        // Vertex was selected but no token was in the environment — fall back
        // to Application Default Credentials via the gcloud CLI.
        Err(EndpointEnvError::Missing("GOOGLE_ACCESS_TOKEN")) => {
            let project = std::env::var("GOOGLE_CLOUD_PROJECT").map_err(|_| {
                AgentError::Config("GOOGLE_CLOUD_PROJECT is required for Vertex AI".into())
            })?;
            let location = std::env::var("GOOGLE_CLOUD_LOCATION")
                .unwrap_or_else(|_| "us-central1".to_string());
            let token = gcloud_access_token()?;
            Ok(ApiEndpoint::vertex(project, location, token))
        }
        Err(e) => Err(AgentError::Config(format!(
            "connect_from_env: {e}. For Google AI set GEMINI_API_KEY; for Vertex AI set \
             GOOGLE_GENAI_USE_VERTEXAI=true and GOOGLE_CLOUD_PROJECT (token via \
             GOOGLE_ACCESS_TOKEN or the gcloud CLI)."
        ))),
    }
}

/// Fetch an OAuth2 access token via `gcloud auth print-access-token`.
fn gcloud_access_token() -> Result<String, gemini_adk_rs::error::AgentError> {
    use gemini_adk_rs::error::AgentError;

    let output = std::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
        .map_err(|e| {
            AgentError::Config(format!(
                "Vertex AI needs an access token: set GOOGLE_ACCESS_TOKEN, or install the \
                 gcloud CLI (failed to run `gcloud auth print-access-token`: {e})"
            ))
        })?;
    if !output.status.success() {
        return Err(AgentError::Config(format!(
            "`gcloud auth print-access-token` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err(AgentError::Config(
            "`gcloud auth print-access-token` returned an empty token".into(),
        ));
    }
    Ok(token)
}

fn uses_audio_output(config: &SessionConfig) -> bool {
    config
        .generation_config
        .response_modalities
        .as_ref()
        .map(|modalities| modalities.iter().any(|m| matches!(m, Modality::Audio)))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_audio_output_defaults_to_audio() {
        let config = SessionConfig::new("key");
        assert!(uses_audio_output(&config));
    }

    #[test]
    fn uses_audio_output_respects_text_only() {
        let config = SessionConfig::new("key").text_only();
        assert!(!uses_audio_output(&config));
    }
}
