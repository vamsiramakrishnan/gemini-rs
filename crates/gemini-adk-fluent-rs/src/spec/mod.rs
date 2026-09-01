//! `SessionSpec` — a whole Live session as one serializable JSON document.
//!
//! Where [`Flow`] made the *governance DAG* data, `SessionSpec` makes the
//! *application* data: session framing (instruction, greeting, modality),
//! declarative tool bindings (mock, HTTP, MCP), schema-as-JSON extraction that
//! fills the state guards read, data-driven phases and watchers over the same
//! closed [`Guard`] vocabulary, reusable flow fragments, and an embedded test
//! suite that replays scripted conversations through the real
//! [`FlowMonitor`](gemini_adk_rs::flow::FlowMonitor) offline.
//!
//! The invariants:
//! - **What serializes, runs.** [`SessionSpec::apply`] configures a
//!   [`Live`] builder from the document; nothing in the document needs Rust.
//! - **What can fail, fails at load time.** [`SessionSpec::validate`] runs the
//!   flow compiler, cross-checks tool names, and diffs the state keys guards
//!   *read* against the keys the session *writes* — the flow-level analogue of
//!   `compile_with_tools` for the dominant silent failure in data-authored
//!   flows (a guard waiting on a key nothing sets).
//! - **The escape hatches stay in code.** Custom closures (guards, tools,
//!   callbacks) are added on the returned builder after `apply`, exactly as
//!   before; the spec never pretends to serialize them.

mod codegen;
mod simulate;

pub use simulate::{
    SimEvent, SimSnapshot, SpecTest, TestExpectation, TestReport, TestStepResult, run_tests,
    trace_test,
};

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use gemini_adk_rs::expr::Expr;
use gemini_adk_rs::flow::{Constraint, Flow, Guard, Pred, Step};
use gemini_adk_rs::live::extractor::{ExtractionTrigger, FieldPromotion, LlmExtractor};
use gemini_adk_rs::live::{ContextDelivery, RepairConfig, SteeringMode};
use gemini_adk_rs::llm::BaseLlm;
use gemini_adk_rs::state::State;
use gemini_adk_rs::tool::{SimpleTool, ToolDispatcher};
use gemini_genai_rs::prelude::{
    AutomaticActivityDetection, Content, FunctionResponseScheduling, Sensitivity, SessionWriter,
    Voice,
};

use crate::compose::tools::T;
use crate::live::Live;

/// Output modality for a spec-driven session.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SpecModality {
    /// Text-only session (the default — no microphone needed).
    #[default]
    Text,
    /// Audio (voice) session.
    Audio,
}

/// An HTTP binding for a declared tool: the call is executed as an HTTP
/// request with `{args.field}` / `{state.key}` interpolation in the URL,
/// headers, and body strings, and the JSON response is returned to the model.
///
/// Requires the `http-tools` feature; without it, validation reports the
/// binding as unsupported instead of failing silently at call time.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HttpBinding {
    /// HTTP method (GET, POST, PUT, PATCH, DELETE). Default GET.
    #[serde(default = "default_method")]
    pub method: String,
    /// Request URL, with `{args.*}`/`{state.*}` interpolation.
    pub url: String,
    /// Request headers, values interpolated.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// JSON body; every string value is interpolated. Omit for no body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

fn default_method() -> String {
    "GET".to_string()
}

/// A declared tool. Without an `http` binding it is a **mock**: it returns
/// `response` (default `{"ok": true}`) and writes `set_state` — enough to
/// model, validate, and demo a governed conversation before any real tool
/// exists. With `http` it performs the request instead (and still applies
/// `set_state` afterwards, so guards latch identically) — swap a mock for a
/// binding without touching the flow.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ToolSpec {
    /// Tool (function) name the model calls.
    pub name: String,
    /// Description shown to the model.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the arguments (Gemini subset). `None` = no parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    /// Canned JSON response (mock tools). Default `{"ok": true}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// State keys written when the tool runs — how a tool latches guards.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set_state: BTreeMap<String, Value>,
    /// Store the tool's full response under this state key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_response_as: Option<String>,
    /// Execute as an HTTP request instead of returning the canned response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpBinding>,
    /// Run non-blocking: the model keeps speaking while the tool executes
    /// (`behavior: NonBlocking` on the wire; Google AI only, stripped on
    /// Vertex). Implied by `scheduling`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    /// How the async response is delivered (implies `background`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling: Option<SchedulingSpec>,
}

/// Delivery mode for a background tool's response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchedulingSpec {
    /// Halt current output and report immediately.
    Interrupt,
    /// Wait until the model finishes its current output.
    WhenIdle,
    /// Integrate silently without notifying the user.
    Silent,
}

impl SchedulingSpec {
    fn to_wire(self) -> FunctionResponseScheduling {
        match self {
            SchedulingSpec::Interrupt => FunctionResponseScheduling::Interrupt,
            SchedulingSpec::WhenIdle => FunctionResponseScheduling::WhenIdle,
            SchedulingSpec::Silent => FunctionResponseScheduling::Silent,
        }
    }
}

/// How extracted fields are promoted into bare state keys.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PromotePolicy {
    /// Write once, never overwrite a known value (default).
    #[default]
    KeepKnown,
    /// Always overwrite.
    Overwrite,
    /// Promote only when the extracted value is `true`.
    TrueOnly,
    /// Promote only non-empty values.
    NonEmpty,
}

/// Promote one extracted field into a session state key, where flow guards
/// (`captured`, `is_true`, …) read it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PromoteSpec {
    /// Field name in the extraction result.
    pub field: String,
    /// Target state key. Defaults to the field name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Merge policy.
    #[serde(default)]
    pub policy: PromotePolicy,
}

impl PromoteSpec {
    fn target(&self) -> &str {
        self.to.as_deref().unwrap_or(&self.field)
    }

    fn to_rule(&self) -> FieldPromotion {
        let rule = match self.policy {
            PromotePolicy::KeepKnown => FieldPromotion::keep_known(&self.field),
            PromotePolicy::Overwrite => FieldPromotion::overwrite(&self.field),
            PromotePolicy::TrueOnly => FieldPromotion::true_only(&self.field),
            PromotePolicy::NonEmpty => FieldPromotion::non_empty(&self.field),
        };
        match &self.to {
            Some(key) => rule.to(key),
            None => rule,
        }
    }
}

/// When an extractor runs.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TriggerSpec {
    /// After every turn (default).
    #[default]
    EveryTurn,
    /// After tool calls complete.
    AfterToolCall,
    /// On generation complete — before interruption truncation.
    OnGenerationComplete,
    /// When a phase transition occurs.
    OnPhaseChange,
}

impl TriggerSpec {
    fn to_trigger(self) -> ExtractionTrigger {
        match self {
            TriggerSpec::EveryTurn => ExtractionTrigger::EveryTurn,
            TriggerSpec::AfterToolCall => ExtractionTrigger::AfterToolCall,
            TriggerSpec::OnGenerationComplete => ExtractionTrigger::OnGenerationComplete,
            TriggerSpec::OnPhaseChange => ExtractionTrigger::OnPhaseChange,
        }
    }
}

/// Schema-as-JSON out-of-band extraction: an OOB model fills `schema` from the
/// transcript, the result lands in state under `name`, and `promote` rules
/// write individual fields to bare keys — closing the loop that lets a flow
/// advance from *speech alone* (`captured` guards latch with no tool call).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtractSpec {
    /// Extraction name — the state key the full result is stored under.
    pub name: String,
    /// Instruction for the extraction model.
    pub instruction: String,
    /// JSON Schema the extraction must satisfy.
    pub schema: Value,
    /// Transcript window in turns. Default 3.
    #[serde(default = "default_window")]
    pub window: usize,
    /// When to run.
    #[serde(default)]
    pub trigger: TriggerSpec,
    /// Field-promotion rules into bare state keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promote: Vec<PromoteSpec>,
}

fn default_window() -> usize {
    3
}

/// A serializable side effect for phase entry, watcher, and pattern actions —
/// the closed-effect counterpart to [`Guard`]'s closed predicates. One
/// vocabulary, honored identically wherever effects fire.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectSpec {
    /// Write these state keys.
    Set(BTreeMap<String, Value>),
    /// Inject a model-role context turn (steering text the model reads before
    /// its next response — it does not answer it directly).
    Context(String),
    /// Inject the text as a model-role turn **and** ask the model to respond
    /// now — the "make the model speak" effect (proactive check-in, nudge).
    Prompt(String),
    /// Durably remember a note (`{state.key}` templates interpolated) through
    /// the session's memory binding. Requires the spec's `memory` section.
    Remember(String),
}

/// A data-driven phase transition: fire `when` the guard holds over state.
///
/// Guards here evaluate against state alone (no flow marking), so
/// `called_ok`/`done` atoms are rejected by validation — use a state key a
/// tool or extractor writes instead.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TransitionSpec {
    /// Target phase.
    pub to: String,
    /// Condition over session state.
    pub when: Guard,
    /// Optional description for navigation steering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A data-driven conversation phase.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PhaseSpec {
    /// Phase name.
    pub name: String,
    /// Instruction while this phase is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// Tool names available in this phase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Required state keys (drives conversation repair).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// Effects fired on phase entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_enter: Vec<EffectSpec>,
    /// Guarded transitions out of this phase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<TransitionSpec>,
    /// Prompt the model on entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_on_enter: Option<bool>,
    /// Terminal phase.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

/// A data-driven state watcher condition.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WatchCondition {
    /// Any change.
    Changed,
    /// Changed to exactly this value.
    ChangedTo(Value),
    /// Numeric value crossed above the threshold.
    CrossedAbove(f64),
    /// Numeric value crossed below the threshold.
    CrossedBelow(f64),
    /// Became `true`.
    BecameTrue,
    /// Became `false`.
    BecameFalse,
}

/// A data-driven state watcher: when `key` satisfies the condition, run the
/// effects. Watchers receive the live session writer, so the full
/// [`EffectSpec`] vocabulary applies — a watcher can set state, inject
/// context, prompt the model, or remember durably.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchSpec {
    /// Watched state key.
    pub key: String,
    /// Trigger condition.
    pub condition: WatchCondition,
    /// State keys written when it fires (sugar for an [`EffectSpec::Set`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, Value>,
    /// Effects fired in order after `set`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectSpec>,
}

