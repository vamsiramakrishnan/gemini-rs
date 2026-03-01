//! High-level agent builder — eliminates boilerplate with a fluent API.
//!
//! [`GeminiAgent`] wraps the low-level [`SessionHandle`] with:
//! - A builder that configures session, tools, and callbacks in one chain
//! - Automatic tool dispatch (no manual `ToolCall` event handling)
//! - Callback-based event routing (no manual `subscribe` + `match` loop)
//! - **Context engineering** via [`ContextPolicy`](crate::context::ContextPolicy)
//! - **Prompt engineering** via [`SystemPrompt`](crate::prompt::SystemPrompt)
//! - **State engineering** via [`StatePolicy`](crate::state::StatePolicy)
//!
//! # Example
//!
//! ```rust,no_run
//! use gemini_live_rs::prelude::*;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let agent = GeminiAgent::builder()
//!     .api_key("YOUR_API_KEY")
//!     .voice(Voice::Kore)
//!     .system_instruction("You are a helpful assistant.")
//!     .on_text(|t| async move { print!("{t}") })
//!     .on_turn_complete(|_| async move { println!("\n---") })
//!     .build()
//!     .await?;
//!
//! agent.send_text("Hello!").await?;
//! agent.wait_until_done().await;
//! # Ok(())
//! # }
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::agent::FunctionRegistry;
use crate::context::{ContextManager, ContextPolicy, InjectionTrigger};
use crate::flow::{BargeInConfig, TurnDetectionConfig};
use crate::prompt::SystemPrompt;
use crate::protocol::{ApiEndpoint, GeminiModel, Modality, SessionConfig, ToolConfig, Voice};
use crate::session::{
    SessionCommand, SessionError, SessionEvent, SessionHandle, SessionPhase, Turn,
};
use crate::state::{StateManager, StatePolicy};
use crate::transport::{connect, TransportConfig};

#[cfg(feature = "vad")]
use crate::vad::VadConfig;

use crate::buffer::JitterConfig;

// ---------------------------------------------------------------------------
// Callback type
// ---------------------------------------------------------------------------

type BoxCallback<T> =
    Arc<dyn Fn(T) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

// ---------------------------------------------------------------------------
// Pipeline configuration
// ---------------------------------------------------------------------------

/// Aggregated audio pipeline configuration, collected by the builder.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// VAD configuration (None = VAD disabled).
    #[cfg(feature = "vad")]
    pub vad_config: Option<VadConfig>,
    /// Barge-in detection configuration.
    pub barge_in_config: BargeInConfig,
    /// Turn detection configuration.
    pub turn_detection_config: TurnDetectionConfig,
    /// Jitter buffer configuration.
    pub jitter_config: JitterConfig,
    /// Input audio sample rate (Hz).
    pub input_sample_rate: u32,
    /// Output audio sample rate (Hz).
    pub output_sample_rate: u32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            #[cfg(feature = "vad")]
            vad_config: Some(VadConfig::default()),
            barge_in_config: BargeInConfig::default(),
            turn_detection_config: TurnDetectionConfig::default(),
            jitter_config: JitterConfig::default(),
            input_sample_rate: 16_000,
            output_sample_rate: 24_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Event router
// ---------------------------------------------------------------------------

/// Internal event router — replaces the manual event loop that every example duplicates.
/// Spawned as a single background Tokio task.
#[derive(Default)]
struct EventRouter {
    on_text_delta: Option<BoxCallback<String>>,
    on_text_complete: Option<BoxCallback<String>>,
    on_audio_data: Option<BoxCallback<Vec<u8>>>,
    on_input_transcription: Option<BoxCallback<String>>,
    on_output_transcription: Option<BoxCallback<String>>,
    on_turn_complete: Option<BoxCallback<Turn>>,
    on_interrupted: Option<BoxCallback<()>>,
    on_error: Option<BoxCallback<String>>,
    on_connected: Option<BoxCallback<()>>,
    on_disconnected: Option<BoxCallback<Option<String>>>,
}

