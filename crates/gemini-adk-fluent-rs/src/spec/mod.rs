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

mod simulate;

pub use simulate::{run_tests, SimEvent, SpecTest, TestExpectation, TestReport, TestStepResult};

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use gemini_adk_rs::flow::{Constraint, Flow, Guard, Pred, Step};
use gemini_adk_rs::live::extractor::{ExtractionTrigger, FieldPromotion, LlmExtractor};
use gemini_adk_rs::llm::BaseLlm;
use gemini_adk_rs::state::State;
use gemini_adk_rs::tool::{SimpleTool, ToolDispatcher};
use gemini_genai_rs::prelude::{Content, Voice};

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

/// A serializable side effect for phase entry and watcher actions — the
/// closed-effect counterpart to [`Guard`]'s closed predicates.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectSpec {
    /// Write these state keys.
    Set(BTreeMap<String, Value>),
    /// Inject a model-role context turn (steering text).
    Context(String),
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

/// A data-driven state watcher: when `key` satisfies the condition, apply the
/// effects (only [`EffectSpec::Set`] — watchers have no session writer).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchSpec {
    /// Watched state key.
    pub key: String,
    /// Trigger condition.
    pub condition: WatchCondition,
    /// State keys written when it fires.
    pub set: BTreeMap<String, Value>,
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
    /// Conversation phases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseSpec>,
    /// Initial phase name (required when `phases` is non-empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_phase: Option<String>,
    /// State watchers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<WatchSpec>,
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

/// External resources a spec cannot carry: model handles.
#[derive(Default)]
pub struct SpecResources {
    /// The OOB model backing `extract` entries. Required when any are present.
    pub extraction_llm: Option<Arc<dyn BaseLlm>>,
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
        }
        keys
    }

    /// Validate the whole document: flow compilation (with the declared tool
    /// registry), fragment splicing, phase-guard restrictions, HTTP-binding
    /// support, and the read/write state-key diff.
    pub fn validate(&self) -> SpecValidation {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();

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
        if let Some(initial) = &self.initial_phase {
            if !self.phases.iter().any(|p| &p.name == initial) {
                errors.push(format!("initial_phase '{initial}' is not a declared phase"));
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
                let names = self.tool_names();
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
                    errors.extend(errs.0.iter().map(|e| e.to_string()));
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
                builder = builder.on_enter(move |state, writer| {
                    let effects = effects.clone();
                    async move {
                        for effect in &effects {
                            match effect {
                                EffectSpec::Set(map) => {
                                    for (key, value) in map {
                                        let _ = state.set(key, value.clone());
                                    }
                                }
                                EffectSpec::Context(text) => {
                                    let _ = writer
                                        .send_client_content(
                                            vec![Content::model(text.clone())],
                                            false,
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                });
            }
            live = builder.done();
        }
        if let Some(initial) = &self.initial_phase {
            live = live.initial_phase(initial.clone());
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
            live = builder.then(move |_old, _new, state| {
                let sets = sets.clone();
                async move {
                    for (key, value) in &sets {
                        let _ = state.set(key, value.clone());
                    }
                }
            });
        }

        Ok(live)
    }
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
#[cfg_attr(not(any(feature = "http-tools", test)), allow(dead_code))]
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
        let mut after: Vec<String> = step
            .after
            .iter()
            .map(|d| {
                if internal.contains(d.as_str()) {
                    prefix(d)
                } else {
                    d.clone()
                }
            })
            .collect();
        if step.after.is_empty() {
            after.extend(directive.after.iter().cloned());
        }
        flow.steps.push(Step {
            id: new_id,
            after,
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
        assert!(no_extract
            .apply(Live::builder(), &state, &SpecResources::default())
            .is_ok());
    }
}
