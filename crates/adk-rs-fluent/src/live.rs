//! `Live` — Fluent builder for callback-driven Gemini Live sessions.
//!
//! Wraps L1's `LiveSessionBuilder` with ergonomic callback registration
//! and integration with composition modules (M, T, P).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use rs_adk::live::extractor::{LlmExtractor, TurnExtractor};
use rs_adk::live::{
    ComputedRegistry, ComputedVar, EventCallbacks, LiveHandle, LiveSessionBuilder, Phase,
    PhaseMachine, RateDetector, SustainedDetector, TemporalPattern, TemporalRegistry,
    TurnCountDetector, Watcher, WatcherRegistry,
};
use rs_adk::llm::BaseLlm;
use rs_adk::tool::ToolDispatcher;
use rs_adk::State;
use rs_genai::prelude::*;
use rs_genai::session::SessionWriter;

use crate::live_builders::{PhaseBuilder, WatchBuilder};

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
    pub fn on_tool_call<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<FunctionCall>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<Vec<FunctionResponse>>> + Send + 'static,
    {
        self.callbacks.on_tool_call = Some(Arc::new(move |calls| Box::pin(f(calls))));
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
    pub fn on_connected<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_connected = Some(Arc::new(move || Box::pin(f())));
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
        if let Some(dispatcher) = self.dispatcher {
            builder = builder.dispatcher(dispatcher);
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
            .on_connected(|| async {})
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
}
