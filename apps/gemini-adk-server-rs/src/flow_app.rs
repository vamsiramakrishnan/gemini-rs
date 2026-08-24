//! Data-driven flow applications — the backend of the Flow Studio.
//!
//! A [`FlowAppSpec`] is a single JSON document that turns a governed
//! [`Flow`] into a *runnable application*: the flow
//! DAG itself plus the session framing (instruction, greeting, modality) and a
//! set of declarative [mock tools](MockToolSpec) that let the conversation be
//! modeled end-to-end without writing any Rust. Each mock tool returns a canned
//! JSON response and can write `set_state` keys into the session [`State`], so
//! `is_true`/`captured` guards latch exactly as they would against real tools.
//!
//! The document is what the Flow Studio drag-and-drop editor reads and writes,
//! what `POST /api/flows/validate` checks, and what a `Start` message's
//! `config` field carries to spin up a live governed session.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use gemini_adk_rs::flow::Flow;
use gemini_adk_rs::state::State;
use gemini_adk_rs::tool::{SimpleTool, ToolDispatcher};

/// Output modality for a flow app session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowModality {
    /// Text-only session (the Flow Studio default — no microphone needed).
    #[default]
    Text,
    /// Audio (voice) session.
    Audio,
}

/// A declarative mock tool: name + schema + canned response + state writes.
///
/// Mock tools make a flow *executable as data*. When the model calls one, the
/// tool writes every `set_state` entry into the session [`State`] and returns
/// `response` (default `{"ok": true}`). Because flow guards read the same
/// state, `Guard::is_true(..)`/`captured(..)` conditions latch exactly as they
/// would with real tool implementations — swap in a real
/// [`ToolDispatcher`] later without touching the flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockToolSpec {
    /// Tool (function) name the model calls.
    pub name: String,
    /// Description shown to the model.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the arguments (Gemini subset). `None` = no parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    /// Canned JSON response returned to the model. Default `{"ok": true}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// State keys written when the tool runs — how a mock tool latches guards
    /// like `is_true("identity_verified")`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set_state: BTreeMap<String, Value>,
}

/// A complete, JSON-authorable flow application.
///
/// Everything a governed Live session needs, as one serializable value:
///
/// ```json
/// {
///   "name": "collections",
///   "instruction": "You are a debt-collection assistant.",
///   "greeting": "Greet the caller and ask for their name.",
///   "modality": "text",
///   "tools": [
///     {"name": "verify_identity", "set_state": {"identity_verified": true}}
///   ],
///   "flow": {
///     "steps": [
///       {"id": "verify", "posture": "Verify identity first.",
///        "allow": ["verify_identity"], "done": {"is_true": "identity_verified"}}
///     ]
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowAppSpec {
    /// App name (display only).
    #[serde(default)]
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Base system instruction for the session.
    #[serde(default)]
    pub instruction: String,
    /// Optional greeting prompt — makes the model speak first on connect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting: Option<String>,
    /// Output modality. Defaults to text.
    #[serde(default)]
    pub modality: FlowModality,
    /// Voice name for audio sessions (e.g. "Puck").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Declarative mock tools available to the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<MockToolSpec>,
    /// The governed flow DAG.
    pub flow: Flow,
}

/// Structured result of validating a [`FlowAppSpec`] — what the Studio UI
/// renders after `POST /api/flows/validate`.
#[derive(Debug, Clone, Serialize)]
pub struct FlowValidation {
    /// Whether the flow compiled cleanly.
    pub valid: bool,
    /// Compile/validation errors (empty when valid).
    pub errors: Vec<String>,
    /// Non-fatal advisories (e.g. a declared tool no step references).
    pub warnings: Vec<String>,
    /// Mermaid `flowchart` rendering of the DAG.
    pub mermaid: String,
    /// Every tool name the flow references.
    pub tools: Vec<String>,
    /// Number of steps in the flow.
    pub steps: usize,
}

impl FlowAppSpec {
    /// Parse a spec from a JSON value.
    ///
    /// Accepts either a full app document (`{"flow": {...}, ...}`) or a *bare
    /// flow* (`{"steps": [...]}`), which is wrapped in a default app — so the
    /// validate endpoint and the Studio can work directly with the flow JSON
    /// from the user guide.
    pub fn from_value(value: Value) -> Result<Self, String> {
        let is_bare_flow = value.get("flow").is_none() && value.get("steps").is_some();
        if is_bare_flow {
            let flow: Flow =
                serde_json::from_value(value).map_err(|e| format!("invalid flow JSON: {e}"))?;
            return Ok(Self {
                name: String::new(),
                description: String::new(),
                instruction: String::new(),
                greeting: None,
                modality: FlowModality::default(),
                voice: None,
                tools: Vec::new(),
                flow,
            });
        }
        serde_json::from_value(value).map_err(|e| format!("invalid flow app JSON: {e}"))
    }

