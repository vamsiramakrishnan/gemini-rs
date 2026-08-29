//! Model, session, and tool configuration methods for `Live`.

use std::sync::Arc;
use std::time::Duration;

use gemini_adk_rs::live::needs::RepairConfig;
use gemini_adk_rs::live::persistence::SessionPersistence;
use gemini_adk_rs::live::steering::{ContextDelivery, SteeringMode};
use gemini_adk_rs::live::{Delivery, DeliveryConfig, ResultFormatter, ToolExecutionMode};
use gemini_adk_rs::tool::ToolDispatcher;
use gemini_genai_rs::prelude::*;

use super::{DeferredAgentTool, Live};

impl Live {
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

    /// Switch to text-only mode (no audio output).
    ///
    /// Sets response modality to `Text` and disables speech config.
    /// Use with `GeminiModel::Gemini2_0FlashLive` for text-only conversations.
    pub fn text_only(mut self) -> Self {
        self.config = self.config.text_only();
        self
    }

    /// Add a raw `Tool` declaration to the session configuration.
    ///
    /// Use this for tools that aren't registered through the `ToolDispatcher`
    /// (e.g., raw `FunctionDeclaration` lists, Google Search, code execution).
    pub fn add_tool(mut self, tool: Tool) -> Self {
        self.config = self.config.add_tool(tool);
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

    // -- Wire recording --

    /// Record every wire byte (both directions) to a JSONL log at `path`.
    ///
    /// The log is written by a
    /// [`FileWireRecorder`] created at connect time (a connect error is returned if the file cannot
    /// be created). Replay it offline with `adk session replay <path>` or
    /// [`gemini_adk_rs::live::replay::replay_session`].
    ///
    /// ```ignore
    /// let handle = Live::builder()
    ///     .model(GeminiModel::Gemini2_0FlashLive)
    ///     .record_wire("/tmp/session.wire.jsonl")
    ///     .connect_from_env()
    ///     .await?;
    /// ```
    pub fn record_wire(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.record_wire_path = Some(path.into());
        self
    }

    /// Record every wire byte to a custom
    /// [`WireRecorder`] implementation.
    ///
    /// Overrides (and is overridden by) the most recent of this and
    /// [`record_wire`](Self::record_wire).
    pub fn wire_recorder(
        mut self,
        recorder: Arc<dyn gemini_genai_rs::prelude::WireRecorder>,
    ) -> Self {
        self.record_wire_path = None;
        self.config = self.config.record_wire(recorder);
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
    /// use gemini_adk_fluent_rs::prelude::*;
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
        use crate::compose::tools::ToolResolution;
        for entry in composite.entries {
            match entry.classify() {
                ToolResolution::Runtime(f) => {
                    self.dispatcher
                        .get_or_insert_with(ToolDispatcher::new)
                        .register_function(f);
                }
                ToolResolution::BuiltIn(tool) => {
                    self.config = self.config.add_tool(tool);
                }
                ToolResolution::Agent {
                    name,
                    description,
                    agent,
                } => {
                    // Reuse the deferred agent-tool path so the sub-agent shares
                    // the session State created at connect time.
                    self.deferred_agent_tools.push(DeferredAgentTool {
                        name,
                        description,
                        agent,
                    });
                }
                ToolResolution::Deferred(deferred) => {
                    // MCP / A2A / OpenAPI / Search need an async connection;
                    // resolve them at connect time (see build_and_connect).
                    self.deferred_tools.push(deferred);
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
        agent: impl gemini_adk_rs::text::TextAgent + 'static,
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
        agent: Arc<dyn gemini_adk_rs::text::TextAgent>,
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
            ToolExecutionMode::Background {
                formatter: None,
                scheduling: None,
            },
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
                scheduling: None,
            },
        );
        self
    }

    /// Mark a tool for background execution with a specific scheduling mode.
    ///
    /// The scheduling mode controls how the model handles async results:
    /// - `Interrupt`: halts current output, immediately reports the result
    /// - `WhenIdle`: waits until current output finishes before handling
    /// - `Silent`: integrates the result without notifying the user
    pub fn tool_background_with_scheduling(
        mut self,
        tool_name: impl Into<String>,
        scheduling: gemini_genai_rs::prelude::FunctionResponseScheduling,
    ) -> Self {
        self.tool_execution_modes.insert(
            tool_name.into(),
            ToolExecutionMode::Background {
                formatter: None,
                scheduling: Some(scheduling),
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

    /// Enable thinking/reasoning with a token budget (Gemini 2.5+).
    ///
    /// Sets the thinking budget for the Live session. Use with
    /// `.include_thoughts()` and `.on_thought()` to receive thought summaries.
    ///
    /// ```ignore
    /// Live::builder()
    ///     .thinking(1024)
    ///     .include_thoughts()
    ///     .on_thought(|text| println!("[Thought] {text}"))
    /// ```
    ///
    /// **Platform support:** Google AI only. On Vertex AI, `thinkingConfig`
    /// is automatically stripped from the setup message.
    pub fn thinking(mut self, budget: u32) -> Self {
        self.config = self.config.thinking(budget);
        self
    }

    /// Include the model's thought summaries in responses.
    ///
    /// When enabled, the model emits `SessionEvent::Thought` events containing
    /// its reasoning process. Register an `.on_thought()` callback to receive them.
    ///
    /// **Platform support:** Google AI only. Stripped on Vertex AI.
    pub fn include_thoughts(mut self) -> Self {
        self.config = self.config.include_thoughts();
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

    /// Run the RNNoise speech enhancer over outgoing mic audio inside
    /// `send_audio` *(feature `denoise`)* — the same stage as
    /// [`voice::Denoiser`](crate::voice::Denoiser), applied server-side to
    /// hosted surfaces (web bridge, API server) that do not run a local
    /// pump. See the hardening chapter for the measured benchmark.
    #[cfg(feature = "denoise")]
    pub fn mic_denoise(mut self) -> Self {
        self.input_audio.stages.push(InputStage::Denoise);
        self
    }

    /// Chain a [`NoiseGate`](crate::voice::NoiseGate) over outgoing mic
    /// audio inside `send_audio`, after any denoiser — frames whose RMS
    /// falls below `threshold_rms` are silenced, with `hold_frames` of
    /// hangover. Calibrate the threshold between the caller's level and the
    /// background (measured sweet spot 400–700 behind the denoiser).
    pub fn mic_noise_gate(mut self, threshold_rms: f64, hold_frames: u32) -> Self {
        self.input_audio.stages.push(InputStage::NoiseGate {
            threshold_rms,
            hold_frames,
        });
        self
    }

    /// Chain any [`InputAudioProcessor`](gemini_adk_rs::live::InputAudioProcessor)
    /// over outgoing mic audio inside `send_audio` — the open slot for
    /// application-side stages (a DeepFilterNet enhancer, AGC, a custom
    /// filter). Stages run in the order configured.
    pub fn mic_processor(
        mut self,
        processor: impl gemini_adk_rs::live::InputAudioProcessor + 'static,
    ) -> Self {
        self.input_audio
            .stages
            .push(InputStage::Custom(Box::new(processor)));
        self
    }

    /// Replace the input VAD's configuration (the detector that runs inside
    /// `send_audio` for client-side speech edges). Use
    /// [`VadConfig::noisy_street()`](gemini_genai_rs::vad::VadConfig::noisy_street)
    /// behind [`mic_denoise`](Self::mic_denoise) for noisy environments.
    pub fn input_vad(mut self, config: gemini_genai_rs::vad::VadConfig) -> Self {
        self.input_audio.vad = Some(config);
        self
    }

    /// Give this client's input VAD interruption authority: the session is
    /// configured with the server's automatic activity detection disabled,
    /// and `send_audio` emits `activityStart`/`activityEnd` on the input
    /// VAD's speech edges. Measured ~2× faster barge-in than server
    /// authority; pair with [`mic_denoise`](Self::mic_denoise) (and
    /// [`mic_noise_gate`](Self::mic_noise_gate)) or noise will drive the
    /// marks.
    pub fn client_interruption_authority(mut self) -> Self {
        self.input_audio.client_authority = true;
        self
    }

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

    /// Resume a previous session from a server-issued resumption handle.
    ///
    /// Capture the handle from the old session before it ends — via
    /// [`LiveHandle::resume_handle`](gemini_adk_rs::live::LiveHandle::resume_handle)
    /// (e.g. inside the `on_go_away` callback) or from a persisted
    /// [`SessionSnapshot`](gemini_adk_rs::live::SessionSnapshot) — and pass it
    /// here on the next connect. Resumption stays enabled for the new session,
    /// so fresh handles keep arriving. No automatic reconnect is performed.
    pub fn session_resume_from(mut self, handle: impl Into<String>) -> Self {
        self.config = self.config.session_resumption(Some(handle.into()));
        self
    }

    /// Enable context window compression.
    pub fn context_compression(mut self, trigger_tokens: u32, target_tokens: u32) -> Self {
        self.config = self
            .config
            .context_window_compression(target_tokens)
            .context_window_trigger_tokens(trigger_tokens);
        self
    }

    // -- Control Plane --

    /// Enable soft turn detection for proactive silence awareness.
    ///
    /// When `proactiveAudio` is enabled, the model may choose not to respond.
    /// After VAD end, if the model stays silent for `timeout`, a lightweight
    /// "soft turn" updates state and fires watchers without forcing a response.
    pub fn soft_turn_timeout(mut self, timeout: Duration) -> Self {
        self.soft_turn_timeout = Some(timeout);
        self
    }

    /// Set the steering mode for how the phase machine delivers instructions.
    ///
    /// - `InstructionUpdate` (default): Replace system instruction on transition.
    /// - `ContextInjection`: Inject steering via `send_client_content`.
    /// - `Hybrid`: Instruction on transition, context injection per turn.
    pub fn steering_mode(mut self, mode: SteeringMode) -> Self {
        self.steering_mode = mode;
        self
    }

    /// Set when model-role context turns are delivered to the wire.
    ///
    /// - `Immediate` (default): Send as a single batched frame during
    ///   TurnComplete processing.
    /// - `Deferred`: Queue context and flush before the next user send
    ///   (`send_audio`/`send_text`/`send_video`).  Eliminates isolated
    ///   WebSocket frames during silence that can confuse the model.
    ///
    /// ```ignore
    /// Live::builder()
    ///     .steering_mode(SteeringMode::ContextInjection)
    ///     .context_delivery(ContextDelivery::Deferred)
    ///     .phase("greeting")
    ///         .instruction("Welcome the guest")
    ///         .done()
    ///     .initial_phase("greeting")
    /// ```
    pub fn context_delivery(mut self, mode: ContextDelivery) -> Self {
        self.context_delivery = mode;
        self
    }

    /// Set the fast-lane delivery (backpressure) policy for every event class.
    ///
    /// The event router forwards fast-lane frames (audio, text, transcripts,
    /// thoughts, VAD, phase) to the fast-lane consumer over a bounded channel.
    /// By default every class is [`Delivery::Lossless`] — the router awaits
    /// (`send().await`) when the channel is full, which preserves the historical
    /// behavior. Opt classes into [`Delivery::LossyDropNewest`] to drop the
    /// newest frame on overflow instead of stalling the router (and thereby
    /// stalling control-lane routing too).
    ///
    /// ```ignore
    /// use gemini_adk_rs::live::{Delivery, DeliveryConfig};
    /// Live::builder()
    ///     .delivery(DeliveryConfig::default()
    ///         .audio(Delivery::LossyDropNewest)
    ///         .transcript(Delivery::LossyDropNewest))
    /// ```
    pub fn delivery(mut self, delivery: DeliveryConfig) -> Self {
        self.delivery = delivery;
        self
    }

    /// Convenience: set the audio class to [`Delivery::LossyDropNewest`] so the
    /// router never blocks on a slow audio consumer, dropping the newest PCM
    /// frame on overflow. Other classes keep their current policy.
    pub fn lossy_audio(mut self) -> Self {
        self.delivery.audio = Delivery::LossyDropNewest;
        self
    }

    /// Convenience: set the transcript class to [`Delivery::LossyDropNewest`].
    /// Other classes keep their current policy.
    pub fn lossy_transcript(mut self) -> Self {
        self.delivery.transcript = Delivery::LossyDropNewest;
        self
    }

    /// Install transcript redaction: sensitive strings (card numbers,
    /// one-time codes, custom patterns) are removed at the event router,
    /// before callbacks, the transcript buffer, extraction, or persistence
    /// see the text.
    ///
    /// ```ignore
    /// use gemini_adk_rs::live::redaction::TranscriptRedactor;
    /// Live::builder()
    ///     .redaction(TranscriptRedactor::new().card_numbers().long_digits(6))
    /// ```
    pub fn redaction(
        mut self,
        redactor: gemini_adk_rs::live::redaction::TranscriptRedactor,
    ) -> Self {
        self.redactor = Some(redactor);
        self
    }

    /// Enable the conversation repair protocol.
    ///
    /// Tracks unfulfilled `needs` per phase. After `nudge_after` stalled turns,
    /// injects a gentle nudge. After `escalate_after` turns, sets
    /// `repair:escalation` in state for phase guards to handle.
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
    ///
    /// When enabled (default), the SDK injects a model-role context turn
    /// telling the model which tools are available in the new phase.
    pub fn tool_advisory(mut self, enabled: bool) -> Self {
        self.tool_advisory = enabled;
        self
    }
}

/// Input-audio hardening configuration accumulated by the builder and
/// applied to the [`LiveHandle`](gemini_adk_rs::live::LiveHandle) right
/// after connect. Stages run over each outgoing frame **in the order they
/// were configured** — chain the denoiser before the gate so the gate
/// calibrates on clean levels (see `Live::mic_denoise`,
/// `Live::mic_noise_gate`, `Live::mic_processor`, `Live::input_vad`,
/// `Live::client_interruption_authority`).
#[derive(Default)]
pub struct InputAudioConfig {
    /// Ordered mic-chain stages.
    pub stages: Vec<InputStage>,
    /// Replacement input-VAD configuration.
    pub vad: Option<gemini_genai_rs::vad::VadConfig>,
    /// Client VAD sends activity marks; server auto-detection is disabled.
    pub client_authority: bool,
}

/// One stage of the input mic chain — the named stages the SDK ships plus
/// an open [`Custom`](Self::Custom) slot for any
/// [`InputAudioProcessor`](gemini_adk_rs::live::InputAudioProcessor).
pub enum InputStage {
    /// RNNoise speech enhancement (feature `denoise`).
    #[cfg(feature = "denoise")]
    Denoise,
    /// Level gate: silences frames whose RMS falls below the threshold.
    NoiseGate {
        /// RMS threshold in sample units.
        threshold_rms: f64,
        /// Quiet frames the gate stays open after the last loud one.
        hold_frames: u32,
    },
    /// Any caller-supplied processor (denoisers, AGC, custom filters).
    Custom(Box<dyn gemini_adk_rs::live::InputAudioProcessor>),
}

impl InputAudioConfig {
    /// Whether any part of the input path is configured.
    pub fn is_configured(&self) -> bool {
        !self.stages.is_empty() || self.vad.is_some() || self.client_authority
    }

    /// Materialize the configured stages into runnable processors.
    pub(crate) fn build_processors(
        self,
    ) -> (
        Vec<Box<dyn gemini_adk_rs::live::InputAudioProcessor>>,
        Option<gemini_genai_rs::vad::VadConfig>,
        bool,
    ) {
        let processors = self
            .stages
            .into_iter()
            .map(
                |stage| -> Box<dyn gemini_adk_rs::live::InputAudioProcessor> {
                    match stage {
                        #[cfg(feature = "denoise")]
                        InputStage::Denoise => Box::new(crate::voice::Denoiser::new(16_000)),
                        InputStage::NoiseGate {
                            threshold_rms,
                            hold_frames,
                        } => Box::new(crate::voice::NoiseGate::new(threshold_rms, hold_frames)),
                        InputStage::Custom(processor) => processor,
                    }
                },
            )
            .collect();
        (processors, self.vad, self.client_authority)
    }
}

#[cfg(test)]
mod input_audio_tests {
    use super::*;

    struct Doubler;
    impl gemini_adk_rs::live::InputAudioProcessor for Doubler {
        fn process_frame(&mut self, frame: &mut Vec<i16>) {
            for s in frame.iter_mut() {
                *s = s.saturating_mul(2);
            }
        }
    }

    #[test]
    fn stages_run_in_configured_order() {
        let live = crate::live::Live::builder()
            .mic_processor(Doubler)
            .mic_noise_gate(400.0, 3)
            .input_vad(gemini_genai_rs::vad::VadConfig::noisy_street())
            .client_interruption_authority();
        assert!(live.input_audio.is_configured());
        assert_eq!(live.input_audio.stages.len(), 2);
        assert!(matches!(live.input_audio.stages[0], InputStage::Custom(_)));
        assert!(matches!(
            live.input_audio.stages[1],
            InputStage::NoiseGate { .. }
        ));
        let (mut processors, vad, client) = live.input_audio.build_processors();
        assert_eq!(processors.len(), 2);
        assert_eq!(vad.unwrap().start_threshold_db, 21.0);
        assert!(client);
        // The custom stage actually runs: 100 doubles to 200, then the gate
        // (RMS 200 < 400) silences the frame — order is observable.
        let mut frame = vec![100i16; 480];
        for p in processors.iter_mut() {
            p.process_frame(&mut frame);
        }
        assert!(
            frame.iter().all(|&s| s == 0),
            "gate should silence the doubled but still-quiet frame"
        );
    }
}
