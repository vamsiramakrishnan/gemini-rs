//! `code`: a tool through which the model runs a short Python program whose
//! functions are the spec's tools.
//!
//! ```json
//! "tools": [
//!   { "name": "lookup_payment_history",
//!     "returns": { "type": "object", "properties": { "payments": { "type": "array",
//!       "items": { "type": "object", "properties": {
//!         "paid": { "type": "string" }, "due": { "type": "string" }, "amount": { "type": "number" } } } } } } },
//!   { "name": "book_table" }
//! ],
//! "code": { "functions": ["lookup_payment_history"] }
//! ```
//!
//! Without `functions`, a program can call every declared tool except the
//! commits (a conversation stage's `commit`, a flow's `commit`) and the
//! background tools. Each call it makes is governed like a direct call.
//! Lowered to a [`CodeMode`]; programs run in
//! [`SpecResources::code_sandbox`](super::SpecResources::code_sandbox), or in
//! this process when it is unset.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use gemini_adk_rs::code_mode::{CodeLimits, CodeMode, CodeSandbox, DEFAULT_TOOL};
use gemini_adk_rs::flow::Constraint;

use super::{SessionSpec, ToolSpec};

/// The code tool of a spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodeSpec {
    /// The code tool's name. Default `run_code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The declared tools a program can call. Empty: every declared tool
    /// except commits and background tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<String>,
    /// Keep the functions declared as tools of their own too, so the model
    /// can make a single lookup without writing a program. Default `true`.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub direct: bool,
    /// Bounds on each program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<CodeLimitsSpec>,
}

impl Default for CodeSpec {
    fn default() -> Self {
        Self {
            tool: None,
            functions: Vec::new(),
            direct: true,
            limits: None,
        }
    }
}

fn yes() -> bool {
    true
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_yes(value: &bool) -> bool {
    *value
}

/// Bounds on each program; unset fields keep the defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodeLimitsSpec {
    /// Function calls per program. Default 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<usize>,
    /// Interpreter time per program, excluding its calls, in ms. Default 2000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,
    /// Wall-clock time per program, its calls included, in ms. Default 30000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    /// Characters of a program's result the model reads. Default 4000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_chars: Option<usize>,
    /// Memory per program in MiB, enforced by worker-process sandboxes.
    /// Default 64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<usize>,
}

impl CodeLimitsSpec {
    /// The runtime limits, defaults filled in.
    pub fn to_limits(&self) -> CodeLimits {
        let base = CodeLimits::default();
        CodeLimits {
            calls: self.calls.unwrap_or(base.calls),
            cpu: self.cpu_ms.map_or(base.cpu, Duration::from_millis),
            wall: self.wall_ms.map_or(base.wall, Duration::from_millis),
            output_chars: self.output_chars.unwrap_or(base.output_chars),
            memory: self.memory_mb.map_or(base.memory, |mb| mb * 1024 * 1024),
        }
    }
}

impl SessionSpec {
    /// The code tool's name, when the spec has one.
    pub fn code_tool(&self) -> Option<&str> {
        self.code
            .as_ref()
            .map(|code| code.tool.as_deref().unwrap_or(DEFAULT_TOOL))
    }

    /// The tools that commit: a conversation stage's `commit` (main flow and
    /// overlays) and a flow's `commit` steps.
    pub fn commit_tools(&self) -> BTreeSet<String> {
        let mut commits = BTreeSet::new();
        if let Some(conversation) = &self.conversation {
            let stages = conversation
                .stages
                .iter()
                .chain(conversation.overlays.iter().flat_map(|o| &o.stages));
            commits.extend(
                stages
                    .filter_map(|stage| stage.commit.as_ref())
                    .map(|commit| commit.tool.clone()),
            );
        } else if let Ok(flow) = self.effective_flow() {
            commits.extend(flow.constraints.iter().filter_map(|c| match c {
                Constraint::NeverUntil { tool, .. } => Some(tool.clone()),
                _ => None,
            }));
        }
        commits
    }