    /// Declared mock tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name.clone()).collect()
    }

    /// Validate the spec: flow referential integrity + compilation, with the
    /// declared tools as the registry when any are declared.
    pub fn validate(&self) -> FlowValidation {
        let mermaid = self.flow.to_mermaid();
        let steps = self.flow.steps.len();

        let compile_result = if self.tools.is_empty() {
            self.flow.clone().compile()
        } else {
            let names = self.tool_names();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.flow.clone().compile_with_tools(&refs)
        };

        let (valid, errors, referenced) = match compile_result {
            Ok(compiled) => {
                let tools = compiled.tool_policy().tools.iter().cloned().collect();
                (true, Vec::new(), tools)
            }
            Err(errs) => {
                let msgs = errs.0.iter().map(|e| e.to_string()).collect();
                (false, msgs, Vec::<String>::new())
            }
        };

        let mut warnings = Vec::new();
        if valid {
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
            for s in &self.flow.steps {
                if !s.terminal && s.posture.is_none() {
                    warnings.push(format!(
                        "step '{}' has no posture — the model gets no steering while it is active",
                        s.id
                    ));
                }
            }
        }

        FlowValidation {
            valid,
            errors,
            warnings,
            mermaid,
            tools: referenced,
            steps,
        }
    }

    /// Build a [`ToolDispatcher`] of mock tools bound to `state`.
    ///
    /// Each tool writes its `set_state` entries into `state` and returns its
    /// canned `response`. Pass the same `State` to the Live builder via
    /// `.with_state(state)` so guards observe the writes.
    pub fn build_dispatcher(&self, state: &State) -> ToolDispatcher {
        let mut dispatcher = ToolDispatcher::new();
        for tool in &self.tools {
            let response = tool.response.clone().unwrap_or_else(|| json!({"ok": true}));
            let sets = tool.set_state.clone();
            let st = state.clone();
            let description = if tool.description.is_empty() {
                format!("Mock tool '{}'", tool.name)
            } else {
                tool.description.clone()
            };
            dispatcher.register(SimpleTool::new(
                &tool.name,
                description,
                tool.parameters.clone(),
                move |_args| {
                    let response = response.clone();
                    let sets = sets.clone();
                    let st = st.clone();
                    async move {
                        for (key, value) in &sets {
                            let _ = st.set(key, value.clone());
                        }
                        Ok(response)
                    }
                },
            ));
        }
        dispatcher
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_json() -> Value {
        json!({
            "name": "collections",
            "instruction": "You collect payments.",
            "tools": [
                {"name": "verify_identity", "set_state": {"identity_verified": true}},
                {"name": "charge_card", "response": {"charged": true}}
            ],
            "flow": {
                "steps": [
                    {"id": "verify", "posture": "Verify the caller.",
                     "allow": ["verify_identity"],
                     "done": {"is_true": "identity_verified"}},
                    {"id": "pay", "after": ["verify"], "posture": "Take payment.",
                     "allow": ["charge_card"],
                     "done": {"called_ok": "charge_card"}}
                ],
                "constraints": [
                    {"never_until": {"tool": "charge_card",
                                     "until": {"is_true": "identity_verified"}}}
                ]
            }
        })
    }

    #[test]
    fn parses_and_validates_full_spec() {
        let spec = FlowAppSpec::from_value(spec_json()).expect("parse");
        let v = spec.validate();
        assert!(v.valid, "errors: {:?}", v.errors);
        assert_eq!(v.steps, 2);
        assert!(v.tools.contains(&"charge_card".to_string()));
        assert!(v.mermaid.contains("verify --> pay"));
    }

    #[test]
    fn wraps_bare_flow_json() {
        let bare = json!({
            "steps": [{"id": "only", "terminal": true}]
        });
        let spec = FlowAppSpec::from_value(bare).expect("parse bare flow");
        assert!(spec.tools.is_empty());
        assert!(spec.validate().valid);
    }

    #[test]
    fn unknown_tool_reference_fails_compilation() {
        let mut value = spec_json();
        // Reference a tool that is not declared.
        value["flow"]["steps"][1]["allow"] = json!(["charge_card", "not_declared"]);
        let spec = FlowAppSpec::from_value(value).expect("parse");
        let v = spec.validate();
        assert!(!v.valid);
        assert!(v.errors.iter().any(|e| e.contains("not_declared")));
    }

    #[test]
    fn unused_declared_tool_warns() {
        let mut value = spec_json();
        value["tools"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "lonely_tool"}));
        let spec = FlowAppSpec::from_value(value).expect("parse");
        let v = spec.validate();
        assert!(v.valid);
        assert!(v.warnings.iter().any(|w| w.contains("lonely_tool")));
    }

    #[tokio::test]
    async fn mock_tools_write_state_and_return_response() {
        let spec = FlowAppSpec::from_value(spec_json()).expect("parse");
        let state = State::new();
        let dispatcher = spec.build_dispatcher(&state);

        let response = dispatcher
            .call_function("verify_identity", json!({}))
            .await
            .expect("mock tool call");
        assert_eq!(response, json!({"ok": true}));
        assert_eq!(state.get::<bool>("identity_verified"), Some(true));
    }
}