/// A data-driven temporal pattern: fire effects when a state condition holds
/// continuously — for a duration (`sustained_secs`) or a number of
/// consecutive turns (`turns`). Exactly one of the two must be set.
///
/// This is the "the caller has sounded confused for 30 seconds" /
/// "we've been stuck on this for 3 turns" reactor, as data.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PatternSpec {
    /// Pattern name (diagnostic identity).
    pub name: String,
    /// Condition over session state (no marking atoms).
    pub when: Guard,
    /// Fire after the condition holds this many seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sustained_secs: Option<u64>,
    /// Fire after the condition holds for this many consecutive turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
    /// Effects when the pattern fires (`set` state and/or inject `context`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectSpec>,
}

/// A computed (derived) state variable authored as data: `key` is written to
/// `derived:{key}` whenever the [`Expr`] evaluates to a value. Dependencies
/// are inferred from the expression's [`Expr::keys_read`], so the runtime's
/// dependency-ordered [`ComputedRegistry`](gemini_adk_rs::live::ComputedRegistry)
/// invariants hold with nothing extra to declare. Guards read the result by
/// its bare key (the `derived:` fallback).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ComputedSpec {
    /// Result key (written as `derived:{key}`).
    pub key: String,
    /// The expression computing the value.
    pub from: Expr,
    /// Human-readable note.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Declared JSON type of a state key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StateType {
    /// JSON boolean.
    Boolean,
    /// JSON number.
    Number,
    /// JSON string.
    String,
    /// JSON object.
    Object,
    /// JSON array.
    Array,
}

impl StateType {
    fn matches(self, value: &Value) -> bool {
        matches!(
            (self, value),
            (StateType::Boolean, Value::Bool(_))
                | (StateType::Number, Value::Number(_))
                | (StateType::String, Value::String(_))
                | (StateType::Object, Value::Object(_))
                | (StateType::Array, Value::Array(_))
        )
    }
}

/// One declared state key: its type, meaning, and optional starting value.
///
/// The `state` section is the session's data dictionary. Declaring it is
/// optional, but once present it powers editor autocomplete, key-existence
/// warnings for every guard/effect/tool reference, typed key constants in
/// generated code, and `default` seeding at connect.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StateFieldSpec {
    /// Declared JSON type.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<StateType>,
    /// What this key means.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Initial value seeded at connect when the key is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// Project one remembered fact into a governed state slot: when memory holds
/// a value for `predicate`, it is written to the `to` state key — where
/// `needs`, `captured`, and every other guard reads it exactly as if the
/// caller had just said it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemorySlotSpec {
    /// Memory predicate (e.g. `dietary_identity`).
    pub predicate: String,
    /// Target state key (must not be `derived:` — that prefix is read-only).
    pub to: String,
}

/// The session's durable-memory declaration. Installing it wires the memory
/// subsystem in through a [`MemoryBinding`] supplied in [`SpecResources`]:
/// the `recall_context` / `manage_memory` tools (ambient, so step `allow`
/// lists don't switch recall off), turn ingestion, end-of-session
/// reconciliation, and the slot projections below. [`EffectSpec::Remember`]
/// writes through the same binding.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemorySpec {
    /// Remembered facts projected into state slots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<MemorySlotSpec>,
}

/// Speech-detection sensitivity for [`VadSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SensitivitySpec {
    /// Fewer false positives; may miss soft speech.
    Low,
    /// Balanced.
    Medium,
    /// Catches everything; more false positives.
    High,
}

impl SensitivitySpec {
    fn to_wire(self) -> Sensitivity {
        match self {
            SensitivitySpec::Low => Sensitivity::SensitivityLow,
            SensitivitySpec::Medium => Sensitivity::SensitivityMedium,
            SensitivitySpec::High => Sensitivity::SensitivityHigh,
        }
    }
}

/// Voice-activity-detection tuning — the knobs that decide how eagerly the
/// session hears speech start and stop.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VadSpec {
    /// Sensitivity for detecting speech onset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_sensitivity: Option<SensitivitySpec>,
    /// Sensitivity for detecting end of speech.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_sensitivity: Option<SensitivitySpec>,
    /// Milliseconds of audio kept before speech onset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_padding_ms: Option<u32>,
    /// Milliseconds of silence before end-of-speech triggers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silence_duration_ms: Option<u32>,
}

/// Input-audio hardening: the measured mic chain (denoiser, noise gate),
/// client input-VAD tuning, and interruption authority. Lowers to
/// `Live::mic_denoise` / `mic_noise_gate` / `input_vad` /
/// `client_interruption_authority`; see the hardening chapter for the
/// benchmark behind each default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AudioSpec {
    /// Run the RNNoise speech enhancer over incoming user audio (requires
    /// the `denoise` feature; skipped with a validation warning otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise: Option<bool>,
    /// Noise gate after the denoiser — silences frames below a level
    /// threshold (near-talker preference; rejects denoiser residue).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noise_gate: Option<NoiseGateSpec>,
    /// Client input-VAD tuning (the detector driving speech edges in
    /// `send_audio`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_vad: Option<ClientVadSpec>,
    /// Who decides when user speech interrupts the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<AuthoritySpec>,
    /// Milliseconds to hold the turn-end marker during mid-turn pauses, suppressing
    /// false end-of-turn commits. Measured on TurnBench dev: 800 ms = 0.1-fp
    /// qualifying point (recall 0.798), 1600 ms = recall 0.508 (frontier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eot_hold_ms: Option<u32>,
    /// Milliseconds to suppress barge-in detection on backchannels and false
    /// interruptions. Measured on TurnBench dev: 1400 ms suppresses backchannel
    /// false positives (0.702 → 0.062), 2000 ms = maximum interruption match window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_interruption_ms: Option<u32>,
}

/// Noise-gate stage parameters.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NoiseGateSpec {
    /// RMS threshold in sample units (i16 full scale 32767; measured sweet
    /// spot 400–700 behind the denoiser).
    #[serde(default = "default_gate_threshold")]
    pub threshold_rms: f64,
    /// Quiet frames the gate stays open after the last loud one.
    #[serde(default = "default_gate_hold")]
    pub hold_frames: u32,
}

fn default_gate_threshold() -> f64 {
    700.0
}
fn default_gate_hold() -> u32 {
    3
}

/// Client input-VAD tuning: start from a preset, override individual knobs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClientVadSpec {
    /// Base preset the overrides apply to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<ClientVadPreset>,
    /// Energy above the noise floor (dB) to open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_threshold_db: Option<f64>,
    /// Energy above the noise floor (dB) to close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_threshold_db: Option<f64>,
    /// Consecutive frames (30 ms each) to confirm onset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_speech_frames: Option<u32>,
    /// Frames of hangover before speech-end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hangover_frames: Option<u32>,
}

/// Named client-VAD starting points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClientVadPreset {
    /// The library defaults (quiet environments).
    Default,
    /// The closed-loop-tuned noisy-environment profile — use behind
    /// `denoise: true` (see `VadConfig::noisy_street`).
    NoisyStreet,
}

impl ClientVadSpec {
    /// Lower to a wire `VadConfig`: preset base plus overrides.
    pub fn to_config(&self) -> gemini_genai_rs::vad::VadConfig {
        let mut config = match self.preset {
            Some(ClientVadPreset::NoisyStreet) => gemini_genai_rs::vad::VadConfig::noisy_street(),
            _ => gemini_genai_rs::vad::VadConfig::default(),
        };
        if let Some(v) = self.start_threshold_db {
            config.start_threshold_db = v;
        }
        if let Some(v) = self.stop_threshold_db {
            config.stop_threshold_db = v;
        }
        if let Some(v) = self.min_speech_frames {
            config.min_speech_frames = v;
        }
        if let Some(v) = self.hangover_frames {
            config.hangover_frames = v;
        }
        config
    }
}

/// Interruption authority (measured trade: client is ~2× faster to barge
/// in; server posted zero false interruptions in every benchmark run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthoritySpec {
    /// The Live API's automatic activity detection decides (default).
    Server,
    /// This client's input VAD decides: automatic detection is disabled and
    /// speech edges send activityStart/activityEnd.
    Client,
}

/// Input/output transcription toggles.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptionSpec {
    /// Transcribe the user's speech.
    #[serde(default = "default_true")]
    pub input: bool,
    /// Transcribe the model's audio output.
    #[serde(default = "default_true")]
    pub output: bool,
}

fn default_true() -> bool {
    true
}

/// How phase instructions are steered to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SteeringSpec {
    /// Replace the system instruction on phase transitions (default).
    InstructionUpdate,
    /// Set the instruction once; deliver phase steering as context turns.
    ContextInjection,
    /// Both.
    Hybrid,
}

/// When batched context turns hit the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextDeliverySpec {
    /// Send immediately during TurnComplete (default).
    Immediate,
    /// Queue and flush with the next user send (voice-glitch avoidance).
    Deferred,
}

/// Conversation-repair thresholds (unmet phase `needs`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepairSpec {
    /// Turns without progress before the first nudge.
    pub nudge_after: u32,
    /// Turns without progress before escalation.
    pub escalate_after: u32,
}

/// Session persistence backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceSpec {
    /// Filesystem snapshots under the given directory.
    Fs {
        /// Snapshot directory (created if missing).
        dir: String,
    },
    /// In-memory (tests, ephemeral sessions).
    Memory,
}

/// Control-plane and voice tuning — every session capability that is
/// configuration rather than conversation, in one section. Everything here
/// lowers to a `Live` builder setter; omitted fields keep the builder's
/// defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RuntimeSpec {
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Thinking budget in tokens (Google AI only; stripped on Vertex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// Receive thought summaries (requires `thinking_budget`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
    /// Input/output transcription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription: Option<TranscriptionSpec>,
    /// Let the model choose to stay silent (proactive audio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactive_audio: Option<bool>,
    /// Voice-activity-detection tuning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vad: Option<VadSpec>,
    /// Input-audio hardening: mic chain, client VAD, interruption authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioSpec>,
    /// Fire a soft turn when the model stays silent this long after VAD end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_turn_timeout_ms: Option<u64>,
    /// How phase instructions reach the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steering: Option<SteeringSpec>,
    /// When context turns hit the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_delivery: Option<ContextDeliverySpec>,
    /// Conversation-repair thresholds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<RepairSpec>,
    /// Session persistence backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence: Option<PersistenceSpec>,
    /// Stable session id (resume across restarts; requires `persistence`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Drop audio chunks instead of applying backpressure when consumers lag.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lossy_audio: bool,
    /// Drop transcript deltas instead of applying backpressure.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lossy_transcript: bool,
}

