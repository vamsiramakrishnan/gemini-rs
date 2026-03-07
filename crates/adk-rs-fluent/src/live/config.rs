//! Model, session, and tool configuration methods for `Live`.

use std::sync::Arc;
use std::time::Duration;

use rs_adk::live::needs::RepairConfig;
use rs_adk::live::persistence::SessionPersistence;
use rs_adk::live::steering::{ContextDelivery, SteeringMode};
use rs_adk::live::{ResultFormatter, ToolExecutionMode};
use rs_adk::tool::ToolDispatcher;
use rs_genai::prelude::*;

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
        scheduling: rs_genai::prelude::FunctionResponseScheduling,
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
