//! `Live` — Fluent builder for callback-driven Gemini Live sessions.
//!
//! Wraps L1's `LiveSessionBuilder` with ergonomic callback registration
//! and integration with composition modules (M, T, P).
//!
//! # Callback Modes
//!
//! Control-lane callbacks support two execution modes via [`rs_adk::live::CallbackMode`]:
//!
//! - **Default methods** (e.g., `.on_turn_complete()`) → [`rs_adk::live::CallbackMode::Blocking`]
//! - **`_concurrent` methods** (e.g., `.on_turn_complete_concurrent()`) → [`rs_adk::live::CallbackMode::Concurrent`]
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

mod callbacks;
mod config;
mod connect;
mod extraction;
mod phases;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rs_adk::live::extractor::TurnExtractor;
use rs_adk::live::needs::RepairConfig;
use rs_adk::live::persistence::SessionPersistence;
use rs_adk::live::steering::SteeringMode;
use rs_adk::live::{
    ComputedRegistry, EventCallbacks, InstructionModifier, Phase, TemporalRegistry,
    ToolExecutionMode, WatcherRegistry,
};
use rs_adk::llm::BaseLlm;
use rs_adk::tool::ToolDispatcher;
use rs_genai::prelude::*;

/// A deferred agent tool registration (resolved at connect time when State is available).
pub(crate) struct DeferredAgentTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) agent: Arc<dyn rs_adk::text::TextAgent>,
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
    pub(crate) config: SessionConfig,
    pub(crate) callbacks: EventCallbacks,
    pub(crate) dispatcher: Option<ToolDispatcher>,
    pub(crate) extractors: Vec<Arc<dyn TurnExtractor>>,
    // L1 registries
    pub(crate) computed: ComputedRegistry,
    pub(crate) phases: Vec<Phase>,
    pub(crate) initial_phase: Option<String>,
    pub(crate) watchers: WatcherRegistry,
    pub(crate) temporal: TemporalRegistry,
    pub(crate) greeting: Option<String>,
    // Phase defaults: modifiers + prompt_on_enter inherited by all phases.
    pub(crate) phase_default_modifiers: Vec<InstructionModifier>,
    pub(crate) phase_default_prompt_on_enter: bool,
    // Per-tool execution modes (standard vs background).
    pub(crate) tool_execution_modes: HashMap<String, ToolExecutionMode>,
    // Deferred agent tools (resolved at connect time).
    pub(crate) deferred_agent_tools: Vec<DeferredAgentTool>,
    // LLMs to warm up at connect time.
    pub(crate) warm_up_llms: Vec<Arc<dyn BaseLlm>>,
    // Control plane configuration.
    pub(crate) soft_turn_timeout: Option<Duration>,
    pub(crate) steering_mode: SteeringMode,
    pub(crate) repair_config: Option<RepairConfig>,
    pub(crate) persistence: Option<Arc<dyn SessionPersistence>>,
    pub(crate) session_id: Option<String>,
    pub(crate) tool_advisory: bool,
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
            soft_turn_timeout: None,
            steering_mode: SteeringMode::default(),
            repair_config: None,
            persistence: None,
            session_id: None,
            tool_advisory: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

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
        use rs_adk::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
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
