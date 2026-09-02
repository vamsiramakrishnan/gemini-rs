//! Runtime contract introspection for Live sessions.
//!
//! A contract is a serializable description of the runtime configuration:
//! phases, tools, extractors, promotions, watchers, and control-plane knobs.
//! It intentionally describes stable metadata only; closures are represented
//! as booleans or human-readable predicate/debug labels.

use serde::{Deserialize, Serialize};

/// Serializable description of a configured Live runtime.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeContract {
    /// Contract schema version.
    pub version: u32,
    /// Configured Gemini model id.
    pub model: String,
    /// Tool declarations visible to the model.
    pub tools: Vec<ToolContract>,
    /// Conversation phase graph.
    pub phases: Vec<PhaseContract>,
    /// Initial phase name, when configured.
    pub initial_phase: Option<String>,
    /// Turn extractors and their promotion policy.
    pub extractors: Vec<ExtractorContract>,
    /// Computed state declarations.
    pub computed: Vec<ComputedContract>,
    /// State watcher declarations.
    pub watchers: Vec<WatcherContract>,
    /// Runtime and voice control settings.
    pub controls: ControlContract,
}

/// A tool declaration in the runtime contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContract {
    /// Function/tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
    /// Tool behavior, when declared by the target platform.
    pub behavior: Option<String>,
}

/// A conversation phase declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseContract {
    /// Phase name.
    pub name: String,
    /// Whether this phase is terminal.
    pub terminal: bool,
    /// Tools enabled in the phase; `None` means all tools.
    pub tools_enabled: Option<Vec<String>>,
    /// State keys this phase gathers.
    pub needs: Vec<String>,
    /// State keys required before entering the phase.
    pub requires: Vec<String>,
    /// Preparation effects registered for this phase.
    pub preparations: Vec<PreparationContract>,
    /// Semantic concepts presented on phase entry.
    pub presents: Vec<String>,
    /// State keys cleared on phase entry.
    pub clear_on_enter: Vec<String>,
    /// Outbound transitions.
    pub transitions: Vec<TransitionContract>,
    /// Whether the phase has an entry guard closure.
    pub has_guard: bool,
    /// Whether the phase prompts the model immediately on entry.
    pub prompt_on_enter: bool,
}

/// A phase transition declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionContract {
    /// Target phase.
    pub target: String,
    /// Human-readable transition description, when provided.
    pub description: Option<String>,
    /// Guards are closures, so this marks that a guard exists.
    pub has_guard: bool,
}

/// A phase preparation declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparationContract {
    /// Preparation name.
    pub name: String,
    /// State keys this preparation is expected to produce.
    pub produces: Vec<String>,
}

/// A turn extractor declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractorContract {
    /// Extractor name.
    pub name: String,
    /// Recent transcript turns consumed.
    pub window_size: usize,
    /// Trigger mode.
    pub trigger: String,
    /// Explicit promotion policy. Empty means every top-level non-null field
    /// is auto-flattened into state under its own name.
    pub promotions: Vec<PromotionContract>,
}

/// A field promotion declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionContract {
    /// Field in raw extractor output.
    pub field: String,
    /// Canonical state key written on acceptance.
    pub state_key: String,
    /// Merge policy.
    pub merge: String,
    /// Whether this promotion has an acceptance predicate.
    pub has_predicate: bool,
}

/// A computed state declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputedContract {
    /// Derived key without the `derived:` prefix.
    pub key: String,
    /// Source dependencies.
    pub dependencies: Vec<String>,
}

/// A watcher declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherContract {
    /// Watched state key.
    pub key: String,
    /// Predicate debug label.
    pub predicate: String,
    /// Whether the watcher blocks the control lane while running.
    pub blocking: bool,
}

/// Runtime control-plane settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlContract {
    /// Soft-turn timeout in milliseconds.
    pub soft_turn_timeout_ms: Option<u64>,
    /// Steering mode.
    pub steering_mode: String,
    /// Context delivery mode.
    pub context_delivery: String,
    /// Whether phase transition tool advisory is enabled.
    pub tool_advisory: bool,
    /// Telemetry interval in milliseconds.
    pub telemetry_interval_ms: Option<u64>,
    /// Whether repair is configured.
    pub repair_enabled: bool,
    /// Whether persistence is configured.
    pub persistence_enabled: bool,
}

impl Default for ControlContract {
    fn default() -> Self {
        Self {
            soft_turn_timeout_ms: None,
            steering_mode: "default".to_string(),
            context_delivery: "default".to_string(),
            tool_advisory: true,
            telemetry_interval_ms: None,
            repair_enabled: false,
            persistence_enabled: false,
        }
    }
}