/// Splice a named flow fragment into the session's flow under a namespace.
///
/// Every step id inside the fragment becomes `{namespace}/{id}`; internal
/// `after` edges, `done(step)` guard atoms, and `before`/`require` constraints
/// are rewritten to match. Fragment root steps (no `after`) gain this entry's
/// `after` dependencies, attaching the fragment into the outer DAG. Steps
/// outside the fragment reference its steps as `{namespace}/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UseFragment {
    /// Fragment name (a key in [`SessionSpec::fragments`]).
    pub fragment: String,
    /// Namespace prefix for the spliced step ids.
    pub namespace: String,
    /// Outer steps the fragment's roots depend on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
}

/// A complete Live session as one JSON document. See the module docs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSpec {
    /// App name (display and registry identity).
    #[serde(default)]
    pub name: String,
    /// Spec version tag (freeform, e.g. "1" or "2025-08-24").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Base system instruction.
    #[serde(default)]
    pub instruction: String,
    /// Optional greeting prompt — the model speaks first on connect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting: Option<String>,
    /// Output modality. Defaults to text.
    #[serde(default)]
    pub modality: SpecModality,
    /// Voice name for audio sessions (e.g. "Puck").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Declared tools (mock or HTTP-bound).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// MCP toolset connection strings — the whole MCP ecosystem as this app's
    /// tool library. Resolved at connect time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<String>,
    /// Out-of-band extraction pipelines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extract: Vec<ExtractSpec>,
    /// Declared state keys — the session's data dictionary.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state: BTreeMap<String, StateFieldSpec>,
    /// Computed (derived) state variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub computed: Vec<ComputedSpec>,
    /// Durable memory: slots projected into state, `remember` effects, and
    /// the ambient recall/manage tools. Requires a [`MemoryBinding`] in
    /// [`SpecResources`] at apply time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySpec>,
    /// Control-plane and voice tuning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeSpec>,
    /// Conversation phases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseSpec>,
    /// Initial phase name (required when `phases` is non-empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_phase: Option<String>,
    /// State watchers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<WatchSpec>,
    /// Temporal patterns (sustained / consecutive-turn conditions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<PatternSpec>,
    /// Reusable flow fragments, spliced via `use_fragments`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fragments: BTreeMap<String, Flow>,
    /// Fragment splice directives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub use_fragments: Vec<UseFragment>,
    /// The governed flow DAG (optional — a spec may be phases-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<Flow>,
    /// Embedded conformance tests, replayed offline by [`run_tests`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<SpecTest>,
}

/// Structured result of validating a [`SessionSpec`].
#[derive(Debug, Clone, Serialize)]
pub struct SpecValidation {
    /// Whether the spec compiled cleanly.
    pub valid: bool,
    /// Errors (empty when valid).
    pub errors: Vec<String>,
    /// Non-fatal advisories.
    pub warnings: Vec<String>,
    /// Mermaid rendering of the effective (fragment-spliced) flow.
    pub mermaid: String,
    /// Every tool name the flow references.
    pub tools: Vec<String>,
    /// Steps in the effective flow.
    pub steps: usize,
}

/// The seam through which a memory engine plugs into a spec-driven session.
///
/// The spec's `memory` section is pure data; the engine that honors it lives
/// above this crate (`gemini-memory-rs` implements this trait over its
/// `MemorySession`). `apply` calls [`install`](Self::install) once to wire
/// tools/ingestion/slots, and routes every [`EffectSpec::Remember`] through
/// [`remember`](Self::remember).
pub trait MemoryBinding: Send + Sync {
    /// Wire the memory subsystem onto the builder per the spec's declaration.
    fn install(&self, live: Live, memory: &MemorySpec) -> Live;
    /// Durably remember a note (fire-and-forget; implementations may commit
    /// asynchronously).
    fn remember(&self, note: String);
}

/// Tool names a [`MemoryBinding`] installs (ambient on the flow).
pub const MEMORY_TOOL_NAMES: [&str; 2] = ["recall_context", "manage_memory"];

/// External resources a spec cannot carry: model handles and capability
/// bindings.
#[derive(Default)]
pub struct SpecResources {
    /// The OOB model backing `extract` entries. Required when any are present.
    pub extraction_llm: Option<Arc<dyn BaseLlm>>,
    /// The memory engine honoring the spec's `memory` section. Required when
    /// that section is present.
    pub memory: Option<Arc<dyn MemoryBinding>>,
}

impl SessionSpec {
    /// Parse a spec from a JSON value. Accepts a full document or a *bare
    /// flow* (`{"steps": [...]}`), which is wrapped in a default spec.
    pub fn from_value(value: Value) -> Result<Self, String> {
        let is_bare_flow = value.get("flow").is_none() && value.get("steps").is_some();
        if is_bare_flow {
            let flow: Flow =
                serde_json::from_value(value).map_err(|e| format!("invalid flow JSON: {e}"))?;
            return Ok(Self {
                flow: Some(flow),
                ..Self::default()
            });
        }
        serde_json::from_value(value).map_err(|e| format!("invalid session spec JSON: {e}"))
    }

    /// The JSON Schema of the spec document itself — for editor autocomplete
    /// and for validating machine-authored specs at generation time.
    pub fn json_schema() -> Value {
        serde_json::to_value(schemars::schema_for!(SessionSpec)).unwrap_or_else(|_| json!({}))
    }