impl EventRouter {
    /// Spawn the router as a background task.
    ///
    /// When `state_mgr` is provided, events are automatically processed through
    /// the state policy (transforms fire on matching events). When `context_mgr`
    /// is provided, turn completions are recorded and context injections are
    /// resolved at appropriate trigger points.
    fn spawn(
        self,
        handle: SessionHandle,
        registry: Option<Arc<FunctionRegistry>>,
        mut state_mgr: Option<StateManager>,
        mut context_mgr: Option<ContextManager>,
    ) -> tokio::task::JoinHandle<()> {
        let mut events = handle.subscribe();
        let cmd_tx = handle.command_tx.clone();
        let session_state = handle.state.clone();

        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                // --- State engineering: apply event transforms ---
                if let Some(ref mut mgr) = state_mgr {
                    mgr.process_event(&event);
                }

                match event {
                    SessionEvent::TextDelta(t) => {
                        if let Some(ref cb) = self.on_text_delta {
                            cb(t).await;
                        }
                    }
                    SessionEvent::TextComplete(t) => {
                        if let Some(ref cb) = self.on_text_complete {
                            cb(t).await;
                        }
                    }
                    SessionEvent::AudioData(d) => {
                        if let Some(ref cb) = self.on_audio_data {
                            cb(d).await;
                        }
                    }
                    SessionEvent::InputTranscription(t) => {
                        if let Some(ref cb) = self.on_input_transcription {
                            cb(t).await;
                        }
                    }
                    SessionEvent::OutputTranscription(t) => {
                        if let Some(ref cb) = self.on_output_transcription {
                            cb(t).await;
                        }
                    }
                    SessionEvent::TurnComplete => {
                        // Provide turn snapshot to callback
                        let turn = session_state
                            .turns
                            .lock()
                            .last()
                            .cloned()
                            .unwrap_or_default();

                        // --- Context engineering: record turn ---
                        if let Some(ref mut ctx) = context_mgr {
                            ctx.record_turn(&turn, "model");
                        }

                        if let Some(ref cb) = self.on_turn_complete {
                            cb(turn).await;
                        }
                    }
                    SessionEvent::ToolCall(calls) => {
                        // Auto-dispatch: the key convenience
                        if let Some(ref reg) = registry {
                            let reg = reg.clone();
                            let tx = cmd_tx.clone();
                            let calls = calls.clone();
                            // Spawn tool execution so it doesn't block event processing
                            tokio::spawn(async move {
                                let responses = reg.execute_all(&calls).await;
                                let _ = tx
                                    .send(SessionCommand::SendToolResponse(responses))
                                    .await;
                            });
                        }
                    }
                    SessionEvent::Interrupted => {
                        if let Some(ref cb) = self.on_interrupted {
                            cb(()).await;
                        }
                    }
                    SessionEvent::Error(e) => {
                        if let Some(ref cb) = self.on_error {
                            cb(e).await;
                        }
                    }
                    SessionEvent::Connected => {
                        // --- Context engineering: fire OnConnect injections ---
                        if let Some(ref ctx) = context_mgr {
                            let _injections = ctx
                                .resolve_injections(&InjectionTrigger::OnConnect)
                                .await;
                            // Injected context could be sent as client_content here
                            // in a future enhancement
                        }

                        if let Some(ref cb) = self.on_connected {
                            cb(()).await;
                        }
                    }
                    SessionEvent::Disconnected(r) => {
                        // --- State engineering: clear temp state on disconnect ---
                        if let Some(ref mut mgr) = state_mgr {
                            mgr.state_mut().clear_temp();
                        }

                        if let Some(ref cb) = self.on_disconnected {
                            cb(r).await;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// GeminiAgent builder
// ---------------------------------------------------------------------------

/// Builder for constructing a [`GeminiAgent`] with fluent configuration.
///
/// Collects session config, tools, callbacks, pipeline settings, and
/// engineering policies (context, prompt, state), then wires everything
/// together in [`build()`](GeminiAgentBuilder::build).
///
/// # Engineering layers
///
/// - **Prompt**: Use [`prompt()`](Self::prompt) to set a structured [`SystemPrompt`]
///   instead of a raw string. Sections are ordered, conditional, and template-interpolated.
/// - **Context**: Use [`context_policy()`](Self::context_policy) to configure compression,
///   memory strategy, and dynamic context injection.
/// - **State**: Use [`state_policy()`](Self::state_policy) to bind transforms and guards
///   to session events for automatic state management.
pub struct GeminiAgentBuilder {
    // Session config
    endpoint: Option<ApiEndpoint>,
    model: GeminiModel,
    voice: Option<Voice>,
    system_instruction: Option<String>,
    modalities: Option<Vec<Modality>>,
    temperature: Option<f32>,
    input_transcription: bool,
    output_transcription: bool,

    // Transport config
    transport_config: TransportConfig,

    // Tools
    registry: FunctionRegistry,
    tool_config: Option<ToolConfig>,
    auto_tool_dispatch: bool,

    // Pipeline config
    pipeline_config: PipelineConfig,

    // Engineering layers
    system_prompt: Option<SystemPrompt>,
    context_policy: Option<ContextPolicy>,
    state_policy: Option<StatePolicy>,

    // Callbacks
    router: EventRouter,
}

impl GeminiAgentBuilder {
    /// Create a new builder with defaults.
    pub fn new() -> Self {
        Self {
            endpoint: None,
            model: GeminiModel::default(),
            voice: None,
            system_instruction: None,
            modalities: None,
            temperature: None,
            input_transcription: false,
            output_transcription: false,
            transport_config: TransportConfig::default(),
            registry: FunctionRegistry::new(),
            tool_config: None,
            auto_tool_dispatch: true,
            pipeline_config: PipelineConfig::default(),
            system_prompt: None,
            context_policy: None,
            state_policy: None,
            router: EventRouter::default(),
        }
    }

    // --- Session configuration ---

    /// Set the API key for Google AI (the simplest auth method).
    ///
    /// Mutually exclusive with [`vertex()`](Self::vertex) — whichever is
    /// called last wins.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.endpoint = Some(ApiEndpoint::google_ai(key));
        self
    }

    /// Configure Vertex AI endpoint with project, location, and OAuth2 token.
    ///
    /// ```rust,no_run
    /// # use gemini_live_rs::app::GeminiAgentBuilder;
    /// let agent = GeminiAgentBuilder::new()
    ///     .vertex("my-project-123", "us-central1", "ya29.ACCESS_TOKEN")
    ///     .build();
    /// ```
    pub fn vertex(
        mut self,
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        self.endpoint = Some(ApiEndpoint::vertex(project, location, access_token));
        self
    }

    /// Set an explicit [`ApiEndpoint`] for full control (custom hosts, etc.).
    pub fn endpoint(mut self, endpoint: ApiEndpoint) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    /// Set the Gemini model.
    pub fn model(mut self, model: GeminiModel) -> Self {
        self.model = model;
        self
    }

    /// Set the output voice.
    pub fn voice(mut self, voice: Voice) -> Self {
        self.voice = Some(voice);
        self
    }

    /// Set the system instruction.
    pub fn system_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.system_instruction = Some(instruction.into());
        self
    }

    /// Set text-only mode (no audio output).
    pub fn text_only(mut self) -> Self {
        self.modalities = Some(vec![Modality::Text]);
        self.voice = None;
        self
    }

    /// Set the generation temperature.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// Enable input audio transcription.
    pub fn input_transcription(mut self) -> Self {
        self.input_transcription = true;
        self
    }

    /// Enable output audio transcription.
    pub fn output_transcription(mut self) -> Self {
        self.output_transcription = true;
        self
    }

    // --- Tool registration ---

    /// Register a tool with an async handler. Tools are auto-dispatched by default.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use gemini_live_rs::app::GeminiAgentBuilder;
    /// # let builder = GeminiAgentBuilder::new();
    /// builder.tool(
    ///     "get_weather",
    ///     "Get weather for a city",
    ///     Some(serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}})),
    ///     |args| async move {
    ///         Ok(serde_json::json!({"temp": 22}))
    ///     },
    /// )
    /// # ;
    /// ```
    pub fn tool<F, Fut>(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Option<serde_json::Value>,
        handler: F,
    ) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, String>> + Send + 'static,
    {
        self.registry.register(name, description, parameters, handler);
        self
    }

    /// Set the tool calling config (auto, any, none).
    pub fn tool_config(mut self, config: ToolConfig) -> Self {
        self.tool_config = Some(config);
        self
    }

    /// Disable automatic tool dispatch (user handles `ToolCall` events manually).
    pub fn manual_tool_dispatch(mut self) -> Self {
        self.auto_tool_dispatch = false;
        self
    }

    // --- Pipeline configuration ---

    /// Set the VAD configuration.
    #[cfg(feature = "vad")]
    pub fn vad_config(mut self, config: VadConfig) -> Self {
        self.pipeline_config.vad_config = Some(config);
        self
    }

    /// Disable client-side VAD.
    #[cfg(feature = "vad")]
    pub fn disable_vad(mut self) -> Self {
        self.pipeline_config.vad_config = None;
        self
    }

    /// Set barge-in detection configuration.
    pub fn barge_in_config(mut self, config: BargeInConfig) -> Self {
        self.pipeline_config.barge_in_config = config;
        self
    }

    /// Set turn detection configuration.
    pub fn turn_detection_config(mut self, config: TurnDetectionConfig) -> Self {
        self.pipeline_config.turn_detection_config = config;
        self
    }

    /// Set jitter buffer configuration.
    pub fn jitter_config(mut self, config: JitterConfig) -> Self {
        self.pipeline_config.jitter_config = config;
        self
    }

    // --- Transport configuration ---

    /// Set the transport configuration.
    pub fn transport(mut self, config: TransportConfig) -> Self {
        self.transport_config = config;
        self
    }

    // --- Engineering layers ---

    /// Set a structured system prompt (replaces `system_instruction`).
    ///
    /// When a [`SystemPrompt`] is set, it takes precedence over any raw
    /// string set via [`system_instruction()`](Self::system_instruction).
    /// The prompt is rendered at build time and sent in the setup message.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use gemini_live_rs::app::GeminiAgentBuilder;
    /// # use gemini_live_rs::prompt::*;
    /// # let builder = GeminiAgentBuilder::new();
    /// builder.prompt(
    ///     SystemPrompt::builder()
    ///         .role("You are a customer service agent.")
    ///         .task("Help with billing inquiries.")
    ///         .constraint("Never share internal pricing.")
    ///         .format("Short, conversational sentences.")
    ///         .build()
    /// )
    /// # ;
    /// ```
    pub fn prompt(mut self, prompt: SystemPrompt) -> Self {
        self.system_prompt = Some(prompt);
        self
    }

    /// Set the context engineering policy.
    ///
    /// Configures server-side compression, client-side memory management,
    /// and dynamic context injection rules.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use gemini_live_rs::app::GeminiAgentBuilder;
    /// # use gemini_live_rs::context::*;
    /// # let builder = GeminiAgentBuilder::new();
    /// builder.context_policy(
    ///     ContextPolicy::builder()
    ///         .compression_threshold(8000)
    ///         .memory(MemoryStrategy::window(20))
    ///         .inject_on_connect("tier", "premium")
    ///         .build()
    /// )
    /// # ;
    /// ```
    pub fn context_policy(mut self, policy: ContextPolicy) -> Self {
        self.context_policy = Some(policy);
        self
    }

    /// Set the state engineering policy.
    ///
    /// Binds state transforms to session events and defines guards
    /// for automatic state management during the conversation.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use gemini_live_rs::app::GeminiAgentBuilder;
    /// # use gemini_live_rs::state::*;
    /// # let builder = GeminiAgentBuilder::new();
    /// builder.state_policy(
    ///     StatePolicy::builder()
    ///         .on_turn_complete(StateTransform::increment("turn_count", 1))
    ///         .on_interrupted(StateTransform::increment("interruptions", 1))
    ///         .initial("turn_count", serde_json::json!(0))
    ///         .build()
    /// )
    /// # ;
    /// ```
    pub fn state_policy(mut self, policy: StatePolicy) -> Self {
        self.state_policy = Some(policy);
        self
    }

    // --- Callbacks ---

    /// Called for each incremental text chunk from the model.
    pub fn on_text<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_text_delta = Some(Arc::new(move |t| Box::pin(f(t))));
        self
    }

    /// Called when a complete text response is available.
    pub fn on_text_complete<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_text_complete = Some(Arc::new(move |t| Box::pin(f(t))));
        self
    }

    /// Called for each audio data chunk from the model (PCM16 bytes).
    pub fn on_audio<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_audio_data = Some(Arc::new(move |d| Box::pin(f(d))));
        self
    }