    /// The tools a program can call: `code.functions`, or every declared
    /// tool except commits and background tools.
    pub fn code_functions(&self) -> Vec<String> {
        let Some(code) = &self.code else {
            return Vec::new();
        };
        if !code.functions.is_empty() {
            return code.functions.clone();
        }
        let commits = self.commit_tools();
        self.tools
            .iter()
            .filter(|t| !commits.contains(&t.name) && !t.background && t.scheduling.is_none())
            .map(|t| t.name.clone())
            .collect()
    }

    /// The spec's code tool, running programs in `sandbox` (in this process
    /// when `None`). `None` when the spec has no `code`.
    pub fn compile_code_mode(&self, sandbox: Option<Arc<dyn CodeSandbox>>) -> Option<CodeMode> {
        let code = self.code.as_ref()?;
        let mut mode = CodeMode::new()
            .tool_name(self.code_tool().unwrap_or(DEFAULT_TOOL))
            .direct(code.direct)
            .limits(code.limits.clone().unwrap_or_default().to_limits());
        for name in self.code_functions() {
            mode = match self
                .tools
                .iter()
                .find(|t| t.name == name)
                .and_then(ToolSpec::returns_shape)
            {
                Some(shape) => mode.function_returning(name, shape),
                None => mode.function(name),
            };
        }
        if let Some(sandbox) = sandbox {
            mode = mode.sandbox(sandbox);
        }
        Some(mode)
    }

    /// Errors and warnings for `code`.
    pub(super) fn code_problems(&self) -> (Vec<String>, Vec<String>) {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let Some(code) = &self.code else {
            return (errors, warnings);
        };
        let tool = self.code_tool().unwrap_or(DEFAULT_TOOL);
        if !self.skills.is_empty() {
            errors
                .push("code: a session of skills cannot have a session-wide code tool".to_string());
        }
        if self.tools.iter().any(|t| t.name == tool) {
            errors.push(format!(
                "code: the code tool's name '{tool}' is also a declared tool"
            ));
        }
        for name in &code.functions {
            if !self.tools.iter().any(|t| &t.name == name) && self.mcp.is_empty() {
                errors.push(format!("code: function '{name}' is not a declared tool"));
            }
        }
        let functions = self.code_functions();
        if functions.is_empty() && self.mcp.is_empty() {
            warnings.push("code: no tool for a program to call".to_string());
        }
        let commits = self.commit_tools();
        for name in functions.iter().filter(|n| commits.contains(*n)) {
            warnings.push(format!(
                "code: '{name}' commits; a commit is clearer as one visible call than as a \
                 line in a program, so consider leaving it out of code.functions"
            ));
        }
        for name in &functions {
            if let Some(t) = self.tools.iter().find(|t| &t.name == name)
                && (t.background || t.scheduling.is_some())
            {
                warnings.push(format!(
                    "code: '{name}' is a background tool; in a program it runs inline, \
                     since the program needs its result"
                ));
            }
        }
        (errors, warnings)
    }
}

impl SessionSpec {
    /// A skill's definition with the skill's `code`, its functions filled in
    /// when unset: the skill's read tools that are not commits.
    pub(super) fn with_skill_code(mut self, skill: &super::SkillSpec) -> Self {
        let Some(mut code) = skill.code.clone() else {
            return self;
        };
        if code.functions.is_empty() {
            let commits = self.commit_tools();
            code.functions = skill
                .tools
                .iter()
                .filter(|t| matches!(t.effect, gemini_adk_rs::tasks::TaskToolEffect::Read))
                .map(|t| t.tool.name.clone())
                .filter(|name| !commits.contains(name))
                .collect();
        }
        self.code = Some(code);
        self
    }
}

impl ToolSpec {
    /// What the tool returns, as a program's docstring shows it: from
    /// `returns`, or the shape of the canned `response` of a mock tool.
    pub fn returns_shape(&self) -> Option<String> {
        match (&self.returns, &self.response) {
            (Some(schema), _) => Some(schema_shape(schema)),
            (None, Some(example)) if self.http.is_none() && self.mcp.is_none() => {
                Some(value_shape(example))
            }
            _ => None,
        }
    }
}

