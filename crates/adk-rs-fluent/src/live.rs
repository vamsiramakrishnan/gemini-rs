//! `Live` — Fluent builder for callback-driven Gemini Live sessions.
//!
//! Wraps L1's `LiveSessionBuilder` with ergonomic callback registration
//! and integration with composition modules (M, T, P).
//!
//! # Callback Modes
//!
//! Control-lane callbacks support two execution modes via [`CallbackMode`]:
//!
//! - **Default methods** (e.g., `.on_turn_complete()`) → [`CallbackMode::Blocking`]
//! - **`_concurrent` methods** (e.g., `.on_turn_complete_concurrent()`) → [`CallbackMode::Concurrent`]
//!
//! Use concurrent mode for fire-and-forget work (logging, analytics, webhook dispatch).
//!
//! # Background Tool Execution
//!
//! Mark tools for background execution to eliminate dead air in voice sessions:
//!
//! ```rust,ignore
//! Live::builder()
//!     .tools(dispatcher)
//!     .tool_background("search_kb")
//!     .connect_vertex(project, location, token)
//!     .await?;
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use rs_adk::live::extractor::{LlmExtractor, TurnExtractor};
use rs_adk::live::{
    CallbackMode, ComputedRegistry, ComputedVar, EventCallbacks, InstructionModifier, LiveHandle,
    LiveSessionBuilder, Phase, PhaseMachine, RateDetector, ResultFormatter, SustainedDetector,
    TemporalPattern, TemporalRegistry, ToolExecutionMode, TurnCountDetector, Watcher,
    WatcherRegistry,
};
use rs_adk::llm::BaseLlm;
use rs_adk::tool::ToolDispatcher;
use rs_adk::State;
use rs_genai::prelude::*;
use rs_genai::session::SessionWriter;

use crate::live_builders::{PhaseBuilder, PhaseDefaults, WatchBuilder};

/// Fluent builder for Gemini Live sessions.
///
/// # Example
/// ```ignore
/// let session = Live::builder()
///     .model(GeminiModel::Gemini2_0FlashLive)
///     .voice(Voice::Kore)
///     .instruction("You are a weather assistant")
///     .tools(dispatcher)
///     .on_audio(|data| playback_tx.send(data.clone()).ok())
///     .on_text(|t| print!("{t}"))
///     .on_interrupted(|| async { playback.flush().await; })
///     .connect_vertex("project", "us-central1", token)
///     .await?;
/// ```
///
/// # Extraction Pipeline
/// ```ignore
/// let handle = Live::builder()
///     .model(GeminiModel::Gemini2_0FlashLive)
///     .instruction("You are a restaurant order assistant")
///     .extract_turns::<OrderState>(
///         flash_llm,
///         "Extract: items ordered, quantities, modifications, order_phase",
///     )
///     .on_extracted(|name, value| async move {
///         println!("Extracted {name}: {value}");
///     })
///     .connect_vertex(project, location, token)
///     .await?;
///
/// // Read latest extraction from shared State at any time:
/// let order: Option<OrderState> = handle.extracted("OrderState");
/// ```
/// A deferred agent tool registration (resolved at connect time when State is available).
struct DeferredAgentTool {
    name: String,
    description: String,
    agent: Arc<dyn rs_adk::text::TextAgent>,
}

/// Fluent builder for constructing and connecting Gemini Live sessions.
///
/// Accumulates model configuration, callbacks, extractors, phases, watchers,
/// temporal patterns, and tool execution modes, then connects via one of
/// the `connect_*` methods.
///
/// Control-lane callbacks can be registered with `_concurrent` suffixed
/// methods for fire-and-forget execution. Tools can be marked for background
/// execution via [`tool_background()`](Self::tool_background).
pub struct Live {
    config: SessionConfig,
    callbacks: EventCallbacks,
    dispatcher: Option<ToolDispatcher>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    // L1 registries
    computed: ComputedRegistry,
    phases: Vec<Phase>,
    initial_phase: Option<String>,
    watchers: WatcherRegistry,
    temporal: TemporalRegistry,
    greeting: Option<String>,
    // Phase defaults: modifiers + prompt_on_enter inherited by all phases.
    pub(crate) phase_default_modifiers: Vec<InstructionModifier>,
    pub(crate) phase_default_prompt_on_enter: bool,
    // Per-tool execution modes (standard vs background).
    tool_execution_modes: HashMap<String, ToolExecutionMode>,
    // Deferred agent tools (resolved at connect time).
    deferred_agent_tools: Vec<DeferredAgentTool>,
    // LLMs to warm up at connect time.
    warm_up_llms: Vec<Arc<dyn BaseLlm>>,
}