    /// Called when input audio transcription is available.
    pub fn on_input_transcription<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_input_transcription = Some(Arc::new(move |t| Box::pin(f(t))));
        self
    }

    /// Called when output audio transcription is available.
    pub fn on_output_transcription<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_output_transcription = Some(Arc::new(move |t| Box::pin(f(t))));
        self
    }

    /// Called when a model turn completes (with the [`Turn`] snapshot).
    pub fn on_turn_complete<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Turn) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_turn_complete = Some(Arc::new(move |t| Box::pin(f(t))));
        self
    }

    /// Called when the model is interrupted by barge-in.
    pub fn on_interrupted<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(()) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_interrupted = Some(Arc::new(move |_| Box::pin(f(()))));
        self
    }

    /// Called on non-fatal errors.
    pub fn on_error<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_error = Some(Arc::new(move |e| Box::pin(f(e))));
        self
    }

    /// Called when the session connects successfully.
    pub fn on_connected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(()) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_connected = Some(Arc::new(move |_| Box::pin(f(()))));
        self
    }

    /// Called when the session disconnects (with optional reason).
    pub fn on_disconnected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.router.on_disconnected = Some(Arc::new(move |r| Box::pin(f(r))));
        self
    }

    // --- Build ---

    /// Build and connect the agent.
    ///
    /// This:
    /// 1. Assembles [`SessionConfig`] from accumulated state
    ///    - If a [`SystemPrompt`] is set, it renders and overrides `system_instruction`
    ///    - If a [`ContextPolicy`] has compression, it sets `context_window_compression`
    ///    - If a [`ContextPolicy`] enables resumption, it sets `session_resumption`
    /// 2. Connects to the Gemini Live API
    /// 3. Waits for the session to become active
    /// 4. Spawns the event router with auto tool dispatch, state management,
    ///    and context tracking
    ///
    /// Returns a fully-connected [`GeminiAgent`].
    pub async fn build(self) -> Result<GeminiAgent, SessionError> {
        let endpoint = self.endpoint.ok_or(SessionError::SetupFailed(
            "API endpoint is required — call .api_key() or .vertex()".into(),
        ))?;

        // Assemble SessionConfig
        let mut session_config = SessionConfig::from_endpoint(endpoint);
        session_config = session_config.model(self.model);

        if let Some(voice) = self.voice {
            session_config = session_config.voice(voice);
        }

        // --- Prompt engineering: structured prompt takes precedence ---
        if let Some(ref prompt) = self.system_prompt {
            session_config.system_instruction = Some(prompt.to_content());
        } else if let Some(instruction) = self.system_instruction {
            session_config = session_config.system_instruction(instruction);
        }

        if let Some(modalities) = self.modalities {
            session_config = session_config.response_modalities(modalities);
        }
        if let Some(temp) = self.temperature {
            session_config = session_config.temperature(temp);
        }
        if self.input_transcription {
            session_config = session_config.enable_input_transcription();
        }
        if self.output_transcription {
            session_config = session_config.enable_output_transcription();
        }

        // --- Context engineering: apply policy to session config ---
        if let Some(ref ctx_policy) = self.context_policy {
            if let Some(threshold) = ctx_policy.compression_threshold {
                session_config = session_config.context_window_compression(threshold);
            }
            if ctx_policy.enable_resumption {
                session_config = session_config.session_resumption(None);
            }
        }

        // Add tool declarations from registry
        if !self.registry.is_empty() {
            session_config = session_config.add_tool(self.registry.to_tool_declaration());
            session_config = session_config
                .tool_config(self.tool_config.unwrap_or_else(ToolConfig::auto));
        }

        // Connect
        let handle = connect(session_config, self.transport_config).await?;
        handle.wait_for_phase(SessionPhase::Active).await;

        // --- State engineering: create state manager ---
        let state_mgr = self.state_policy.map(StateManager::new);

        // --- Context engineering: create context manager ---
        let context_mgr = self.context_policy.map(|policy| {
            ContextManager::new(policy, handle.session_id().to_string())
        });

        // Spawn event router with optional auto-dispatch, state, and context
        let registry = if self.auto_tool_dispatch && !self.registry.is_empty() {
            Some(Arc::new(self.registry))
        } else {
            None
        };
        let router_handle = self
            .router
            .spawn(handle.clone(), registry, state_mgr, context_mgr);

        Ok(GeminiAgent {
            handle,
            router_handle,
            pipeline_config: self.pipeline_config,
        })
    }
}