/// A JSON Schema as a compact shape: `{"payments": [{"amount": float}]}`.
pub fn schema_shape(schema: &Value) -> String {
    let kind = schema
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    match kind.as_deref() {
        Some("object") => match schema.get("properties").and_then(Value::as_object) {
            Some(properties) if !properties.is_empty() => format!(
                "{{{}}}",
                properties
                    .iter()
                    .map(|(k, v)| format!("\"{k}\": {}", schema_shape(v)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => "dict".to_string(),
        },
        Some("array") => match schema.get("items") {
            Some(items) => format!("[{}]", schema_shape(items)),
            None => "list".to_string(),
        },
        Some("string") => "str".to_string(),
        Some("integer") => "int".to_string(),
        Some("number") => "float".to_string(),
        Some("boolean") => "bool".to_string(),
        _ => "object".to_string(),
    }
}

/// The shape of an example value: `{"times": [str]}`.
pub fn value_shape(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Number(n) if n.is_i64() || n.is_u64() => "int".to_string(),
        Value::Number(_) => "float".to_string(),
        Value::String(_) => "str".to_string(),
        Value::Array(items) => match items.first() {
            Some(first) => format!("[{}]", value_shape(first)),
            None => "list".to_string(),
        },
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("\"{k}\": {}", value_shape(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(doc: Value) -> SessionSpec {
        serde_json::from_value(doc).expect("spec parses")
    }

    fn booking() -> Value {
        json!({
            "name": "booking",
            "tools": [
                { "name": "check_availability",
                  "response": { "date": "2026-10-12", "times": ["19:00", "20:30"] } },
                { "name": "lookup_payment_history",
                  "returns": { "type": "object", "properties": { "payments": {
                      "type": "array", "items": { "type": "object", "properties": {
                          "amount": { "type": "number" }, "paid": { "type": "string" } } } } } } },
                { "name": "book_table" },
                { "name": "notify", "background": true }
            ],
            "conversation": { "name": "book", "stages": [
                { "id": "confirm", "commit": { "tool": "book_table", "when": { "decided": "confirmed" } } }
            ] },
            "code": {}
        })
    }

    #[test]
    fn by_default_a_program_calls_every_tool_but_commits_and_background_tools() {
        let spec = spec(booking());
        assert_eq!(
            spec.code_functions(),
            ["check_availability", "lookup_payment_history"]
        );
        let mode = spec.compile_code_mode(None).expect("code mode");
        assert_eq!(mode.tool(), "run_code");
        let returns: Vec<Option<&str>> = mode
            .function_list()
            .iter()
            .map(|f| f.returns.as_deref())
            .collect();
        assert_eq!(
            returns,
            [
                Some(r#"{"date": str, "times": [str]}"#),
                Some(r#"{"payments": [{"amount": float, "paid": str}]}"#),
            ]
        );
    }

    #[test]
    fn listing_a_commit_warns_and_an_unknown_function_is_an_error() {
        let mut doc = booking();
        doc["code"] = json!({ "functions": ["book_table", "refund"] });
        let (errors, warnings) = spec(doc).code_problems();
        assert_eq!(errors, ["code: function 'refund' is not a declared tool"]);
        assert!(warnings[0].contains("'book_table' commits"), "{warnings:?}");
    }

    #[test]
    fn the_code_tool_cannot_take_a_declared_tool_name() {
        let mut doc = booking();
        doc["code"] = json!({ "tool": "book_table" });
        let (errors, _) = spec(doc).code_problems();
        assert!(errors[0].contains("'book_table' is also a declared tool"));
    }

    #[test]
    fn limits_fill_in_defaults() {
        let limits = CodeLimitsSpec {
            calls: Some(5),
            memory_mb: Some(16),
            ..Default::default()
        }
        .to_limits();
        assert_eq!(limits.calls, 5);
        assert_eq!(limits.memory, 16 * 1024 * 1024);
        assert_eq!(limits.cpu, CodeLimits::default().cpu);
    }
}