    /// Declared tool names (mock/HTTP; MCP names resolve at connect).
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name.clone()).collect()
    }

    /// The flow with every `use_fragments` directive spliced in.
    pub fn effective_flow(&self) -> Result<Flow, Vec<String>> {
        let mut flow = self.flow.clone().unwrap_or_default();
        let mut errors = Vec::new();
        for use_frag in &self.use_fragments {
            match self.fragments.get(&use_frag.fragment) {
                Some(fragment) => {
                    splice_fragment(&mut flow, fragment, use_frag, &mut errors);
                }
                None => errors.push(format!(
                    "use_fragments references unknown fragment '{}'",
                    use_frag.fragment
                )),
            }
        }
        if errors.is_empty() {
            Ok(flow)
        } else {
            Err(errors)
        }
    }

    /// Every state key the session declares a *writer* for: tool `set_state`
    /// and `save_response_as`, extraction names and promotion targets, phase
    /// and watcher `set` effects.
    pub fn state_keys_written(&self) -> std::collections::BTreeSet<String> {
        let mut keys = std::collections::BTreeSet::new();
        for t in &self.tools {
            keys.extend(t.set_state.keys().cloned());
            keys.extend(t.save_response_as.iter().cloned());
        }
        for e in &self.extract {
            keys.insert(e.name.clone());
            for p in &e.promote {
                keys.insert(p.target().to_string());
            }
        }
        for p in &self.phases {
            for eff in &p.on_enter {
                if let EffectSpec::Set(map) = eff {
                    keys.extend(map.keys().cloned());
                }
            }
        }
        for w in &self.watch {
            keys.extend(w.set.keys().cloned());
            for eff in &w.effects {
                if let EffectSpec::Set(map) = eff {
                    keys.extend(map.keys().cloned());
                }
            }
        }
        for p in &self.patterns {
            for eff in &p.effects {
                if let EffectSpec::Set(map) = eff {
                    keys.extend(map.keys().cloned());
                }
            }
        }
        for c in &self.computed {
            // A computed var writes `derived:{key}`, and guards read it by
            // either name thanks to the `derived:` fallback.
            keys.insert(c.key.clone());
            keys.insert(format!("derived:{}", c.key));
        }
        if let Some(memory) = &self.memory {
            for slot in &memory.slots {
                keys.extend([slot.to.clone()]);
            }
        }
        for (key, field) in &self.state {
            if field.default.is_some() {
                keys.insert(key.clone());
            }
        }
        keys
    }

    /// Every [`EffectSpec`] anywhere in the document, with a location label.
    fn all_effects(&self) -> Vec<(String, &EffectSpec)> {
        let mut out = Vec::new();
        for p in &self.phases {
            for eff in &p.on_enter {
                out.push((format!("phase '{}'", p.name), eff));
            }
        }
        for w in &self.watch {
            for eff in &w.effects {
                out.push((format!("watch '{}'", w.key), eff));
            }
        }
        for p in &self.patterns {
            for eff in &p.effects {
                out.push((format!("pattern '{}'", p.name), eff));
            }
        }
        out
    }

    /// Re-evaluate every computed variable in dependency order, writing
    /// results to `derived:{key}`. Used by the offline simulator so guards
    /// over computed keys latch exactly as they do live. Validation forbids
    /// cycles, so iterating to a fixed point terminates.
    pub(crate) fn recompute_computed(&self, state: &State) {
        for _ in 0..self.computed.len() {
            let mut changed = false;
            for c in &self.computed {
                if let Some(value) = c.from.eval(state) {
                    let derived = format!("derived:{}", c.key);
                    if state.get_raw(&derived).as_ref() != Some(&value) {
                        let _ = state.set(&derived, value);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Seed declared `state` defaults for keys not yet set.
    pub(crate) fn seed_state_defaults(&self, state: &State) {
        for (key, field) in &self.state {
            if let Some(default) = &field.default
                && state.get_raw(key).is_none()
            {
                let _ = state.set(key, default.clone());
            }
        }
    }

    /// Validate the whole document: flow compilation (with the declared tool
    /// registry), fragment splicing, phase-guard restrictions, HTTP-binding
    /// support, and the read/write state-key diff.
    pub fn validate(&self) -> SpecValidation {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        // Fragment namespace validation: must be non-empty to create valid step ids.
        for use_frag in &self.use_fragments {
            if use_frag.namespace.is_empty() {
                errors.push(
                    "use_fragments directive has empty namespace — step ids would be malformed"
                        .into(),
                );
            }
        }

        let flow = match self.effective_flow() {
            Ok(flow) => flow,
            Err(errs) => {
                errors.extend(errs);
                self.flow.clone().unwrap_or_default()
            }
        };
        let mermaid = flow.to_mermaid();
        let steps = flow.steps.len();
        let has_flow = !flow.steps.is_empty();

        if !has_flow && self.phases.is_empty() {
            errors.push("spec has neither a flow nor phases — nothing to run".into());
        }
        if !self.phases.is_empty() && self.initial_phase.is_none() {
            errors.push("phases are declared but initial_phase is not set".into());
        }
        if let Some(initial) = &self.initial_phase
            && !self.phases.iter().any(|p| &p.name == initial)
        {
            errors.push(format!("initial_phase '{initial}' is not a declared phase"));
        }
        // Check for duplicate phase names
        {
            let phase_names: std::collections::BTreeSet<&str> =
                self.phases.iter().map(|p| p.name.as_str()).collect();
            if phase_names.len() != self.phases.len() {
                let mut seen = std::collections::BTreeSet::new();
                for p in &self.phases {
                    if !seen.insert(p.name.as_str()) {
                        errors.push(format!("phase '{}' is declared more than once", p.name));
                    }
                }
            }
        }
        for pattern in &self.patterns {
            match (pattern.sustained_secs, pattern.turns) {
                (Some(_), Some(_)) | (None, None) => errors.push(format!(
                    "pattern '{}' must set exactly one of sustained_secs or turns",
                    pattern.name
                )),
                _ => {}
            }
            if guard_uses_marking(&pattern.when) {
                errors.push(format!(
                    "pattern '{}' uses a called_ok/done atom — pattern guards see state only",
                    pattern.name
                ));
            }
        }
        for p in &self.phases {
            for t in &p.transitions {
                if guard_uses_marking(&t.when) {
                    errors.push(format!(
                        "phase '{}' transition to '{}' uses a called_ok/done atom — phase guards \
                         see state only (no flow marking); latch a state key instead",
                        p.name, t.to
                    ));
                }
            }
        }
        if cfg!(not(feature = "http-tools")) {
            for t in &self.tools {
                if t.http.is_some() {
                    errors.push(format!(
                        "tool '{}' has an http binding but the `http-tools` feature is not \
                         enabled",
                        t.name
                    ));
                }
            }
        }

        // Computed variables: unique keys, no dependency cycles (Kahn over the
        // computed subset; a bare read and a `derived:` read are the same
        // dependency).
        {
            let computed_keys: std::collections::BTreeSet<&str> =
                self.computed.iter().map(|c| c.key.as_str()).collect();
            if computed_keys.len() != self.computed.len() {
                errors.push("computed variables declare a duplicate key".into());
            }
            let normalize = |k: &str| k.strip_prefix("derived:").unwrap_or(k).to_string();
            let mut in_degree: BTreeMap<&str, usize> =
                computed_keys.iter().map(|k| (*k, 0)).collect();
            let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for c in &self.computed {
                for dep in c.from.keys_read() {
                    let dep = normalize(&dep);
                    if dep != c.key && computed_keys.contains(dep.as_str()) {
                        let dep_key = *computed_keys.get(dep.as_str()).unwrap();
                        dependents.entry(dep_key).or_default().push(c.key.as_str());
                        *in_degree.entry(c.key.as_str()).or_default() += 1;
                    }
                    if dep == c.key {
                        errors.push(format!("computed '{}' reads its own key", c.key));
                    }
                }
            }
            let mut queue: Vec<&str> = in_degree
                .iter()
                .filter(|(_, d)| **d == 0)
                .map(|(k, _)| *k)
                .collect();
            let mut visited = 0usize;
            while let Some(key) = queue.pop() {
                visited += 1;
                for dependent in dependents.get(key).cloned().unwrap_or_default() {
                    let d = in_degree.get_mut(dependent).unwrap();
                    *d -= 1;
                    if *d == 0 {
                        queue.push(dependent);
                    }
                }
            }
            if visited != computed_keys.len() {
                let cycle: Vec<&str> = in_degree
                    .iter()
                    .filter(|(_, d)| **d > 0)
                    .map(|(k, _)| *k)
                    .collect();
                errors.push(format!(
                    "computed variables form a dependency cycle: {}",
                    cycle.join(", ")
                ));
            }
        }

        // Effects: `remember` needs the memory section; memory slots must not
        // target the read-only `derived:` scope.
        for (location, effect) in self.all_effects() {
            if matches!(effect, EffectSpec::Remember(_)) && self.memory.is_none() {
                errors.push(format!(
                    "{location} uses a `remember` effect but the spec has no `memory` section"
                ));
            }
        }
        if let Some(memory) = &self.memory {
            for slot in &memory.slots {
                if slot.to.starts_with("derived:") {
                    errors.push(format!(
                        "memory slot '{}' targets read-only key '{}' — the `derived:` scope \
                         belongs to computed variables",
                        slot.predicate, slot.to
                    ));
                }
            }
        }

        // Declared state dictionary: defaults must match their declared type;
        // once a dictionary exists, undeclared keys are worth flagging.
        for (key, field) in &self.state {
            if let (Some(kind), Some(default)) = (field.kind, &field.default)
                && !kind.matches(default)
            {
                warnings.push(format!(
                    "state key '{key}' declares type {kind:?} but its default is {default}"
                ));
            }
        }
        if !self.state.is_empty() {
            let declared: std::collections::BTreeSet<&str> =
                self.state.keys().map(String::as_str).collect();
            let mut undeclared = std::collections::BTreeSet::new();
            for key in self.state_keys_written() {
                let bare = key.strip_prefix("derived:").unwrap_or(&key);
                if !declared.contains(bare) && !self.computed.iter().any(|c| c.key == bare) {
                    undeclared.insert(key.clone());
                }
            }
            for key in &undeclared {
                warnings.push(format!(
                    "state key '{key}' is written but not declared in the `state` section"
                ));
            }
        }

        // Computed inputs: like guard reads, a dependency nothing writes can
        // never produce a value.
        {
            let written = self.state_keys_written();
            for c in &self.computed {
                for dep in c.from.keys_read() {
                    let bare = dep.strip_prefix("derived:").unwrap_or(&dep);
                    if !written.contains(&dep)
                        && !written.contains(bare)
                        && !dep.ends_with(":result")
                    {
                        warnings.push(format!(
                            "computed '{}' reads state key '{dep}' but nothing writes it",
                            c.key
                        ));
                    }
                }
            }
        }

        // Runtime coherence.
        if let Some(runtime) = &self.runtime {
            if runtime.include_thoughts == Some(true) && runtime.thinking_budget.is_none() {
                warnings.push(
                    "runtime.include_thoughts is set without runtime.thinking_budget — no \
                     thoughts will arrive"
                        .into(),
                );
            }
            if let Some(audio) = &runtime.audio {
                if audio.denoise == Some(true) && !cfg!(feature = "denoise") {
                    warnings.push(
                        "runtime.audio.denoise requires building with the `denoise` feature — \
                         the stage will be skipped"
                            .into(),
                    );
                }
                if audio.authority == Some(AuthoritySpec::Client) && audio.denoise != Some(true) {
                    warnings.push(
                        "runtime.audio.authority=client without denoise — in noise the raw \
                         energy VAD latches open and will drive interruptions falsely \
                         (measured); enable denoise or expect spurious barge-ins"
                            .into(),
                    );
                }
                if audio.noise_gate.is_some() && audio.denoise != Some(true) {
                    warnings.push(
                        "runtime.audio.noise_gate without denoise — the gate calibrates on \
                         noisy levels; chain it behind denoise so it gates clean audio"
                            .into(),
                    );
                }
                if audio.authority == Some(AuthoritySpec::Client) && runtime.vad.is_some() {
                    warnings.push(
                        "runtime.audio.authority=client disables the server's automatic \
                         activity detection — runtime.vad sensitivities will have no effect"
                            .into(),
                    );
                }
                if let Some(eot_ms) = audio.eot_hold_ms
                    && eot_ms > 1600
                {
                    warnings.push(
                        "runtime.audio.eot_hold_ms exceeds the measured frontier (1600 ms): \
                             recall fell to 0.508 at 1600ms on TurnBench dev — values beyond this \
                             may cause missed turn-end detection"
                            .into(),
                    );
                }
                if let Some(min_int_ms) = audio.min_interruption_ms
                    && min_int_ms > 2000
                {
                    warnings.push(
                        "runtime.audio.min_interruption_ms exceeds the interruption match \
                             window (2000 ms) — commits may land too late to count"
                            .into(),
                    );
                }
            }
            if runtime.session_id.is_some() && runtime.persistence.is_none() {
                warnings.push(
                    "runtime.session_id is set without runtime.persistence — nothing will be \
                     snapshotted"
                        .into(),
                );
            }
        }

        // Flow compilation, with declared tools as the registry. MCP tool
        // names are unknown until connect, so their presence downgrades the
        // unknown-tool check to a warning-free plain compile.
        let (valid_flow, referenced) = if has_flow {
            let compile_result = if self.tools.is_empty() || !self.mcp.is_empty() {
                if !self.mcp.is_empty() && !self.tools.is_empty() {
                    warnings.push(
                        "MCP toolsets resolve at connect time, so tool-name checking against \
                         the flow is skipped"
                            .into(),
                    );
                }
                flow.clone().compile()
            } else {
                let mut names = self.tool_names();
                if self.memory.is_some() {
                    // The memory binding installs its tools at connect; the
                    // flow may reference or gate them.
                    names.extend(
                        MEMORY_TOOL_NAMES
                            .iter()
                            .map(std::string::ToString::to_string),
                    );
                }
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                flow.clone().compile_with_tools(&refs)
            };
            match compile_result {
                Ok(compiled) => (
                    true,
                    compiled
                        .tool_policy()
                        .tools
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                Err(errs) => {
                    errors.extend(errs.0.iter().map(std::string::ToString::to_string));
                    (false, Vec::new())
                }
            }
        } else {
            (true, Vec::new())
        };

        if valid_flow && has_flow {
            // Read/write state-key diff — the guard-key analogue of the
            // unknown-tool check.
            let written = self.state_keys_written();
            for key in flow.state_keys_read() {
                if written.contains(&key) {
                    continue;
                }
                // `{name}:result` keys are written by on_enter orchestration.
                if key.ends_with(":result") {
                    continue;
                }
                let hint = written
                    .iter()
                    .filter(|w| levenshtein(&key, w) <= 2)
                    .cloned()
                    .collect::<Vec<_>>();
                let suffix = if hint.is_empty() {
                    String::new()
                } else {
                    format!(" — did you mean {}?", hint.join(" / "))
                };
                warnings.push(format!(
                    "a guard reads state key '{key}' but no tool, extractor, phase, or watcher \
                     writes it (it can never latch){suffix}"
                ));
            }
            for t in &self.tools {
                if !referenced.contains(&t.name) {
                    warnings.push(format!(
                        "tool '{}' is declared but no step or constraint references it \
                         (it will be denied whenever a step with an `allow` list is active \
                         unless you add it to `ambient`)",
                        t.name
                    ));
                }
            }
            for s in &flow.steps {
                if !s.terminal && s.posture.is_none() {
                    warnings.push(format!(
                        "step '{}' has no posture — the model gets no steering while it is active",
                        s.id
                    ));
                }
            }
        }

        SpecValidation {
            valid: errors.is_empty(),
            errors,
            warnings,
            mermaid,
            tools: referenced,
            steps,
        }
    }

    /// Build the dispatcher of declared tools bound to `state`.
    pub fn build_dispatcher(&self, state: &State) -> ToolDispatcher {
        let mut dispatcher = ToolDispatcher::new();
        for tool in &self.tools {
            dispatcher.register(build_tool(tool, state));
        }
        dispatcher
    }

    /// Apply the mock semantics of a declared tool to `state` (its
    /// `set_state` writes). Used by the offline simulator.
    pub(crate) fn apply_tool_state(&self, name: &str, state: &State) {
        if let Some(tool) = self.tools.iter().find(|t| t.name == name) {
            for (key, value) in &tool.set_state {
                let _ = state.set(key, value.clone());
            }
        }
    }

    /// Run the embedded test suite offline (no model, no network) — scripted
    /// events replayed through the real [`FlowMonitor`](gemini_adk_rs::flow::FlowMonitor).
    pub fn run_tests(&self) -> Vec<TestReport> {
        run_tests(self)
    }

    /// Configure a [`Live`] builder from this spec.
    ///
    /// `state` is the session state the declared tools bind to — pass the
    /// same one via `.with_state` (this method does). Returns an error when
    /// the spec fails validation or requires a resource `resources` lacks.
    /// Everything code-only (callbacks, custom guards, middleware) is added on
    /// the returned builder afterwards.
    pub fn apply(
        &self,
        live: Live,
        state: &State,
        resources: &SpecResources,
    ) -> Result<Live, String> {
        let validation = self.validate();
        if !validation.valid {
            return Err(format!(
                "spec failed validation: {}",
                validation.errors.join("; ")
            ));
        }
        if !self.extract.is_empty() && resources.extraction_llm.is_none() {
            return Err(
                "spec declares extraction but SpecResources.extraction_llm is not set".into(),
            );
        }
        if self.memory.is_some() && resources.memory.is_none() {
            return Err("spec declares memory but SpecResources.memory is not set".into());
        }

        // Seed declared defaults before anything reads state.
        self.seed_state_defaults(state);

        let mut live = live
            .with_state(state.clone())
            .instruction(if self.instruction.is_empty() {
                "Follow the conversation flow you are given.".to_string()
            } else {
                self.instruction.clone()
            });

        if let Some(greeting) = &self.greeting {
            live = live.greeting(greeting.clone());
        }
        live = match self.modality {
            SpecModality::Text => live.text_only(),
            SpecModality::Audio => live.voice(resolve_voice(self.voice.as_deref())),
        };

        // Tools: declared (mock/HTTP) via the dispatcher, MCP merged on top.
        if !self.tools.is_empty() {
            live = live.tools(self.build_dispatcher(state));
        }
        for params in &self.mcp {
            live = live.with_tools(T::mcp(params.clone()));
        }

        // Governance.
        let flow = self.effective_flow().map_err(|e| e.join("; "))?;
        if !flow.steps.is_empty() {
            live = live.govern(flow);
        }

        // Computed (derived) state variables — dependencies inferred from the
        // expression, so the registry's topological ordering holds.
        for c in &self.computed {
            let expr = c.from.clone();
            let deps: Vec<String> = expr.keys_read().into_iter().collect();
            let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
            live = live.computed(c.key.clone(), &dep_refs, move |s| expr.eval(s));
        }

        // Memory: install through the binding (tools, ingestion, slots).
        let memory_binding = resources.memory.clone();
        if let (Some(memory), Some(binding)) = (&self.memory, &memory_binding) {
            live = binding.install(live, memory);
        }

        // Background tools (async function calling).
        for tool in &self.tools {
            match (tool.background, tool.scheduling) {
                (_, Some(scheduling)) => {
                    live = live
                        .tool_background_with_scheduling(tool.name.clone(), scheduling.to_wire());
                }
                (true, None) => {
                    live = live.tool_background(tool.name.clone());
                }
                (false, None) => {}
            }
        }

        // Control-plane and voice tuning.
        if let Some(runtime) = &self.runtime {
            live = apply_runtime(live, runtime);
        }

        // Extraction.
        if let Some(llm) = &resources.extraction_llm {
            for e in &self.extract {
                let mut extractor =
                    LlmExtractor::new(e.name.clone(), llm.clone(), e.instruction.clone(), e.window)
                        .with_schema(e.schema.clone())
                        .with_min_words(3)
                        .with_trigger(e.trigger.to_trigger());
                if !e.promote.is_empty() {
                    extractor = extractor
                        .with_promotions(e.promote.iter().map(PromoteSpec::to_rule).collect());
                }
                live = live.extractor(Arc::new(extractor));
            }
        }

        // Phases.
        for p in &self.phases {
            let mut builder = live.phase(p.name.clone());
            if let Some(instruction) = &p.instruction {
                builder = builder.instruction(instruction.clone());
            }
            if !p.tools.is_empty() {
                builder = builder.tools(p.tools.clone());
            }
            if !p.needs.is_empty() {
                let refs: Vec<&str> = p.needs.iter().map(String::as_str).collect();
                builder = builder.needs(&refs);
            }
            if let Some(prompt) = p.prompt_on_enter {
                builder = builder.prompt_on_enter(prompt);
            }
            if p.terminal {
                builder = builder.terminal();
            }
            for t in &p.transitions {
                let guard = t.when.clone();
                let predicate = move |s: &State| guard.eval_state(s);
                builder = match &t.description {
                    Some(desc) => builder.transition_with(&t.to, predicate, desc.clone()),
                    None => builder.transition(&t.to, predicate),
                };
            }
            if !p.on_enter.is_empty() {
                let effects = p.on_enter.clone();
                let memory = memory_binding.clone();
                builder = builder.on_enter(move |state, writer| {
                    let effects = effects.clone();
                    let memory = memory.clone();
                    async move {
                        run_effects(&effects, &state, &writer, memory.as_ref()).await;
                    }
                });
            }
            live = builder.done();
        }
        if let Some(initial) = &self.initial_phase {
            live = live.initial_phase(initial.clone());
        }

        // Temporal patterns.
        for pattern in &self.patterns {
            let guard = pattern.when.clone();
            let condition = move |s: &State| guard.eval_state(s);
            let effects = pattern.effects.clone();
            let memory = memory_binding.clone();
            let action = move |state: State, writer: Arc<dyn SessionWriter>| {
                let effects = effects.clone();
                let memory = memory.clone();
                async move {
                    run_effects(&effects, &state, &writer, memory.as_ref()).await;
                }
            };
            if let Some(secs) = pattern.sustained_secs {
                live = live.when_sustained(
                    pattern.name.clone(),
                    condition,
                    std::time::Duration::from_secs(secs),
                    action,
                );
            } else if let Some(turns) = pattern.turns {
                live = live.when_turns(pattern.name.clone(), condition, turns, action);
            }
        }

        // Watchers.
        for w in &self.watch {
            let builder = live.watch(w.key.clone());
            let builder = match &w.condition {
                WatchCondition::Changed => builder.changed(),
                WatchCondition::ChangedTo(v) => builder.changed_to(v.clone()),
                WatchCondition::CrossedAbove(t) => builder.crossed_above(*t),
                WatchCondition::CrossedBelow(t) => builder.crossed_below(*t),
                WatchCondition::BecameTrue => builder.became_true(),
                WatchCondition::BecameFalse => builder.became_false(),
            };
            let sets = w.set.clone();
            let effects = w.effects.clone();
            let memory = memory_binding.clone();
            live = builder.then_with_writer(move |_old, _new, state, writer| {
                let sets = sets.clone();
                let effects = effects.clone();
                let memory = memory.clone();
                async move {
                    for (key, value) in &sets {
                        let _ = state.set(key, value.clone());
                    }
                    run_effects(&effects, &state, &writer, memory.as_ref()).await;
                }
            });
        }

        Ok(live)
    }
}

/// Execute a list of closed effects — the one executor behind phase
/// `on_enter`, watcher, and pattern actions, so every surface honors the
/// vocabulary identically.
async fn run_effects(
    effects: &[EffectSpec],
    state: &State,
    writer: &Arc<dyn SessionWriter>,
    memory: Option<&Arc<dyn MemoryBinding>>,
) {
    for effect in effects {
        match effect {
            EffectSpec::Set(map) => {
                for (key, value) in map {
                    let _ = state.set(key, value.clone());
                }
            }
            EffectSpec::Context(text) => {
                let _ = writer
                    .send_client_content(vec![Content::model(text.clone())], false)
                    .await;
            }
            EffectSpec::Prompt(text) => {
                let _ = writer
                    .send_client_content(vec![Content::model(text.clone())], true)
                    .await;
            }
            EffectSpec::Remember(template) => {
                if let Some(binding) = memory {
                    binding.remember(interpolate(template, &Value::Null, state));
                }
            }
        }
    }
}

/// Lower the turn-commit tuning knobs onto the Live builder. A knob left
/// unset keeps the default from
/// [`TurnCommitConfig::responsive()`](gemini_adk_rs::live::TurnCommitConfig::responsive).
fn apply_turn_commit(
    mut live: Live,
    eot_hold_ms: Option<u32>,
    min_interruption_ms: Option<u32>,
) -> Live {
    if let Some(ms) = eot_hold_ms {
        live = live.turn_commit_eot_hold_ms(u64::from(ms));
    }
    if let Some(ms) = min_interruption_ms {
        live = live.turn_commit_min_interruption_ms(u64::from(ms));
    }
    live
}

/// Lower the `runtime` section onto the builder.
fn apply_runtime(mut live: Live, runtime: &RuntimeSpec) -> Live {
    if let Some(t) = runtime.temperature {
        live = live.temperature(t);
    }
    if let Some(budget) = runtime.thinking_budget {
        live = live.thinking(budget);
    }
    if runtime.include_thoughts == Some(true) {
        live = live.include_thoughts();
    }
    if let Some(t) = runtime.transcription {
        live = live.transcription(t.input, t.output);
    }
    if let Some(enabled) = runtime.proactive_audio {
        live = live.proactive_audio(enabled);
    }
    if let Some(vad) = &runtime.vad {
        live = live.vad(AutomaticActivityDetection {
            disabled: None,
            start_of_speech_sensitivity: vad.start_sensitivity.map(SensitivitySpec::to_wire),
            end_of_speech_sensitivity: vad.end_sensitivity.map(SensitivitySpec::to_wire),
            prefix_padding_ms: vad.prefix_padding_ms,
            silence_duration_ms: vad.silence_duration_ms,
        });
    }
    if let Some(audio) = &runtime.audio {
        #[cfg(feature = "denoise")]
        if audio.denoise == Some(true) {
            live = live.mic_denoise();
        }
        if let Some(gate) = &audio.noise_gate {
            live = live.mic_noise_gate(gate.threshold_rms, gate.hold_frames);
        }
        if let Some(vad) = &audio.client_vad {
            live = live.input_vad(vad.to_config());
        }
        if audio.authority == Some(AuthoritySpec::Client) {
            live = live.client_interruption_authority();
        }
        // Turn-commit tuning knobs integration seam: if either field is set,
        // the Live builder's turn_commit(...) method will wire them through.
        live = apply_turn_commit(live, audio.eot_hold_ms, audio.min_interruption_ms);
    }
    if let Some(ms) = runtime.soft_turn_timeout_ms {
        live = live.soft_turn_timeout(std::time::Duration::from_millis(ms));
    }
    if let Some(steering) = runtime.steering {
        live = live.steering_mode(match steering {
            SteeringSpec::InstructionUpdate => SteeringMode::InstructionUpdate,
            SteeringSpec::ContextInjection => SteeringMode::ContextInjection,
            SteeringSpec::Hybrid => SteeringMode::Hybrid,
        });
    }
    if let Some(delivery) = runtime.context_delivery {
        live = live.context_delivery(match delivery {
            ContextDeliverySpec::Immediate => ContextDelivery::Immediate,
            ContextDeliverySpec::Deferred => ContextDelivery::Deferred,
        });
    }
    if let Some(repair) = runtime.repair {
        live = live.repair(RepairConfig {
            nudge_after: repair.nudge_after,
            escalate_after: repair.escalate_after,
        });
    }
    if let Some(persistence) = &runtime.persistence {
        live = match persistence {
            PersistenceSpec::Fs { dir } => {
                live.persistence(Arc::new(gemini_adk_rs::live::FsPersistence::new(dir)))
            }
            PersistenceSpec::Memory => {
                live.persistence(Arc::new(gemini_adk_rs::live::MemoryPersistence::new()))
            }
        };
    }
    if let Some(id) = &runtime.session_id {
        live = live.session_id(id.clone());
    }
    if runtime.lossy_audio {
        live = live.lossy_audio();
    }
    if runtime.lossy_transcript {
        live = live.lossy_transcript();
    }
    live
}

/// Build one declared tool as a [`SimpleTool`] bound to `state`.
fn build_tool(tool: &ToolSpec, state: &State) -> SimpleTool {
    let description = if tool.description.is_empty() {
        format!("Tool '{}'", tool.name)
    } else {
        tool.description.clone()
    };
    let response = tool.response.clone().unwrap_or_else(|| json!({"ok": true}));
    let sets = tool.set_state.clone();
    let save_as = tool.save_response_as.clone();
    let http = tool.http.clone();
    let st = state.clone();
    let name = tool.name.clone();
    SimpleTool::new(
        &tool.name,
        description,
        tool.parameters.clone(),
        move |args| {
            let response = response.clone();
            let sets = sets.clone();
            let save_as = save_as.clone();
            let http = http.clone();
            let st = st.clone();
            let name = name.clone();
            async move {
                let result = match &http {
                    Some(binding) => execute_http(binding, &args, &st).await.map_err(|e| {
                        gemini_adk_rs::error::ToolError::Other(format!("{name}: {e}"))
                    })?,
                    None => response,
                };
                for (key, value) in &sets {
                    let _ = st.set(key, value.clone());
                }
                if let Some(key) = &save_as {
                    let _ = st.set(key, result.clone());
                }
                Ok(result)
            }
        },
    )
}

/// Interpolate `{args.field}` and `{state.key}` templates in a string.
fn interpolate(template: &str, args: &Value, state: &State) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let expr = after[..close].trim();
        let value = if let Some(field) = expr.strip_prefix("args.") {
            args.get(field).cloned()
        } else if let Some(key) = expr.strip_prefix("state.") {
            state.get::<Value>(key)
        } else {
            None
        };
        match value {
            Some(Value::String(s)) => out.push_str(&s),
            Some(v) => out.push_str(&v.to_string()),
            None => {}
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Walk a JSON value, interpolating every string.
#[cfg_attr(not(feature = "http-tools"), allow(dead_code))]
fn interpolate_value(value: &Value, args: &Value, state: &State) -> Value {
    match value {
        Value::String(s) => Value::String(interpolate(s, args, state)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| interpolate_value(v, args, state))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), interpolate_value(v, args, state)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(feature = "http-tools")]
async fn execute_http(binding: &HttpBinding, args: &Value, state: &State) -> Result<Value, String> {
    let url = interpolate(&binding.url, args, state);
    let client = reqwest::Client::new();
    let method = reqwest::Method::from_bytes(binding.method.to_uppercase().as_bytes())
        .map_err(|_| format!("invalid HTTP method '{}'", binding.method))?;
    let mut request = client.request(method, &url);
    for (name, value) in &binding.headers {
        request = request.header(name, interpolate(value, args, state));
    }
    if let Some(body) = &binding.body {
        request = request.json(&interpolate_value(body, args, state));
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|e| e.to_string())?;
    Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({ "status": status, "body": text })))
}

#[cfg(not(feature = "http-tools"))]
async fn execute_http(
    _binding: &HttpBinding,
    _args: &Value,
    _state: &State,
) -> Result<Value, String> {
    Err("http tool bindings require the `http-tools` feature".to_string())
}

/// Splice `fragment` into `flow` under the directive's namespace.
fn splice_fragment(
    flow: &mut Flow,
    fragment: &Flow,
    directive: &UseFragment,
    errors: &mut Vec<String>,
) {
    let ns = &directive.namespace;
    let prefix = |id: &str| format!("{ns}/{id}");
    let internal: std::collections::BTreeSet<&str> =
        fragment.steps.iter().map(|s| s.id.as_str()).collect();

    for step in &fragment.steps {
        let new_id = prefix(&step.id);
        if flow.steps.iter().any(|s| s.id == new_id) {
            errors.push(format!(
                "fragment splice '{ns}' collides with existing step '{new_id}'"
            ));
            continue;
        }
        let mut after: Vec<gemini_adk_rs::flow::Edge> = step
            .after
            .iter()
            .map(|d| gemini_adk_rs::flow::Edge {
                step: if internal.contains(d.step.as_str()) {
                    prefix(&d.step)
                } else {
                    d.step.clone()
                },
                when: d
                    .when
                    .clone()
                    .map(|g| rewrite_guard_steps(g, &internal, ns)),
            })
            .collect();
        if step.after.is_empty() {
            after.extend(
                directive
                    .after
                    .iter()
                    .cloned()
                    .map(gemini_adk_rs::flow::Edge::to),
            );
        }
        flow.steps.push(Step {
            id: new_id,
            after,
            join: step.join,
            gate: step
                .gate
                .clone()
                .map(|g| rewrite_guard_steps(g, &internal, ns)),
            done: step
                .done
                .clone()
                .map(|g| rewrite_guard_steps(g, &internal, ns)),
            posture: step.posture.clone(),
            ground: step.ground.clone(),
            allow: step.allow.clone(),
            deny: step.deny.clone(),
            terminal: step.terminal,
        });
    }
    for constraint in &fragment.constraints {
        flow.constraints.push(match constraint {
            Constraint::Once(t) => Constraint::Once(t.clone()),
            Constraint::Before(a, b) => Constraint::Before(
                if internal.contains(a.as_str()) {
                    prefix(a)
                } else {
                    a.clone()
                },
                if internal.contains(b.as_str()) {
                    prefix(b)
                } else {
                    b.clone()
                },
            ),
            Constraint::NeverUntil { tool, until } => Constraint::NeverUntil {
                tool: tool.clone(),
                until: rewrite_guard_steps(until.clone(), &internal, ns),
            },
            Constraint::Require(rs) => Constraint::Require(
                rs.iter()
                    .map(|r| {
                        if internal.contains(r.as_str()) {
                            prefix(r)
                        } else {
                            r.clone()
                        }
                    })
                    .collect(),
            ),
            Constraint::Reset { steps, when } => Constraint::Reset {
                steps: steps
                    .iter()
                    .map(|r| {
                        if internal.contains(r.as_str()) {
                            prefix(r)
                        } else {
                            r.clone()
                        }
                    })
                    .collect(),
                when: rewrite_guard_steps(when.clone(), &internal, ns),
            },
        });
    }
    for tool in &fragment.ambient {
        if !flow.ambient.contains(tool) {
            flow.ambient.push(tool.clone());
        }
    }
    for tool in &fragment.confirm_tools {
        if !flow.confirm_tools.contains(tool) {
            flow.confirm_tools.push(tool.clone());
        }
    }
}

/// Rewrite `done(step)` atoms that reference fragment-internal steps.
fn rewrite_guard_steps(
    guard: Guard,
    internal: &std::collections::BTreeSet<&str>,
    ns: &str,
) -> Guard {
    fn rewrite(pred: Pred, internal: &std::collections::BTreeSet<&str>, ns: &str) -> Pred {
        match pred {
            Pred::Done(s) if internal.contains(s.as_str()) => Pred::Done(format!("{ns}/{s}")),
            Pred::All(ps) => Pred::All(ps.into_iter().map(|p| rewrite(p, internal, ns)).collect()),
            Pred::Any(ps) => Pred::Any(ps.into_iter().map(|p| rewrite(p, internal, ns)).collect()),
            Pred::Not(p) => Pred::Not(Box::new(rewrite(*p, internal, ns))),
            other => other,
        }
    }
    match guard {
        Guard::Spec(p) => Guard::Spec(rewrite(p, internal, ns)),
        custom => custom,
    }
}

/// Whether a guard contains `called_ok`/`done` atoms, which need a flow
/// marking and therefore cannot back phase transitions.
fn guard_uses_marking(guard: &Guard) -> bool {
    fn walk(pred: &Pred) -> bool {
        match pred {
            Pred::CalledOk(_) | Pred::Done(_) => true,
            Pred::All(ps) | Pred::Any(ps) => ps.iter().any(walk),
            Pred::Not(p) => walk(p),
            _ => false,
        }
    }
    match guard {
        Guard::Spec(p) => walk(p),
        Guard::Custom(_) => false,
    }
}

/// Resolve a voice name to the `Voice` enum.
fn resolve_voice(name: Option<&str>) -> Voice {
    match name {
        Some("Aoede") => Voice::Aoede,
        Some("Charon") => Voice::Charon,
        Some("Fenrir") => Voice::Fenrir,
        Some("Kore") => Voice::Kore,
        Some("Puck") | None => Voice::Puck,
        Some(other) => Voice::Custom(other.to_string()),
    }
}

/// Edit distance for the did-you-mean hint on unmatched state keys.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {

    #[test]
    fn audio_spec_lowers_and_validates() {
        let spec: SessionSpec = serde_json::from_str(
            r#"{
                "name": "noisy",
                "instruction": "hi",
                "runtime": {
                    "audio": {
                        "denoise": true,
                        "noise_gate": { "threshold_rms": 700.0 },
                        "client_vad": { "preset": "noisy_street", "hangover_frames": 12 },
                        "authority": "client"
                    }
                }
            }"#,
        )
        .unwrap();
        let audio = spec.runtime.as_ref().unwrap().audio.as_ref().unwrap();
        assert_eq!(audio.noise_gate.as_ref().unwrap().hold_frames, 3); // serde default
        let config = audio.client_vad.as_ref().unwrap().to_config();
        assert_eq!(config.start_threshold_db, 21.0); // preset
        assert_eq!(config.hangover_frames, 12); // override wins
        assert_eq!(audio.authority, Some(AuthoritySpec::Client));
        // Round-trips through JSON unchanged.
        let json = serde_json::to_value(&spec).unwrap();
        let back: SessionSpec = serde_json::from_value(json).unwrap();
        assert_eq!(
            back.runtime
                .unwrap()
                .audio
                .unwrap()
                .client_vad
                .unwrap()
                .hangover_frames,
            Some(12)
        );
    }

    #[test]
    fn audio_spec_warns_on_risky_combinations() {
        let spec: SessionSpec = serde_json::from_str(
            r#"{
                "name": "risky",
                "instruction": "hi",
                "runtime": { "audio": { "authority": "client", "noise_gate": {} } }
            }"#,
        )
        .unwrap();
        let validation = spec.validate();
        assert!(
            validation
                .warnings
                .iter()
                .any(|w| w.contains("authority=client without denoise")),
            "expected client-authority warning, got {:?}",
            validation.warnings
        );
        assert!(
            validation
                .warnings
                .iter()
                .any(|w| w.contains("noise_gate without denoise")),
            "expected gate warning, got {:?}",
            validation.warnings
        );
    }

    #[test]
    fn turn_commit_tuning_knobs_serialize_and_round_trip() {
        let spec: SessionSpec = serde_json::from_str(
            r#"{
                "name": "tune",
                "instruction": "hi",
                "runtime": {
                    "audio": {
                        "eot_hold_ms": 800,
                        "min_interruption_ms": 1400
                    }
                }
            }"#,
        )
        .unwrap();
        let audio = spec.runtime.as_ref().unwrap().audio.as_ref().unwrap();
        assert_eq!(audio.eot_hold_ms, Some(800));
        assert_eq!(audio.min_interruption_ms, Some(1400));
        // Round-trips through JSON unchanged.
        let json = serde_json::to_value(&spec).unwrap();
        let back: SessionSpec = serde_json::from_value(json).unwrap();
        let audio_back = back.runtime.unwrap().audio.unwrap();
        assert_eq!(audio_back.eot_hold_ms, Some(800));
        assert_eq!(audio_back.min_interruption_ms, Some(1400));
    }

    #[test]
    fn turn_commit_tuning_knobs_validate_thresholds() {
        // Warn on eot_hold_ms > 1600
        let spec: SessionSpec = serde_json::from_str(
            r#"{
                "name": "frontier",
                "instruction": "hi",
                "runtime": { "audio": { "eot_hold_ms": 1700 } }
            }"#,
        )
        .unwrap();
        let validation = spec.validate();
        assert!(
            validation
                .warnings
                .iter()
                .any(|w| w.contains("eot_hold_ms") && w.contains("1600") && w.contains("frontier")),
            "expected eot_hold_ms frontier warning, got {:?}",
            validation.warnings
        );

        // Warn on min_interruption_ms > 2000
        let spec2: SessionSpec = serde_json::from_str(
            r#"{
                "name": "window",
                "instruction": "hi",
                "runtime": { "audio": { "min_interruption_ms": 2100 } }
            }"#,
        )
        .unwrap();
        let validation2 = spec2.validate();
        assert!(
            validation2
                .warnings
                .iter()
                .any(|w| w.contains("min_interruption_ms")
                    && w.contains("2000")
                    && w.contains("window")),
            "expected min_interruption_ms window warning, got {:?}",
            validation2.warnings
        );
    }
    use super::*;

    fn collections_spec() -> SessionSpec {
        SessionSpec::from_value(json!({
            "name": "collections",
            "instruction": "Collect payments.",
            "tools": [
                {"name": "verify_identity", "set_state": {"identity_verified": true}},
                {"name": "charge_card", "response": {"charged": true}}
            ],
            "extract": [{
                "name": "ptp",
                "instruction": "Extract the promise to pay.",
                "schema": {"type": "object", "properties": {
                    "ptp_amount": {"type": "number"}, "ptp_date": {"type": "string"}}},
                "promote": [
                    {"field": "ptp_amount"},
                    {"field": "ptp_date", "policy": "overwrite"}
                ]
            }],
            "flow": {
                "steps": [
                    {"id": "verify", "posture": "Verify the caller.",
                     "allow": ["verify_identity"],
                     "done": {"is_true": "identity_verified"}},
                    {"id": "pay", "after": ["verify"], "posture": "Take payment.",
                     "allow": ["charge_card"],
                     "gate": {"captured": ["ptp_amount", "ptp_date"]},
                     "done": {"called_ok": "charge_card"}}
                ]
            }
        }))
        .expect("spec parses")
    }

    #[test]
    fn extraction_promotions_satisfy_guard_reads() {
        let v = collections_spec().validate();
        assert!(v.valid, "errors: {:?}", v.errors);
        assert!(
            v.warnings.iter().all(|w| !w.contains("can never latch")),
            "promoted keys cover the guard reads: {:?}",
            v.warnings
        );
    }

    #[test]
    fn unwritten_guard_key_warns_with_suggestion() {
        let mut spec = collections_spec();
        // Typo in the guard key relative to the tool's write.
        spec.flow.as_mut().unwrap().steps[0].done = Some(Guard::is_true("identity_verifed"));
        let v = spec.validate();
        assert!(v.valid);
        let warning = v
            .warnings
            .iter()
            .find(|w| w.contains("identity_verifed"))
            .expect("warns about the unwritten key");
        assert!(
            warning.contains("identity_verified"),
            "suggests the fix: {warning}"
        );
    }

    #[test]
    fn phase_guard_rejects_marking_atoms() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "phases": [{"name": "a", "transitions": [
                {"to": "b", "when": {"called_ok": "some_tool"}}]},
                {"name": "b"}],
            "initial_phase": "a"
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(!v.valid);
        assert!(v.errors.iter().any(|e| e.contains("called_ok")));
    }

    #[test]
    fn fragments_splice_with_namespacing() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "tools": [
                {"name": "check_id", "set_state": {"id_ok": true}},
                {"name": "book", "response": {}}
            ],
            "fragments": {
                "verify": {"steps": [
                    {"id": "ask", "posture": "Ask for ID.", "allow": ["check_id"],
                     "done": {"is_true": "id_ok"}},
                    {"id": "confirm", "after": ["ask"], "terminal": true,
                     "gate": {"done": "ask"}}
                ]}
            },
            "use_fragments": [{"fragment": "verify", "namespace": "v"}],
            "flow": {"steps": [
                {"id": "book_step", "after": ["v/confirm"], "allow": ["book"],
                 "done": {"called_ok": "book"}}
            ]}
        }))
        .expect("parses");
        let flow = spec.effective_flow().expect("splices");
        let ids: Vec<&str> = flow.steps.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"v/ask") && ids.contains(&"v/confirm"));
        // Internal done() atom rewritten to the namespaced id.
        let confirm = flow.steps.iter().find(|s| s.id == "v/confirm").unwrap();
        assert_eq!(
            serde_json::to_value(confirm.gate.as_ref().unwrap()).unwrap(),
            json!({"done": "v/ask"})
        );
        let v = spec.validate();
        assert!(v.valid, "errors: {:?}", v.errors);
    }

    #[test]
    fn bare_flow_round_trips() {
        let spec = SessionSpec::from_value(json!({
            "steps": [{"id": "only", "terminal": true}]
        }))
        .expect("parses");
        assert!(spec.validate().valid);
    }

    #[test]
    fn empty_fragment_namespace_is_rejected() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "fragments": {
                "verify": {"steps": [
                    {"id": "ask", "terminal": true}
                ]}
            },
            "use_fragments": [{"fragment": "verify", "namespace": ""}],
            "flow": {"steps": [
                {"id": "start", "terminal": true}
            ]}
        }))
        .expect("parses");
        let v = spec.validate();
        // Empty namespace should be a validation error
        assert!(
            !v.valid,
            "empty namespace should fail validation, got errors: {:?}",
            v.errors
        );
        assert!(
            v.errors.iter().any(|e| e.contains("namespace")),
            "should mention namespace issue: {:?}",
            v.errors
        );
    }

    #[test]
    fn duplicate_computed_keys_are_rejected() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "flow": {"steps": [{"id": "only", "terminal": true}]},
            "computed": [
                {"key": "risk", "from": {"key": "score"}},
                {"key": "risk", "from": {"key": "other_score"}}
            ]
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(!v.valid, "duplicate computed keys should fail validation");
        assert!(
            v.errors.iter().any(|e| e.contains("duplicate")),
            "should mention duplicate computed key: {:?}",
            v.errors
        );
    }

    #[test]
    fn duplicate_phase_names_are_rejected() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "phases": [
                {"name": "greet", "instruction": "Welcome."},
                {"name": "greet", "instruction": "Hi again."}
            ],
            "initial_phase": "greet"
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(!v.valid, "duplicate phase names should fail validation");
        assert!(
            v.errors.iter().any(|e| e.contains("phase")),
            "should mention duplicate phase: {:?}",
            v.errors
        );
    }

    #[test]
    fn tool_background_false_round_trips() {
        // Test that explicitly setting background: false round-trips correctly
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "tools": [
                {"name": "search", "background": false},
                {"name": "log", "background": true}
            ],
            "flow": {"steps": [{"id": "only", "terminal": true}]}
        }))
        .expect("parses");

        // After serialization
        let serialized = serde_json::to_value(&spec).unwrap();
        let tools = serialized["tools"].as_array().unwrap();

        // background: false is skipped in serialization (optimization), but
        // when deserialized, missing field defaults to false ✓
        assert!(
            tools[0].get("background").is_none(),
            "background: false is optimized away"
        );

        // background: true is serialized
        assert_eq!(tools[1].get("background"), Some(&json!(true)));

        // Round-trip test
        let back: SessionSpec = serde_json::from_value(serialized).unwrap();
        assert!(!back.tools[0].background);
        assert!(back.tools[1].background);
    }

    #[test]
    fn promotion_spec_with_no_to_field_uses_field_name() {
        // Test that promotion without explicit "to" uses the field name as target
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "tools": [{"name": "extract", "set_state": {"test": true}}],
            "extract": [{
                "name": "data",
                "instruction": "Extract.",
                "schema": {"type": "object"},
                "promote": [
                    {"field": "amount"},
                    {"field": "date", "to": "extracted_date"}
                ]
            }],
            "flow": {"steps": [{"id": "only", "terminal": true}]}
        }))
        .expect("parses");

        let promote = &spec.extract[0].promote;
        assert_eq!(promote[0].target(), "amount");
        assert_eq!(promote[1].target(), "extracted_date");
    }

    #[test]
    fn spec_schema_publishes() {
        let schema = SessionSpec::json_schema().to_string();
        for token in [
            "is_true",
            "never_until",
            "set_state",
            "use_fragments",
            "promote",
        ] {
            assert!(schema.contains(token), "schema missing {token}");
        }
    }

    #[test]
    fn interpolation_reads_args_and_state() {
        let state = State::new();
        let _ = state.set("city", "Paris");
        let args = json!({"guests": 4});
        assert_eq!(
            interpolate("book/{state.city}/{args.guests}/{missing}", &args, &state),
            "book/Paris/4/"
        );
    }

    #[tokio::test]
    async fn declared_tools_write_state() {
        let spec = collections_spec();
        let state = State::new();
        let dispatcher = spec.build_dispatcher(&state);
        let out = dispatcher
            .call_function("verify_identity", json!({}))
            .await
            .expect("tool runs");
        assert_eq!(out, json!({"ok": true}));
        assert_eq!(state.get::<bool>("identity_verified"), Some(true));
    }

    #[test]
    fn computed_cycles_are_load_time_errors() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "flow": {"steps": [{"id": "only", "terminal": true}]},
            "computed": [
                {"key": "a", "from": {"add": [{"key": "b"}, {"const": 1}]}},
                {"key": "b", "from": {"add": [{"key": "derived:a"}, {"const": 1}]}}
            ]
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(!v.valid);
        assert!(v.errors.iter().any(|e| e.contains("dependency cycle")));

        let self_read = SessionSpec::from_value(json!({
            "instruction": "x",
            "flow": {"steps": [{"id": "only", "terminal": true}]},
            "computed": [{"key": "a", "from": {"key": "a"}}]
        }))
        .expect("parses");
        assert!(
            self_read
                .validate()
                .errors
                .iter()
                .any(|e| e.contains("reads its own key"))
        );
    }

    #[test]
    fn computed_keys_satisfy_guard_reads_and_deps_are_checked() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "tools": [{"name": "record", "set_state": {"score": 0.8}}],
            "computed": [{"key": "high_risk",
                          "from": {"gt": [{"key": "score"}, {"const": 0.5}]}}],
            "flow": {"steps": [
                {"id": "assess", "posture": "Assess.", "allow": ["record"],
                 "done": {"is_true": "high_risk"}}
            ]}
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(v.valid, "errors: {:?}", v.errors);
        assert!(
            v.warnings.iter().all(|w| !w.contains("can never latch")),
            "computed key covers the guard read: {:?}",
            v.warnings
        );

        let mut dangling = spec.clone();
        dangling.computed[0].from =
            serde_json::from_value(json!({"gt": [{"key": "scoer"}, {"const": 0.5}]})).unwrap();
        let v = dangling.validate();
        assert!(
            v.warnings
                .iter()
                .any(|w| w.contains("computed 'high_risk' reads state key 'scoer'"))
        );
    }

    #[test]
    fn remember_requires_the_memory_section() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "flow": {"steps": [{"id": "only", "terminal": true}]},
            "patterns": [{"name": "note", "when": {"is_true": "flag"}, "turns": 2,
                          "effects": [{"remember": "caller likes {state.thing}"}]}]
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(!v.valid);
        assert!(v.errors.iter().any(|e| e.contains("no `memory` section")));
    }

    #[test]
    fn memory_slots_join_written_keys_and_reject_derived_targets() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "memory": {"slots": [{"predicate": "dietary_identity", "to": "user:diet"}]},
            "flow": {"steps": [
                {"id": "plan", "posture": "Plan dinner.",
                 "done": {"is_set": "user:diet"}}
            ]}
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(v.valid, "errors: {:?}", v.errors);
        assert!(v.warnings.iter().all(|w| !w.contains("can never latch")));

        let mut bad = spec.clone();
        bad.memory.as_mut().unwrap().slots[0].to = "derived:diet".into();
        assert!(
            bad.validate()
                .errors
                .iter()
                .any(|e| e.contains("read-only key"))
        );
    }

    #[test]
    fn memory_section_requires_a_binding_at_apply() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "memory": {},
            "flow": {"steps": [{"id": "only", "terminal": true}]}
        }))
        .expect("parses");
        let err = spec
            .apply(Live::builder(), &State::new(), &SpecResources::default())
            .err()
            .expect("memory binding required");
        assert!(err.contains("SpecResources.memory"));

        struct NullBinding;
        impl MemoryBinding for NullBinding {
            fn install(&self, live: Live, _memory: &MemorySpec) -> Live {
                live
            }
            fn remember(&self, _note: String) {}
        }
        let resources = SpecResources {
            memory: Some(Arc::new(NullBinding)),
            ..Default::default()
        };
        assert!(
            spec.apply(Live::builder(), &State::new(), &resources)
                .is_ok()
        );
    }

    #[test]
    fn state_dictionary_seeds_defaults_and_flags_undeclared_writes() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "state": {
                "attempts": {"type": "number", "default": 0,
                             "description": "Verification attempts so far."},
                "verified": {"type": "boolean", "default": "yes"}
            },
            "tools": [{"name": "verify", "set_state": {"verified": true, "vip": true}}],
            "flow": {"steps": [
                {"id": "v", "posture": "Verify.", "allow": ["verify"],
                 "done": {"is_true": "verified"}}
            ]}
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(v.valid, "errors: {:?}", v.errors);
        assert!(
            v.warnings
                .iter()
                .any(|w| w.contains("'verified' declares type Boolean")),
            "type-mismatched default warns: {:?}",
            v.warnings
        );
        assert!(
            v.warnings
                .iter()
                .any(|w| w.contains("'vip' is written but not declared")),
            "undeclared write warns: {:?}",
            v.warnings
        );

        let state = State::new();
        spec.seed_state_defaults(&state);
        assert_eq!(state.get::<i64>("attempts"), Some(0));
    }

    #[test]
    fn runtime_section_lowers_onto_the_builder() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "flow": {"steps": [{"id": "only", "terminal": true}]},
            "runtime": {
                "temperature": 0.4,
                "thinking_budget": 1024,
                "include_thoughts": true,
                "transcription": {"input": true, "output": false},
                "proactive_audio": true,
                "vad": {"start_sensitivity": "high", "silence_duration_ms": 400},
                "soft_turn_timeout_ms": 1500,
                "steering": "context_injection",
                "context_delivery": "deferred",
                "repair": {"nudge_after": 2, "escalate_after": 5},
                "persistence": "memory",
                "session_id": "user-1",
                "lossy_audio": true
            }
        }))
        .expect("parses");
        let v = spec.validate();
        assert!(v.valid, "errors: {:?}", v.errors);
        assert!(
            spec.apply(Live::builder(), &State::new(), &SpecResources::default())
                .is_ok()
        );

        let mut incoherent = spec.clone();
        incoherent.runtime.as_mut().unwrap().thinking_budget = None;
        assert!(
            incoherent
                .validate()
                .warnings
                .iter()
                .any(|w| w.contains("include_thoughts"))
        );
    }

    #[test]
    fn background_tools_and_scheduling_parse_and_apply() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "tools": [
                {"name": "search_kb", "background": true},
                {"name": "log_event", "scheduling": "silent"}
            ],
            "flow": {"steps": [
                {"id": "s", "posture": "Serve.", "allow": ["search_kb", "log_event"],
                 "done": {"called_ok": "search_kb"}}
            ]}
        }))
        .expect("parses");
        assert!(spec.validate().valid);
        assert!(
            spec.apply(Live::builder(), &State::new(), &SpecResources::default())
                .is_ok()
        );
    }

    #[test]
    fn apply_configures_a_builder() {
        let spec = collections_spec();
        let state = State::new();
        // No extraction LLM provided → apply must refuse (extraction declared).
        let err = spec
            .apply(Live::builder(), &state, &SpecResources::default())
            .err()
            .expect("requires extraction llm");
        assert!(err.contains("extraction_llm"));

        // Without the extraction entries it applies clean.
        let mut no_extract = spec.clone();
        no_extract.extract.clear();
        assert!(
            no_extract
                .apply(Live::builder(), &state, &SpecResources::default())
                .is_ok()
        );
    }
}