impl Live {
    /// Start building a Live session.
    pub fn builder() -> Self {
        Self {
            config: SessionConfig::from_endpoint(ApiEndpoint::google_ai("")),
            callbacks: EventCallbacks::default(),
            dispatcher: None,
            extractors: Vec::new(),
            computed: ComputedRegistry::new(),
            phases: Vec::new(),
            initial_phase: None,
            watchers: WatcherRegistry::new(),
            temporal: TemporalRegistry::new(),
            greeting: None,
            phase_default_modifiers: Vec::new(),
            phase_default_prompt_on_enter: false,
            tool_execution_modes: HashMap::new(),
            deferred_agent_tools: Vec::new(),
            warm_up_llms: Vec::new(),
        }
    }

    // -- Model & Voice --

    /// Set the Gemini model.
    pub fn model(mut self, model: GeminiModel) -> Self {
        self.config = self.config.model(model);
        self
    }

    /// Set the output voice.
    pub fn voice(mut self, voice: Voice) -> Self {
        self.config = self.config.voice(voice);
        self
    }

    /// Set the system instruction.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.config = self.config.system_instruction(instruction);
        self
    }

    /// Set a greeting prompt to trigger the model to initiate the conversation.
    ///
    /// When set, this text is sent immediately after the session connects,
    /// causing the model to respond first (e.g. with a greeting or introduction).
    ///
    /// ```ignore
    /// let handle = Live::builder()
    ///     .model(GeminiModel::Gemini2_0FlashLive)
    ///     .instruction("You are a friendly assistant")
    ///     .greeting("Greet the user warmly and introduce yourself.")
    ///     .connect_vertex(project, location, token)
    ///     .await?;
    /// // Model will speak first without any user input
    /// ```
    pub fn greeting(mut self, prompt: impl Into<String>) -> Self {
        self.greeting = Some(prompt.into());
        self
    }

    /// Set the temperature.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.config = self.config.temperature(temp);
        self
    }

    // -- Tools --

    /// Set the tool dispatcher (auto-dispatches tool calls).
    pub fn tools(mut self, dispatcher: ToolDispatcher) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// Register tools from a `T` module composition.
    ///
    /// ```ignore
    /// use adk_rs_fluent::prelude::*;
    ///
    /// Live::builder()
    ///     .with_tools(
    ///         T::simple("get_weather", "Get weather", |args| async move {
    ///             Ok(serde_json::json!({"temp": 22}))
    ///         })
    ///         | T::google_search()
    ///     )
    /// ```
    pub fn with_tools(mut self, composite: crate::compose::tools::ToolComposite) -> Self {
        let dispatcher = self.dispatcher.get_or_insert_with(ToolDispatcher::new);
        for entry in composite.entries {
            match entry {
                crate::compose::tools::ToolCompositeEntry::Function(f) => {
                    dispatcher.register_function(f);
                }
                crate::compose::tools::ToolCompositeEntry::BuiltIn(tool) => {
                    // Built-in tools go directly to session config
                    self.config = self.config.add_tool(tool);
                }
            }
        }
        self
    }

    /// Register a text agent as a tool the live model can call.
    ///
    /// The agent shares the session's `State`, so it can read live-extracted
    /// values and its mutations are visible to watchers and phase transitions.
    ///
    /// ```ignore
    /// Live::builder()
    ///     .agent_tool("verify_identity", "Verify caller identity", verifier_agent)
    ///     .agent_tool("calc_payment", "Calculate payment plans", calc_pipeline)
    /// ```
    pub fn agent_tool(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        agent: impl rs_adk::text::TextAgent + 'static,
    ) -> Self {
        self.deferred_agent_tools.push(DeferredAgentTool {
            name: name.into(),
            description: description.into(),
            agent: Arc::new(agent),
        });
        self
    }

    /// Register a text agent (already `Arc`'d) as a tool.
    pub fn agent_tool_arc(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        agent: Arc<dyn rs_adk::text::TextAgent>,
    ) -> Self {
        self.deferred_agent_tools.push(DeferredAgentTool {
            name: name.into(),
            description: description.into(),
            agent,
        });
        self
    }

    /// Enable Google Search built-in tool.
    pub fn google_search(mut self) -> Self {
        self.config = self.config.with_google_search();
        self
    }

    /// Enable code execution built-in tool.
    pub fn code_execution(mut self) -> Self {
        self.config = self.config.with_code_execution();
        self
    }

    /// Enable URL context built-in tool.
    pub fn url_context(mut self) -> Self {
        self.config = self.config.with_url_context();
        self
    }

    /// Mark a tool for background execution (zero dead-air).
    ///
    /// When the model calls this tool, an immediate "running" acknowledgment
    /// is sent back while the tool executes in a background task. The final
    /// result is delivered asynchronously when complete.
    pub fn tool_background(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_execution_modes.insert(
            tool_name.into(),
            ToolExecutionMode::Background { formatter: None },
        );
        self
    }

    /// Mark a tool for background execution with a custom result formatter.
    ///
    /// The formatter controls the shape of the acknowledgment ("running"),
    /// completion, and cancellation messages sent to the model.
    pub fn tool_background_with_formatter(
        mut self,
        tool_name: impl Into<String>,
        formatter: Arc<dyn ResultFormatter>,
    ) -> Self {
        self.tool_execution_modes.insert(
            tool_name.into(),
            ToolExecutionMode::Background {
                formatter: Some(formatter),
            },
        );
        self
    }

    // -- Audio/Video Config --

    /// Enable input and/or output transcription.
    pub fn transcription(mut self, input: bool, output: bool) -> Self {
        if input {
            self.config = self.config.enable_input_transcription();
        }
        if output {
            self.config = self.config.enable_output_transcription();
        }
        self
    }

    /// Enable affective dialog (emotionally expressive responses).
    pub fn affective_dialog(mut self, enabled: bool) -> Self {
        self.config = self.config.affective_dialog(enabled);
        self
    }

    /// Enable proactive audio.
    pub fn proactive_audio(mut self, enabled: bool) -> Self {
        self.config = self.config.proactive_audio(enabled);
        self
    }

    /// Set media resolution for video/image input.
    pub fn media_resolution(mut self, res: MediaResolution) -> Self {
        self.config = self.config.media_resolution(res);
        self
    }

    // -- VAD & Activity --

    /// Configure server-side VAD.
    pub fn vad(mut self, detection: AutomaticActivityDetection) -> Self {
        self.config = self.config.server_vad(detection);
        self
    }

    /// Set activity handling mode (interrupts vs no-interruption).
    pub fn activity_handling(mut self, handling: ActivityHandling) -> Self {
        self.config = self.config.activity_handling(handling);
        self
    }

    /// Set turn coverage mode.
    pub fn turn_coverage(mut self, coverage: TurnCoverage) -> Self {
        self.config = self.config.turn_coverage(coverage);
        self
    }

    // -- Session Lifecycle --

    /// Enable session resumption.
    pub fn session_resume(mut self, enabled: bool) -> Self {
        if enabled {
            self.config = self.config.session_resumption(None);
        }
        self
    }

    /// Enable context window compression.
    pub fn context_compression(mut self, trigger_tokens: u32, target_tokens: u32) -> Self {
        self.config = self.config
            .context_window_compression(target_tokens)
            .context_window_trigger_tokens(trigger_tokens);
        self
    }

    // -- Turn Extraction Pipeline --

    /// Add a turn extractor that runs an OOB LLM after each turn to extract
    /// structured data from the transcript window.
    ///
    /// Automatically enables both input and output transcription.
    /// The extraction result is stored in `State` under the type name
    /// (e.g., `"OrderState"`) and can be read via `handle.extracted::<T>(name)`.
    ///
    /// The type `T` must implement `JsonSchema` for schema-guided extraction.
    /// The window size defaults to 3 turns.
    pub fn extract_turns<T>(self, llm: Arc<dyn BaseLlm>, prompt: impl Into<String>) -> Self
    where
        T: DeserializeOwned + Serialize + schemars::JsonSchema + Send + Sync + 'static,
    {
        self.extract_turns_windowed::<T>(llm, prompt, 3)
    }

    /// Like `extract_turns` but with a custom window size.
    pub fn extract_turns_windowed<T>(
        mut self,
        llm: Arc<dyn BaseLlm>,
        prompt: impl Into<String>,
        window_size: usize,
    ) -> Self
    where
        T: DeserializeOwned + Serialize + schemars::JsonSchema + Send + Sync + 'static,
    {
        // Auto-enable transcription
        self.config = self
            .config
            .enable_input_transcription()
            .enable_output_transcription();

        // Derive name from type
        let name = std::any::type_name::<T>()
            .rsplit("::")
            .next()
            .unwrap_or("Extraction")
            .to_string();

        // Generate JSON schema from the type
        let root_schema = schemars::schema_for!(T);
        let schema =
            serde_json::to_value(root_schema).unwrap_or(serde_json::Value::Null);

        // Auto-register LLM for connection warming
        self.warm_up_llms.push(llm.clone());

        let extractor = LlmExtractor::new(name, llm, prompt, window_size).with_schema(schema);
        self.extractors.push(Arc::new(extractor));
        self
    }

    /// Add a custom `TurnExtractor` implementation.
    pub fn extractor(mut self, extractor: Arc<dyn TurnExtractor>) -> Self {
        // Auto-enable transcription
        self.config = self
            .config
            .enable_input_transcription()
            .enable_output_transcription();
        self.extractors.push(extractor);
        self
    }

    /// Called when a TurnExtractor produces a result.
    ///
    /// The callback receives the extractor name and the extracted JSON value.
    pub fn on_extracted<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extracted =
            Some(Arc::new(move |name, value| Box::pin(f(name, value))));
        self
    }

    /// Called when a TurnExtractor fails.
    ///
    /// The callback receives the extractor name and error message.
    /// Use this for custom error handling (alerting, retry logic, etc.).
    pub fn on_extraction_error<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extraction_error =
            Some(Arc::new(move |name, error| Box::pin(f(name, error))));
        self
    }

    // -- Outbound Interceptors --

    /// Intercept tool responses before they are sent back to Gemini.
    ///
    /// Use this to rewrite, augment, or filter tool results based on
    /// conversation state. The callback receives the tool responses and the
    /// shared `State`, and returns (potentially modified) responses.
    ///
    /// # Example
    /// ```ignore
    /// .before_tool_response(|responses, state| async move {
    ///     let order: OrderState = state.get("OrderState").unwrap_or_default();
    ///     responses.into_iter().map(|mut r| {
    ///         r.response["current_order"] = serde_json::to_value(&order).unwrap();
    ///         r
    ///     }).collect()
    /// })
    /// ```
    pub fn before_tool_response<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<FunctionResponse>, rs_adk::State) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Vec<FunctionResponse>> + Send + 'static,
    {
        self.callbacks.before_tool_response =
            Some(Arc::new(move |responses, state| Box::pin(f(responses, state))));
        self
    }

    /// Hook called at turn boundaries — after extractors run, before `on_turn_complete`.
    ///
    /// Receives the shared `State` and a `SessionWriter` for injecting content
    /// into the conversation. Use for context stuffing, K/V data injection,
    /// condensed state summaries, or any outbound content interleaving.
    ///
    /// # Example
    /// ```ignore
    /// .on_turn_boundary(|state, writer| async move {
    ///     let summary = state.get::<String>("summary").unwrap_or_default();
    ///     writer.send_client_content(
    ///         vec![Content::user().text(format!("[Context: {summary}]"))],
    ///         false,
    ///     ).await.ok();
    /// })
    /// ```
    pub fn on_turn_boundary<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(rs_adk::State, Arc<dyn rs_genai::session::SessionWriter>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_turn_boundary =
            Some(Arc::new(move |state, writer| Box::pin(f(state, writer))));
        self
    }

    /// State-reactive system instruction template.
    ///
    /// Called after extractors run on each turn. If it returns `Some(instruction)`,
    /// the system instruction is updated mid-session (deduped — same instruction
    /// is not sent twice). Returns `None` to leave the instruction unchanged.
    ///
    /// # Example
    /// ```ignore
    /// .instruction_template(|state| {
    ///     let phase: String = state.get("phase").unwrap_or_default();
    ///     match phase.as_str() {
    ///         "ordering" => Some("Focus on taking the order accurately.".into()),
    ///         "confirming" => Some("Summarize and confirm the order.".into()),
    ///         _ => None,
    ///     }
    /// })
    /// ```
    pub fn instruction_template(
        mut self,
        f: impl Fn(&rs_adk::State) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.callbacks.instruction_template = Some(Arc::new(f));
        self
    }

    /// State-reactive instruction amendment (additive, not replacement).
    ///
    /// Unlike `instruction_template` (which replaces the entire instruction),
    /// this appends to the current phase instruction. The developer never needs
    /// to know or repeat the base instruction.
    ///
    /// # Example
    /// ```ignore
    /// .instruction_amendment(|state| {
    ///     let risk: String = state.get("derived:risk").unwrap_or_default();
    ///     if risk == "high" {
    ///         Some("[IMPORTANT: Use empathetic language. Do not threaten.]".into())
    ///     } else {
    ///         None
    ///     }
    /// })
    /// ```
    pub fn instruction_amendment(
        mut self,
        f: impl Fn(&rs_adk::State) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.callbacks.instruction_amendment = Some(Arc::new(f));
        self
    }

    // -- Computed State --

    /// Register a computed (derived) state variable.
    ///
    /// The compute function receives the full `State` and returns `Some(value)`
    /// to write to `derived:{key}`, or `None` to skip.
    pub fn computed(
        mut self,
        key: impl Into<String>,
        deps: &[&str],
        f: impl Fn(&State) -> Option<Value> + Send + Sync + 'static,
    ) -> Self {
        self.computed.register(ComputedVar {
            key: key.into(),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            compute: Arc::new(f),
        });
        self
    }

    // -- Phase Machine --

    /// Set default modifiers and `prompt_on_enter` inherited by all phases.
    ///
    /// Phase-specific modifiers are applied *after* defaults, so they extend (not replace).
    ///
    /// ```ignore
    /// Live::builder()
    ///     .phase_defaults(|p| {
    ///         p.with_state(&["emotional_state", "risk_level"])
    ///          .when(risk_is_elevated, "Show extra empathy.")
    ///          .prompt_on_enter(true)
    ///     })
    ///     .phase("greet").instruction("...").done()
    ///     .phase("close").instruction("...").done()
    ///     // Both phases inherit the modifiers and prompt_on_enter.
    /// ```
    pub fn phase_defaults(mut self, f: impl FnOnce(PhaseDefaults) -> PhaseDefaults) -> Self {
        let defaults = f(PhaseDefaults::new());
        self.phase_default_modifiers = defaults.modifiers;
        self.phase_default_prompt_on_enter = defaults.prompt_on_enter;
        self
    }

    /// Start building a conversation phase.
    ///
    /// Returns a [`PhaseBuilder`] that flows back to this `Live` via `.done()`.
    pub fn phase(self, name: impl Into<String>) -> PhaseBuilder {
        PhaseBuilder::new(self, name)
    }

    /// Set the initial phase name (must match a registered phase).
    pub fn initial_phase(mut self, name: impl Into<String>) -> Self {
        self.initial_phase = Some(name.into());
        self
    }

    /// Internal method called by [`PhaseBuilder::done`].
    pub(crate) fn add_phase(&mut self, phase: Phase) {
        self.phases.push(phase);
    }

    // -- Watchers --

    /// Start building a state watcher.
    ///
    /// Returns a [`WatchBuilder`] that flows back to this `Live` via `.then()`.
    pub fn watch(self, key: impl Into<String>) -> WatchBuilder {
        WatchBuilder::new(self, key)
    }

    /// Internal method called by [`WatchBuilder::then`].
    pub(crate) fn add_watcher(&mut self, watcher: Watcher) {
        self.watchers.add(watcher);
    }

    // -- Temporal Patterns --

    /// Register a sustained condition pattern.
    ///
    /// Fires when the condition remains true for at least `duration`.
    pub fn when_sustained<F, Fut>(
        mut self,
        name: impl Into<String>,
        condition: impl Fn(&State) -> bool + Send + Sync + 'static,
        duration: Duration,
        action: F,
    ) -> Self
    where
        F: Fn(State, Arc<dyn SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let detector = SustainedDetector::new(Arc::new(condition), duration);
        self.temporal.add(TemporalPattern::new(
            name,
            Box::new(detector),
            Arc::new(move |s, w| Box::pin(action(s, w))),
            None,
        ));
        self
    }

    /// Register a rate detection pattern.
    ///
    /// Fires when at least `count` matching events occur within `window`.
    pub fn when_rate<F, Fut>(
        mut self,
        name: impl Into<String>,
        filter: impl Fn(&SessionEvent) -> bool + Send + Sync + 'static,
        count: u32,
        window: Duration,
        action: F,
    ) -> Self
    where
        F: Fn(State, Arc<dyn SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let detector = RateDetector::new(Arc::new(filter), count, window);
        self.temporal.add(TemporalPattern::new(
            name,
            Box::new(detector),
            Arc::new(move |s, w| Box::pin(action(s, w))),
            None,
        ));
        self
    }

    /// Register a turn count pattern.
    ///
    /// Fires when the condition is true for `turn_count` consecutive turns.
    pub fn when_turns<F, Fut>(
        mut self,
        name: impl Into<String>,
        condition: impl Fn(&State) -> bool + Send + Sync + 'static,
        turn_count: u32,
        action: F,
    ) -> Self
    where
        F: Fn(State, Arc<dyn SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let detector = TurnCountDetector::new(Arc::new(condition), turn_count);
        self.temporal.add(TemporalPattern::new(
            name,
            Box::new(detector),
            Arc::new(move |s, w| Box::pin(action(s, w))),
            None,
        ));
        self
    }

    // -- Fast Lane Callbacks (sync, < 1ms) --

    /// Called for each audio chunk from the model (PCM16 24kHz).
    pub fn on_audio(mut self, f: impl Fn(&Bytes) + Send + Sync + 'static) -> Self {
        self.callbacks.on_audio = Some(Box::new(f));
        self
    }

    /// Called for each incremental text delta.
    pub fn on_text(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.callbacks.on_text = Some(Box::new(f));
        self
    }

    /// Called when model completes a text response.
    pub fn on_text_complete(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.callbacks.on_text_complete = Some(Box::new(f));
        self
    }

    /// Called for input (user speech) transcription.
    pub fn on_input_transcript(mut self, f: impl Fn(&str, bool) + Send + Sync + 'static) -> Self {
        self.callbacks.on_input_transcript = Some(Box::new(f));
        self
    }

    /// Called for output (model speech) transcription.
    pub fn on_output_transcript(mut self, f: impl Fn(&str, bool) + Send + Sync + 'static) -> Self {
        self.callbacks.on_output_transcript = Some(Box::new(f));
        self
    }

    /// Called when server VAD detects voice activity start.
    pub fn on_vad_start(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.callbacks.on_vad_start = Some(Box::new(f));
        self
    }

    /// Called when server VAD detects voice activity end.
    pub fn on_vad_end(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.callbacks.on_vad_end = Some(Box::new(f));
        self
    }

    // -- Control Lane Callbacks (async, can block) --

    /// Called when model is interrupted by barge-in.
    pub fn on_interrupted<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_interrupted = Some(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called when model requests tool execution.
    /// Return `None` to auto-dispatch, `Some(responses)` to override.
    /// Receives State for natural state promotion from tool results.
    pub fn on_tool_call<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<FunctionCall>, State) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<Vec<FunctionResponse>>> + Send + 'static,
    {
        self.callbacks.on_tool_call = Some(Arc::new(move |calls, state| Box::pin(f(calls, state))));
        self
    }

    /// Called when model turn completes.
    pub fn on_turn_complete<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_turn_complete = Some(Arc::new(move || Box::pin(f())));
        self
    }

    /// Called when server sends GoAway.
    pub fn on_go_away<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_go_away = Some(Arc::new(move |d| Box::pin(f(d))));
        self
    }

    /// Called when session connects (setup complete).
    ///
    /// Receives a `SessionWriter` for sending messages on connect.
    pub fn on_connected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Arc<dyn rs_genai::session::SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_connected = Some(Arc::new(move |w| Box::pin(f(w))));
        self
    }

    /// Called when session disconnects.
    pub fn on_disconnected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_disconnected = Some(Arc::new(move |r| Box::pin(f(r))));
        self
    }

    /// Called on non-fatal errors.
    pub fn on_error<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_error = Some(Arc::new(move |e| Box::pin(f(e))));
        self
    }

    // -- Concurrent callback variants --
    // These set CallbackMode::Concurrent so the callback is spawned as a
    // detached tokio task instead of being awaited inline.

    /// Called when model turn completes (spawned concurrently).
    pub fn on_turn_complete_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_turn_complete = Some(Arc::new(move || Box::pin(f())));
        self.callbacks.on_turn_complete_mode = CallbackMode::Concurrent;
        self
    }

    /// Called when session connects (spawned concurrently).
    pub fn on_connected_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Arc<dyn rs_genai::session::SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_connected = Some(Arc::new(move |w| Box::pin(f(w))));
        self.callbacks.on_connected_mode = CallbackMode::Concurrent;
        self
    }

    /// Called when session disconnects (spawned concurrently).
    pub fn on_disconnected_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_disconnected = Some(Arc::new(move |r| Box::pin(f(r))));
        self.callbacks.on_disconnected_mode = CallbackMode::Concurrent;
        self
    }

    /// Called on non-fatal errors (spawned concurrently).
    pub fn on_error_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_error = Some(Arc::new(move |e| Box::pin(f(e))));
        self.callbacks.on_error_mode = CallbackMode::Concurrent;
        self
    }

    /// Called when server sends GoAway (spawned concurrently).
    pub fn on_go_away_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_go_away = Some(Arc::new(move |d| Box::pin(f(d))));
        self.callbacks.on_go_away_mode = CallbackMode::Concurrent;
        self
    }

    /// Called when a TurnExtractor produces a result (spawned concurrently).
    pub fn on_extracted_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extracted = Some(Arc::new(move |name, value| Box::pin(f(name, value))));
        self.callbacks.on_extracted_mode = CallbackMode::Concurrent;
        self
    }

    /// Called when a TurnExtractor fails (spawned concurrently).
    pub fn on_extraction_error_concurrent<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extraction_error =
            Some(Arc::new(move |name, error| Box::pin(f(name, error))));
        self.callbacks.on_extraction_error_mode = CallbackMode::Concurrent;
        self
    }

    // -- Connect --

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
            let d = dispatcher.get_or_insert_with(ToolDispatcher::new);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_chain_compiles() {
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .voice(Voice::Kore)
            .instruction("Test")
            .temperature(0.7)
            .google_search()
            .transcription(true, true)
            .affective_dialog(true)
            .session_resume(true)
            .context_compression(4000, 2000)
            .on_audio(|_data| {})
            .on_text(|_t| {})
            .on_vad_start(|| {})
            .on_interrupted(|| async {})
            .on_turn_complete(|| async {})
            .on_go_away(|_d| async {})
            .on_connected(|_writer| async {})
            .on_disconnected(|_r| async {})
            .on_error(|_e| async {});
        // Just verify the builder chain compiles
    }

    #[test]
    fn builder_with_extraction_compiles() {
        use rs_adk::llm::{LlmError, LlmRequest, LlmResponse};
        use schemars::JsonSchema;

        #[derive(serde::Deserialize, serde::Serialize, JsonSchema)]
        struct OrderState {
            phase: String,
            items: Vec<String>,
        }

        struct FakeLlm;

        #[async_trait::async_trait]
        impl BaseLlm for FakeLlm {
            fn model_id(&self) -> &str {
                "fake"
            }
            async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                unimplemented!()
            }
        }

        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .instruction("Restaurant order assistant")
            .extract_turns::<OrderState>(
                Arc::new(FakeLlm),
                "Extract order state: items, quantities, phase",
            )
            .on_extracted(|name, value| async move {
                let _ = (name, value);
            })
            // Outbound interceptors
            .before_tool_response(|responses, _state| async move {
                responses // pass through
            })
            .on_turn_boundary(|_state, _writer| async move {
                // inject context
            })
            .instruction_template(|state| {
                let phase: String = state.get("phase").unwrap_or_default();
                match phase.as_str() {
                    "ordering" => Some("Take orders accurately.".into()),
                    _ => None,
                }
            });
        // Just verify the builder chain with all features compiles
    }

    #[test]
    fn builder_with_computed_state_compiles() {
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .instruction("Test computed state")
            .computed("doubled", &["app:count"], |state| {
                let count: i64 = state.get("app:count")?;
                Some(serde_json::json!(count * 2))
            })
            .computed("level", &["app:score"], |state| {
                let score: f64 = state.get("app:score")?;
                if score > 0.5 {
                    Some(serde_json::json!("high"))
                } else {
                    Some(serde_json::json!("low"))
                }
            });
    }

    #[test]
    fn builder_with_phases_compiles() {
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .phase("greeting")
                .instruction("Welcome the user warmly")
                .transition("main", |s| s.get::<bool>("greeted").unwrap_or(false))
                .on_enter(|state, _writer| async move {
                    state.set("entered_greeting", true);
                })
                .done()
            .phase("main")
                .dynamic_instruction(|s| {
                    let topic: String = s.get("topic").unwrap_or_default();
                    format!("Discuss {topic}")
                })
                .tools(vec!["search".into(), "lookup".into()])
                .transition("farewell", |s| s.get::<bool>("done").unwrap_or(false))
                .done()
            .phase("farewell")
                .instruction("Say goodbye")
                .terminal()
                .done()
            .initial_phase("greeting");
    }

    #[test]
    fn builder_with_phase_guard_compiles() {
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .phase("start")
                .instruction("Begin")
                .transition("secure", |_| true)
                .done()
            .phase("secure")
                .instruction("Secure area")
                .guard(|s| s.get::<bool>("verified").unwrap_or(false))
                .on_exit(|state, _writer| async move {
                    state.set("left_secure", true);
                })
                .terminal()
                .done()
            .initial_phase("start");
    }

    #[test]
    fn builder_with_watchers_compiles() {
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .watch("app:score")
                .crossed_above(0.9)
                .then(|_old, _new, state| async move {
                    state.set("high_score_alert", true);
                })
            .watch("app:status")
                .changed_to(serde_json::json!("complete"))
                .blocking()
                .then(|_old, _new, _state| async move {
                    // blocking action
                })
            .watch("app:flag")
                .became_true()
                .then(|_old, _new, _state| async move {
                    // flag became true
                });
    }

    #[test]
    fn builder_with_temporal_patterns_compiles() {
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .when_sustained(
                "user_confused",
                |s| s.get::<bool>("confused").unwrap_or(false),
                Duration::from_secs(30),
                |_state, _writer| async move {
                    // offer help
                },
            )
            .when_rate(
                "rapid_errors",
                |evt| matches!(evt, SessionEvent::TextDelta(_)),
                5,
                Duration::from_secs(10),
                |_state, _writer| async move {
                    // throttle
                },
            )
            .when_turns(
                "stuck_in_loop",
                |s| s.get::<bool>("repeating").unwrap_or(false),
                3,
                |_state, _writer| async move {
                    // break loop
                },
            );
    }

    #[test]
    fn builder_full_l1_chain_compiles() {
        // Full chain combining all L1 features in a single builder
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .voice(Voice::Kore)
            .instruction("Full featured agent")
            // Computed state
            .computed("sentiment_level", &["app:sentiment_score"], |state| {
                let score: f64 = state.get("app:sentiment_score")?;
                if score > 0.7 {
                    Some(serde_json::json!("positive"))
                } else if score < 0.3 {
                    Some(serde_json::json!("negative"))
                } else {
                    Some(serde_json::json!("neutral"))
                }
            })
            // Phases
            .phase("greeting")
                .instruction("Greet the user")
                .transition("help", |s| s.get::<bool>("needs_help").unwrap_or(false))
                .done()
            .phase("help")
                .instruction("Help the user")
                .terminal()
                .done()
            .initial_phase("greeting")
            // Watchers
            .watch("app:sentiment_score")
                .crossed_below(0.2)
                .then(|_old, _new, state| async move {
                    state.set("alert:low_sentiment", true);
                })
            // Temporal
            .when_turns(
                "repeated_confusion",
                |s| s.get::<bool>("confused").unwrap_or(false),
                3,
                |_state, _writer| async move {},
            )
            // Standard callbacks
            .on_audio(|_data| {})
            .on_text(|_t| {})
            .on_turn_complete(|| async {});
    }

    #[test]
    fn builder_with_callback_modes_compiles() {
        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .on_turn_complete_concurrent(|| async {})
            .on_error_concurrent(|_e| async {})
            .on_extracted_concurrent(|_name, _val| async {})
            .on_extraction_error_concurrent(|_name, _err| async {})
            .on_connected_concurrent(|_w| async {})
            .on_disconnected_concurrent(|_r| async {})
            .on_go_away_concurrent(|_d| async {});
    }

    #[test]
    fn builder_with_background_tools_compiles() {
        use rs_adk::live::DefaultResultFormatter;

        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .tool_background("search_kb")
            .tool_background_with_formatter(
                "analyze_document",
                Arc::new(DefaultResultFormatter),
            );
    }

    #[test]
    fn builder_mixed_callback_modes_and_bg_tools() {
        use rs_adk::live::DefaultResultFormatter;

        let _live = Live::builder()
            .model(GeminiModel::Gemini2_0FlashLive)
            .voice(Voice::Kore)
            .instruction("Full featured agent")
            .tool_background("slow_tool")
            .tool_background_with_formatter("kb_search", Arc::new(DefaultResultFormatter))
            .on_turn_complete_concurrent(|| async {})
            .on_extracted_concurrent(|_name, _val| async {})
            .on_audio(|_data| {})
            .on_text(|_t| {})
            .on_interrupted(|| async {});
    }
}
