//! Connection methods for `Live`.

use gemini_adk_rs::live::{LiveHandle, LiveSessionBuilder, PhaseMachine};
use gemini_genai_rs::prelude::*;

use super::Live;

/// Fold builder-registered ambient tools into the flow's own list.
///
/// Idempotent and duplicate-free, because an application may name a tool the
/// flow already declares — or an extension may be installed twice.
pub(crate) fn merge_ambient(flow: &mut gemini_adk_rs::flow::Flow, ambient: &[String]) {
    for tool in ambient {
        if !flow.ambient.contains(tool) {
            flow.ambient.push(tool.clone());
        }
    }
}

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

        // Client interruption authority: the server must stop deciding, so
        // its automatic activity detection is disabled in the setup message
        // (the input VAD's activity marks take over in `send_audio`).
        if self.input_audio.client_authority {
            self.config =
                self.config
                    .server_vad(gemini_genai_rs::prelude::AutomaticActivityDetection {
                        disabled: Some(true),
                        start_of_speech_sensitivity: None,
                        end_of_speech_sensitivity: None,
                        prefix_padding_ms: None,
                        silence_duration_ms: None,
                    });
        }
        let input_audio = std::mem::take(&mut self.input_audio);

        // Resolve a `.record_wire(path)` request into a FileWireRecorder now
        // that we are actually connecting.
        if let Some(path) = self.record_wire_path.take() {
            let recorder = FileWireRecorder::create(&path).map_err(|e| {
                gemini_adk_rs::error::AgentError::Config(format!(
                    "failed to create wire log at {}: {e}",
                    path.display()
                ))
            })?;
            self.config = self.config.record_wire(std::sync::Arc::new(recorder));
        }

        // Config-level tool declarations, captured before `config` moves.
        let builder_config_tools = self.config.tools.clone();
        let mut builder = LiveSessionBuilder::new(self.config);

        // The session's `State`. A caller-supplied one is used as-is so tools
        // they already built around it write where the flow monitor and phase
        // machine read; otherwise agent tools get a fresh one as before.
        let shared_state = self.state.clone();
        if let Some(ref state) = shared_state {
            builder = builder.with_state(state.clone());
        }

        // Resolve deferred agent tools: register TextAgentTools against it.
        let mut dispatcher = self.dispatcher;
        if !self.deferred_agent_tools.is_empty() {
            let state = shared_state.clone().unwrap_or_default();
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

        // Resolve deferred async tools (MCP connections, etc.).
        if !self.deferred_tools.is_empty() {
            let d = dispatcher.get_or_insert_with(gemini_adk_rs::tool::ToolDispatcher::new);
            for deferred in std::mem::take(&mut self.deferred_tools) {
                resolve_deferred_tool(deferred, d).await?;
            }
        }

        // Attach the confirmation provider so `T::confirm(..)` tools are gated.
        if let Some(provider) = self.confirmation_provider {
            dispatcher
                .get_or_insert_with(gemini_adk_rs::tool::ToolDispatcher::new)
                .set_confirmation_provider(provider);
        }

        // Capture the resolved tool names before the dispatcher moves into the
        // builder. This is the only point where the set is complete: MCP/A2A/
        // OpenAPI tools exist only after the handshakes above.
        let resolved_tool_names: Vec<String> = {
            let mut names = super::introspect::declaration_names(&builder_config_tools);
            if let Some(d) = &dispatcher {
                names.extend(super::introspect::declaration_names(
                    &d.to_tool_declarations(),
                ));
            }
            names.sort();
            names.dedup();
            names
        };

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
        builder = builder.delivery(self.delivery);
        if let Some(redactor) = self.redactor {
            builder = builder.redaction(redactor);
        }
        if let Some(config) = self.repair_config {
            builder = builder.repair(config);
        }
        if let Some(p) = self.persistence {
            builder = builder.persistence(p);
        }
        if let Some(id) = self.session_id {
            builder = builder.session_id(id);
        }
        for layer in self.middleware_layers {
            builder = builder.middleware(layer);
        }
        if let Some(mut flow) = self.flow {
            // Merged here rather than in `govern`/`ambient_tools` so the two
            // compose regardless of the order the caller wrote them in.
            merge_ambient(&mut flow, &self.ambient_tools);
            // A flow names tools as strings, and a name that matches nothing is
            // not inert: an `allow` whitelist containing only a typo denies
            // every tool for as long as that step is active, silently and for
            // the rest of the session. The registry to check against only
            // exists here, after the deferred handshakes above.
            //
            // Skipped for `govern_compiled`/`observe_compiled`, whose contract
            // is that the caller already surfaced these diagnostics via
            // `Flow::compile`/`compile_with_tools` and connect will not repeat
            // the work.
            if !self.flow_precompiled {
                let registry: Vec<&str> = resolved_tool_names.iter().map(String::as_str).collect();
                flow.clone()
                    .compile_with_tools(&registry)
                    .map_err(|errors| {
                        gemini_adk_rs::error::AgentError::Config(format!(
                            "governing flow does not match this session's tools: {errors}. \
                             Registered tools: [{}]. Use `Flow::compile_with_tools` at load \
                             time and `govern_compiled` to check this yourself.",
                            registry.join(", ")
                        ))
                    })?;
            }
            let mut monitor = gemini_adk_rs::flow::FlowMonitor::new(flow, self.flow_mode);
            for (step, agent, mode) in self.flow_actions {
                monitor = monitor.on_enter(step, gemini_adk_rs::flow::run(agent, mode));
            }
            builder = builder.flow_monitor(monitor);
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

        let handle = builder.connect().await?;

        // Input-audio hardening: materialize the configured stages (in
        // order) and hand them plus VAD tuning and authority to the handle.
        if input_audio.is_configured() {
            let (processors, vad, client_authority) = input_audio.build_processors();
            handle.configure_input_audio(
                processors,
                vad,
                if client_authority {
                    gemini_adk_rs::live::ActivityAuthority::Client
                } else {
                    gemini_adk_rs::live::ActivityAuthority::Server
                },
            );
        }
        Ok(handle)
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

/// Resolve a single [`DeferredTool`](crate::compose::tools::DeferredTool) into
/// concrete tool registrations on the dispatcher. Runs at connect time because
/// these tools require async I/O (a network call or a subprocess handshake).
async fn resolve_deferred_tool(
    tool: crate::compose::tools::DeferredTool,
    dispatcher: &mut gemini_adk_rs::tool::ToolDispatcher,
) -> Result<(), gemini_adk_rs::error::AgentError> {
    use crate::compose::tools::DeferredTool;
    use gemini_adk_rs::error::AgentError;
    use gemini_adk_rs::tools::mcp::{McpSessionManager, McpTool};
    use std::sync::Arc;

    match tool {
        DeferredTool::Mcp { params } => {
            let manager = Arc::new(McpSessionManager::new(parse_mcp_params(&params)));
            let infos = manager.list_tools().await.map_err(|e| {
                AgentError::Config(format!("MCP tool discovery failed for {params:?}: {e}"))
            })?;
            for info in infos {
                dispatcher.register_function(Arc::new(McpTool::new(
                    info.name,
                    info.description,
                    Some(info.input_schema),
                    manager.clone(),
                )));
            }
            Ok(())
        }
        // The following are part of the ADK-parity toolset roadmap; they are
        // surfaced as explicit connect-time errors rather than silently dropped.
        DeferredTool::A2a { url, skill } => Err(AgentError::Config(format!(
            "T::a2a(url={url:?}, skill={skill:?}) is not yet implemented; tracked for ADK parity"
        ))),
        DeferredTool::OpenApi { name, spec_url } => Err(AgentError::Config(format!(
            "T::openapi(name={name:?}, spec_url={spec_url:?}) is not yet implemented; \
             tracked for ADK parity"
        ))),
        DeferredTool::Search { name, .. } => Err(AgentError::Config(format!(
            "T::search(name={name:?}) is not yet implemented; tracked for ADK parity"
        ))),
    }
}

/// Parse an MCP connection string: an `http(s)://` URL becomes an SSE/HTTP
/// connection, anything else is treated as a stdio command line.
fn parse_mcp_params(params: &str) -> gemini_adk_rs::tools::mcp::McpConnectionParams {
    use gemini_adk_rs::tools::mcp::McpConnectionParams;

    let trimmed = params.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        McpConnectionParams::Sse {
            url: trimmed.to_string(),
            headers: None,
        }
    } else {
        let mut parts = trimmed.split_whitespace();
        let command = parts.next().unwrap_or_default().to_string();
        let args = parts.map(str::to_string).collect();
        McpConnectionParams::Stdio {
            command,
            args,
            timeout: Some(std::time::Duration::from_secs(30)),
        }
    }
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

    // ─── ambient tool merge ─────────────────────────────────────────────────

    use gemini_adk_rs::flow::{Flow, Guard};

    fn whitelisting_flow() -> Flow {
        Flow::new()
            .step("book")
            .allow(["book_table"])
            .done(Guard::called_ok("book_table"))
            .build()
            .expect("flow is structurally valid")
    }

    #[test]
    fn merge_ambient_adds_registered_tools() {
        let mut flow = whitelisting_flow();
        merge_ambient(&mut flow, &["recall_context".to_string()]);
        assert_eq!(flow.ambient, ["recall_context"]);
    }

    #[test]
    fn merge_ambient_does_not_duplicate() {
        // The flow already declares it and an extension registers it too.
        let mut flow = Flow::new()
            .ambient(["recall_context"])
            .step("book")
            .allow(["book_table"])
            .done(Guard::called_ok("book_table"))
            .build()
            .expect("flow is structurally valid");
        merge_ambient(&mut flow, &["recall_context".to_string()]);
        merge_ambient(&mut flow, &["recall_context".to_string()]);
        assert_eq!(
            flow.ambient,
            ["recall_context"],
            "merging is idempotent, so an extension installed twice is harmless"
        );
    }

    // ─── connect-time flow validation ───────────────────────────────────────
    //
    // These run to a real `connect_google_ai` with a junk key. That is safe and
    // deliberate: validation happens before `builder.connect()`, so a `Config`
    // error proves the check fired *and* that it fired before any socket was
    // opened. A `Session` error would mean the flow was accepted and the
    // failure came from the network instead.

    fn book_tool() -> crate::compose::tools::ToolComposite {
        crate::compose::T::simple("book_table", "Book a table", |_| async {
            Ok(serde_json::json!({"ok": true}))
        })
    }

    #[tokio::test]
    async fn a_flow_naming_an_unregistered_tool_is_refused_at_connect() {
        let flow = Flow::new()
            .step("book")
            .allow(["book_tabel"]) // typo: the registered tool is `book_table`
            .done(Guard::called_ok("book_tabel"))
            .build()
            .expect("structurally valid — the name is the problem, not the shape");

        let err = Live::builder()
            .with_tools(book_tool())
            .govern(flow)
            .connect_google_ai("not-a-real-key")
            .await
            .err()
            .expect("a flow naming a tool that does not exist must not connect");

        let msg = err.to_string();
        assert!(
            msg.contains("book_tabel"),
            "the error must name the tool that does not exist: {msg}"
        );
        assert!(
            msg.contains("book_table"),
            "and list what is registered, so the typo is visible: {msg}"
        );
    }

    #[tokio::test]
    async fn a_flow_matching_the_registered_tools_passes_validation() {
        // Reaches the network and fails there — which is the proof that the
        // flow check passed rather than short-circuiting.
        let flow = Flow::new()
            .step("book")
            .allow(["book_table"])
            .done(Guard::called_ok("book_table"))
            .build()
            .expect("valid");

        let err = Live::builder()
            .with_tools(book_tool())
            .govern(flow)
            .connect_google_ai("not-a-real-key")
            .await
            .err()
            .expect("the key is junk, so this still fails — just not on the flow");

        assert!(
            !err.to_string().contains("governing flow"),
            "a flow whose tools all exist must clear validation: {err}"
        );
    }

    #[tokio::test]
    async fn ambient_tools_count_as_registered_for_validation() {
        // `ambient` joins the tool universe, so an ambient tool that is really
        // registered must satisfy the check rather than trip it.
        let flow = Flow::new()
            .ambient(["book_table"])
            .step("book")
            .done(Guard::called_ok("book_table"))
            .build()
            .expect("valid");

        let err = Live::builder()
            .with_tools(book_tool())
            .govern(flow)
            .connect_google_ai("not-a-real-key")
            .await
            .err()
            .expect("junk key");

        assert!(
            !err.to_string().contains("governing flow"),
            "an ambient tool that exists must not be reported missing: {err}"
        );
    }

    #[tokio::test]
    async fn a_precompiled_flow_is_not_revalidated() {
        // `govern_compiled` documents that connect does not re-check. Honour it
        // even when the flow would fail the check, or the documented
        // compile-once-govern-many path would silently stop working.
        let compiled = Flow::new()
            .step("book")
            .allow(["book_tabel"])
            .done(Guard::called_ok("book_tabel"))
            .build()
            .expect("valid shape")
            .compile()
            .expect("compiles without a tool registry");

        let err = Live::builder()
            .with_tools(book_tool())
            .govern_compiled(compiled)
            .connect_google_ai("not-a-real-key")
            .await
            .err()
            .expect("junk key");

        assert!(
            !err.to_string().contains("governing flow"),
            "a pre-compiled flow must not be re-validated at connect: {err}"
        );
    }

    #[test]
    fn ambient_tools_registers_regardless_of_govern_order() {
        // The whole reason the merge happens at connect: an application may
        // write `.govern(..)` before or after the extension that needs ambient
        // tools, and neither order may lose the registration.
        let before = Live::builder()
            .govern(whitelisting_flow())
            .ambient_tools(["recall_context"]);
        let after = Live::builder()
            .ambient_tools(["recall_context"])
            .govern(whitelisting_flow());
        assert_eq!(before.ambient_tool_names(), ["recall_context"]);
        assert_eq!(after.ambient_tool_names(), ["recall_context"]);
    }
}