impl Default for GeminiAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// GeminiAgent
// ---------------------------------------------------------------------------

/// A fully-configured, connected Gemini agent.
///
/// Created via [`GeminiAgent::builder()`]. Wraps a [`SessionHandle`] with
/// automatic event routing and tool dispatch.
pub struct GeminiAgent {
    handle: SessionHandle,
    router_handle: tokio::task::JoinHandle<()>,
    /// Pipeline configuration (available for [`CallSession`](crate::call::CallSession) construction).
    pub pipeline_config: PipelineConfig,
}

impl std::fmt::Debug for GeminiAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiAgent")
            .field("session_id", &self.handle.session_id())
            .field("phase", &self.handle.phase())
            .finish()
    }
}

impl GeminiAgent {
    /// Create a new builder.
    pub fn builder() -> GeminiAgentBuilder {
        GeminiAgentBuilder::new()
    }

    /// Access the underlying [`SessionHandle`] for advanced use.
    pub fn session(&self) -> &SessionHandle {
        &self.handle
    }

    /// Current session phase.
    pub fn phase(&self) -> SessionPhase {
        self.handle.phase()
    }

    /// Session ID.
    pub fn session_id(&self) -> &str {
        self.handle.session_id()
    }

    /// Send a text message.
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), SessionError> {
        self.handle.send_text(text).await
    }

    /// Send audio data (raw PCM16 bytes).
    pub async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError> {
        self.handle.send_audio(data).await
    }

    /// Subscribe to raw session events (escape hatch for advanced use).
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.handle.subscribe()
    }

    /// Graceful shutdown — disconnects the session and waits for the router to finish.
    pub async fn shutdown(self) -> Result<(), SessionError> {
        self.handle.disconnect().await?;
        let _ = self.router_handle.await;
        Ok(())
    }

    /// Wait until the agent disconnects (e.g., remote hangup or error).
    pub async fn wait_until_done(self) {
        let _ = self.router_handle.await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults() {
        let builder = GeminiAgentBuilder::new();
        assert!(builder.endpoint.is_none());
        assert_eq!(builder.model, GeminiModel::default());
        assert!(builder.voice.is_none());
        assert!(builder.auto_tool_dispatch);
        assert!(builder.registry.is_empty());
    }

    #[test]
    fn builder_fluent_config() {
        let builder = GeminiAgent::builder()
            .api_key("test-key")
            .model(GeminiModel::Gemini2_5FlashNativeAudio)
            .voice(Voice::Kore)
            .system_instruction("Be helpful.")
            .temperature(0.7)
            .input_transcription()
            .output_transcription();

        assert!(builder.endpoint.is_some());
        assert_eq!(builder.model, GeminiModel::Gemini2_5FlashNativeAudio);
        assert_eq!(builder.voice, Some(Voice::Kore));
        assert_eq!(builder.system_instruction, Some("Be helpful.".to_string()));
        assert_eq!(builder.temperature, Some(0.7));
        assert!(builder.input_transcription);
        assert!(builder.output_transcription);
    }

    #[test]
    fn builder_text_only() {
        let builder = GeminiAgent::builder()
            .voice(Voice::Kore)
            .text_only();

        assert!(builder.voice.is_none());
        assert_eq!(builder.modalities, Some(vec![Modality::Text]));
    }

    #[test]
    fn builder_tool_registration() {
        let builder = GeminiAgent::builder()
            .tool("test_fn", "A test", None, |_| async { Ok(serde_json::json!({})) })
            .tool("test_fn2", "Another test", None, |_| async { Ok(serde_json::json!({})) });

        assert_eq!(builder.registry.len(), 2);
        assert!(builder.registry.has("test_fn"));
        assert!(builder.registry.has("test_fn2"));
    }

    #[test]
    fn builder_manual_tool_dispatch() {
        let builder = GeminiAgent::builder().manual_tool_dispatch();
        assert!(!builder.auto_tool_dispatch);
    }

    #[tokio::test]
    async fn build_fails_without_endpoint() {
        let result = GeminiAgent::builder().build().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("API endpoint is required"));
    }

    #[test]
    fn pipeline_config_defaults() {
        let config = PipelineConfig::default();
        assert_eq!(config.input_sample_rate, 16_000);
        assert_eq!(config.output_sample_rate, 24_000);
        assert!(config.barge_in_config.enabled);
        assert!(config.turn_detection_config.enabled);
    }

    #[test]
    fn builder_with_system_prompt() {
        use crate::prompt::SystemPrompt;

        let builder = GeminiAgent::builder()
            .api_key("key")
            .prompt(
                SystemPrompt::builder()
                    .role("You are an agent.")
                    .task("Help users.")
                    .constraint("Be nice.")
                    .build(),
            );

        assert!(builder.system_prompt.is_some());
        // system_instruction should be None (prompt takes precedence)
        assert!(builder.system_instruction.is_none());
    }

    #[test]
    fn builder_with_context_policy() {
        use crate::context::{ContextPolicy, MemoryStrategy};

        let builder = GeminiAgent::builder()
            .api_key("key")
            .context_policy(
                ContextPolicy::builder()
                    .compression_threshold(8000)
                    .memory(MemoryStrategy::window(20))
                    .inject_on_connect("tier", "gold")
                    .enable_resumption()
                    .build(),
            );

        let policy = builder.context_policy.as_ref().unwrap();
        assert_eq!(policy.compression_threshold, Some(8000));
        assert!(policy.enable_resumption);
        assert_eq!(policy.injections.len(), 1);
    }

    #[test]
    fn builder_with_state_policy() {
        use crate::state::{StatePolicy, StateTransform};

        let builder = GeminiAgent::builder()
            .api_key("key")
            .state_policy(
                StatePolicy::builder()
                    .on_turn_complete(StateTransform::increment("turns", 1))
                    .initial("turns", serde_json::json!(0))
                    .build(),
            );

        let policy = builder.state_policy.as_ref().unwrap();
        assert_eq!(policy.event_transforms.len(), 1);
        assert_eq!(policy.initial_state.len(), 1);
    }

    #[test]
    fn builder_vertex_config() {
        let builder = GeminiAgent::builder()
            .vertex("my-project", "us-central1", "ya29.token")
            .model(GeminiModel::Gemini2_5FlashNativeAudio)
            .voice(Voice::Kore);

        assert!(matches!(builder.endpoint, Some(ApiEndpoint::VertexAI(_))));
    }

    #[test]
    fn builder_endpoint_last_wins() {
        // vertex() overrides previous api_key()
        let builder = GeminiAgent::builder()
            .api_key("key")
            .vertex("proj", "us-central1", "token");
        assert!(matches!(builder.endpoint, Some(ApiEndpoint::VertexAI(_))));

        // api_key() overrides previous vertex()
        let builder2 = GeminiAgent::builder()
            .vertex("proj", "us-central1", "token")
            .api_key("key");
        assert!(matches!(builder2.endpoint, Some(ApiEndpoint::GoogleAI { .. })));
    }

    #[test]
    fn builder_all_engineering_layers() {
        use crate::context::{ContextPolicy, MemoryStrategy};
        use crate::prompt::PromptStrategy;
        use crate::state::{StatePolicy, StateTransform};

        let builder = GeminiAgent::builder()
            .api_key("key")
            .voice(Voice::Kore)
            // Prompt engineering
            .prompt(
                PromptStrategy::customer_service("TechCorp", "Handle billing.")
                    .example("What's my balance?", "Let me check that for you.")
                    .build(),
            )
            // Context engineering
            .context_policy(
                ContextPolicy::builder()
                    .compression_threshold(8000)
                    .memory(MemoryStrategy::window(20))
                    .enable_resumption()
                    .build(),
            )
            // State engineering
            .state_policy(
                StatePolicy::builder()
                    .on_turn_complete(StateTransform::increment("turn_count", 1))
                    .on_interrupted(StateTransform::increment("interruptions", 1))
                    .initial("turn_count", serde_json::json!(0))
                    .initial("interruptions", serde_json::json!(0))
                    .build(),
            )
            // Tools
            .tool("get_balance", "Get account balance", None, |_| async {
                Ok(serde_json::json!({"balance": 100.0}))
            });

        assert!(builder.system_prompt.is_some());
        assert!(builder.context_policy.is_some());
        assert!(builder.state_policy.is_some());
        assert_eq!(builder.registry.len(), 1);
    }
}
