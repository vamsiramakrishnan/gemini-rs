//! Generate a project around a [`SessionSpec`] in Rust, Python or Go.
//!
//! The spec stays the agent: model, instruction, conversation, tool
//! declarations and tests, as data in `agent.json`. A generated project adds
//! what data cannot carry, the tool implementations, as one typed function
//! per tool the spec declares as a mock. Each function returns the spec's
//! mock response until its body is replaced, so a fresh project behaves
//! exactly like the spec does offline.
//!
//! - **Rust** registers the functions in process with
//!   [`SpecResources::implement`](super::SpecResources::implement) and runs
//!   the session itself.
//! - **Python** and **Go** serve the functions as an MCP tool server over
//!   stdio. The project's `agent.json` points each of those tools at the
//!   server through its `mcp` binding, and any runtime that loads the spec
//!   (`adk spec run`, the runtime server) calls them there.
//!
//! In every language the declaration in `agent.json` is what the model sees,
//! and the spec's `set_state` and `save_response_as` still apply.

use std::fmt::Write as _;

use serde_json::Value;

use super::{SessionSpec, SpecModality, ToolSpec};

/// The language of a generated project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectLanguage {
    /// A Cargo binary that runs the session with in-process tools.
    Rust,
    /// An MCP tool server using the official `mcp` package.
    Python,
    /// An MCP tool server using the official Go SDK.
    Go,
}

impl std::str::FromStr for ProjectLanguage {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Ok(Self::Rust),
            "python" | "py" => Ok(Self::Python),
            "go" | "golang" => Ok(Self::Go),
            other => Err(format!(
                "unknown language '{other}': use rust, python or go"
            )),
        }
    }
}

/// Where a generated Rust project gets the SDK crates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SdkSource {
    /// crates.io, at this SDK's version.
    #[default]
    Registry,
    /// A local checkout of this repository, by path to its root.
    Path(String),
}

/// Options for [`SessionSpec::to_project_with`].
#[derive(Debug, Clone, Default)]
pub struct ProjectOptions {
    /// Where a Rust project gets the SDK crates.
    pub sdk: SdkSource,
}

/// One file of a generated project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectFile {
    /// Path relative to the project root, with `/` separators.
    pub path: String,
    /// File contents.
    pub contents: String,
}

impl ProjectFile {
    fn new(path: &str, contents: String) -> Self {
        Self {
            path: path.to_string(),
            contents,
        }
    }
}

/// The command a Python project's `agent.json` uses to start its server.
pub const PYTHON_TOOL_SERVER: &str = "python3 server.py";
/// The command a Go project's `agent.json` uses to start its server.
pub const GO_TOOL_SERVER: &str = "go run .";
/// The Go MCP SDK version generated projects require.
const GO_MCP_SDK: &str = "v1.8.0";

impl SessionSpec {
    /// A project in `language` that runs this spec. See the
    /// [module docs](self) for what each language generates.
    pub fn to_project(&self, language: ProjectLanguage) -> Vec<ProjectFile> {
        self.to_project_with(language, &ProjectOptions::default())
    }

    /// [`to_project`](Self::to_project) with options.
    pub fn to_project_with(
        &self,
        language: ProjectLanguage,
        options: &ProjectOptions,
    ) -> Vec<ProjectFile> {
        let tools = stubbed_tools(self);
        match language {
            ProjectLanguage::Rust => rust_project(self, &tools, options),
            ProjectLanguage::Python => python_project(self, &tools),
            ProjectLanguage::Go => go_project(self, &tools),
        }
    }
}

/// The tools a project implements: those with neither an HTTP nor an MCP
/// binding, i.e. the spec's mocks.
fn stubbed_tools(spec: &SessionSpec) -> Vec<&ToolSpec> {
    spec.tools
        .iter()
        .filter(|t| t.http.is_none() && t.mcp.is_none())
        .collect()
}

/// `agent.json` with each stubbed tool bound to `server`.
fn spec_bound_to(spec: &SessionSpec, server: Option<&str>) -> String {
    let mut spec = spec.clone();
    if let Some(server) = server {
        for tool in &mut spec.tools {
            if tool.http.is_none() && tool.mcp.is_none() {
                tool.mcp = Some(server.to_string());
            }
        }
    }
    let mut json = serde_json::to_string_pretty(&spec).unwrap_or_else(|_| "{}".into());
    json.push('\n');
    json
}

fn project_name(spec: &SessionSpec) -> String {
    let name: String = spec
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches('-').to_string();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("agent-{name}").trim_end_matches('-').to_string()
    } else {
        name
    }
}

fn mock_response(tool: &ToolSpec) -> Value {
    tool.response
        .clone()
        .unwrap_or_else(|| serde_json::json!({ "ok": true }))
}

// ── Parameters ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Int,
    Num,
    Bool,
    List,
    Object,
    Any,
}

struct Field {
    json: String,
    kind: Kind,
    required: bool,
    description: Option<String>,
    choices: Vec<String>,
}

/// The top-level parameters of a tool, required ones first.
fn fields(tool: &ToolSpec) -> Vec<Field> {
    let Some(schema) = &tool.parameters else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut fields: Vec<Field> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| {
            props
                .iter()
                .map(|(name, prop)| Field {
                    json: name.clone(),
                    kind: kind_of(prop),
                    required: required.contains(&name.as_str()),
                    description: prop
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    choices: prop
                        .get("enum")
                        .and_then(Value::as_array)
                        .map(|e| {
                            e.iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    fields.sort_by_key(|f| !f.required);
    fields
}

fn kind_of(prop: &Value) -> Kind {
    let ty = match prop.get("type") {
        Some(Value::String(t)) => t.as_str(),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or(""),
        _ => "",
    };
    match ty.to_ascii_lowercase().as_str() {
        "string" => Kind::Str,
        "integer" => Kind::Int,
        "number" => Kind::Num,
        "boolean" => Kind::Bool,
        "array" => Kind::List,
        "object" => Kind::Object,
        _ => Kind::Any,
    }
}

/// `snake_case` identifier characters from any name.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 && !out.ends_with('_') {
            out.push('_');
        }
        out.push(if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '_'
        });
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn pascal(name: &str) -> String {
    let mut out = String::new();
    for part in snake(name).split('_').filter(|p| !p.is_empty()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'T');
    }
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A JSON string literal, which is also a valid Rust, Python and Go string
/// literal for the characters JSON escapes.
fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

// ── Rust ────────────────────────────────────────────────────────────────────

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

fn rust_ident(name: &str) -> String {
    let ident = snake(name);
    if RUST_KEYWORDS.contains(&ident.as_str()) {
        format!("{ident}_")
    } else {
        ident
    }
}

fn rust_type(kind: Kind) -> &'static str {
    match kind {
        Kind::Str => "String",
        Kind::Int => "i64",
        Kind::Num => "f64",
        Kind::Bool => "bool",
        Kind::List => "Vec<Value>",
        Kind::Object | Kind::Any => "Value",
    }
}

fn rust_project(
    spec: &SessionSpec,
    tools: &[&ToolSpec],
    options: &ProjectOptions,
) -> Vec<ProjectFile> {
    vec![
        ProjectFile::new("Cargo.toml", rust_cargo_toml(spec, options)),
        ProjectFile::new("agent.json", spec_bound_to(spec, None)),
        ProjectFile::new("src/main.rs", rust_main(spec)),
        ProjectFile::new("src/tools.rs", rust_tools(tools)),
        ProjectFile::new("README.md", readme(spec, ProjectLanguage::Rust, tools)),
        ProjectFile::new(".gitignore", "/target\n".into()),
    ]
}

fn rust_cargo_toml(spec: &SessionSpec, options: &ProjectOptions) -> String {
    let mut features = vec!["gemini-llm"];
    if spec.modality == SpecModality::Audio {
        features.push("voice-io");
    }
    if spec.tools.iter().any(|t| t.http.is_some()) {
        features.push("http-tools");
    }
    let features = features
        .iter()
        .map(|f| quoted(f))
        .collect::<Vec<_>>()
        .join(", ");
    let source = |krate: &str| match &options.sdk {
        SdkSource::Registry => format!("version = \"{}\"", env!("CARGO_PKG_VERSION")),
        SdkSource::Path(root) => format!(
            "path = {}",
            quoted(&format!("{}/crates/{krate}", root.trim_end_matches('/')))
        ),
    };
    let mut out = format!(
        "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\ngemini-adk-fluent-rs = {{ {}, features = [{features}] }}\n",
        project_name(spec),
        source("gemini-adk-fluent-rs"),
    );
    if spec.memory.is_some() {
        let _ = writeln!(
            out,
            "gemini-memory-rs = {{ {} }}",
            source("gemini-memory-rs")
        );
    }
    out.push_str(
        "serde = { version = \"1\", features = [\"derive\"] }\nserde_json = \"1\"\n\
         tokio = { version = \"1\", features = [\"full\"] }\n\n\
         # A project of its own, even when generated inside another workspace.\n[workspace]\n",
    );
    out
}

fn rust_main(spec: &SessionSpec) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! {}: runs the agent in `agent.json`.",
        display_name(spec)
    );
    out.push_str(
        "//!\n//! `agent.json` is the agent: model, instruction, conversation, tool\n\
         //! declarations and tests. `src/tools.rs` implements its tools.\n\n\
         mod tools;\n\n",
    );
    if spec.extract.is_empty() && spec.memory.is_none() {
        out.push_str("use gemini_adk_fluent_rs::prelude::*;\n");
    } else {
        out.push_str("use std::sync::Arc;\n\nuse gemini_adk_fluent_rs::prelude::*;\n");
    }
    out.push_str(
        "use gemini_adk_fluent_rs::spec::SessionSpec;\n\n\
         /// The agent, as data. Compiled in, so the binary is self-contained.\n\
         const SPEC: &str = include_str!(\"../agent.json\");\n\n\
         fn spec() -> Result<SessionSpec, String> {\n    \
         SessionSpec::from_value(serde_json::from_str(SPEC).map_err(|e| e.to_string())?)\n}\n\n\
         #[tokio::main]\n\
         async fn main() -> Result<(), Box<dyn std::error::Error>> {\n    \
         let spec = spec()?;\n    \
         let state = State::new();\n",
    );
    let needs_more = !spec.extract.is_empty() || spec.memory.is_some();
    if needs_more {
        out.push_str("    let resources = gemini_adk_fluent_rs::spec::SpecResources {\n");
        if !spec.extract.is_empty() {
            out.push_str(
                "        // The out-of-band model behind the spec's `extract` entries.\n        \
                 extraction_llm: Some(Arc::new(GeminiLlm::from_env()?)),\n",
            );
        }
        if spec.memory.is_some() {
            out.push_str(
                "        // An in-memory engine; swap in a durable store for production.\n        \
                 memory: Some({\n            \
                 use gemini_memory_rs::prelude::{MemoryEngine, SessionId, UserId};\n            \
                 use gemini_memory_rs::runtime::SessionMemoryBinding;\n            \
                 let engine = MemoryEngine::in_memory(UserId::new(\"user\"));\n            \
                 let session = Arc::new(engine.begin_session(SessionId::new(\"session\")));\n            \
                 Arc::new(SessionMemoryBinding::new(session))\n        \
                 }),\n",
            );
        }
        out.push_str("        ..tools::resources()\n    };\n");
    } else {
        out.push_str("    let resources = tools::resources();\n");
    }
    out.push_str("    let session = spec\n        .apply(Live::builder(), &state, &resources)?\n");
    match spec.modality {
        SpecModality::Text => {
            out.push_str(
                "        .on_text(|t| print!(\"{t}\"))\n        \
                 .on_turn_complete(|| async { println!() })\n        \
                 .connect_from_env()\n        .await?;\n\n    \
                 // Type a line, read the reply.\n    \
                 let stdin = std::io::stdin();\n    \
                 let mut line = String::new();\n    \
                 while stdin.read_line(&mut line)? > 0 {\n        \
                 session.send_text(line.trim()).await?;\n        \
                 line.clear();\n    }\n    \
                 session.disconnect().await?;\n",
            );
        }
        SpecModality::Audio => {
            out.push_str(
                "        .connect_from_env()\n        .await?;\n\n    \
                 // Microphone in, speakers out, barge-in handled.\n    \
                 session.talk().await?;\n",
            );
        }
    }
    out.push_str(
        "    Ok(())\n}\n\n\
         #[cfg(test)]\nmod tests {\n    use super::*;\n\n    \
         /// The spec validates, and its embedded tests and scenarios pass.\n    \
         #[tokio::test]\n    \
         async fn the_spec_holds() {\n        \
         let spec = spec().unwrap();\n        \
         let validation = spec.validate();\n        \
         assert!(validation.valid, \"{:?}\", validation.errors);\n        \
         for report in spec.run_tests() {\n            \
         assert!(report.passed, \"test {}: {:?}\", report.name, report.failures);\n        }\n        \
         for report in spec.run_scenarios().await {\n            \
         assert!(\n                \
         report.passed,\n                \
         \"scenario {}: {:?}\",\n                \
         report.name, report.error\n            \
         );\n        }\n    }\n\n    \
         /// Every implementation belongs to a tool the spec declares.\n    \
         #[test]\n    \
         fn every_implementation_is_declared() {\n        \
         let spec = spec().unwrap();\n        \
         for name in tools::resources().tools.keys() {\n            \
         assert!(\n                \
         spec.tools.iter().any(|t| &t.name == name),\n                \
         \"{name} is not declared in agent.json\"\n            );\n        }\n    }\n}\n",
    );
    out
}

fn rust_tools(tools: &[&ToolSpec]) -> String {
    let mut out = String::from(
        "//! One function per tool `agent.json` declares as a mock.\n//!\n\
         //! Each returns the spec's mock response until you replace its body. The\n\
         //! declaration in `agent.json` stays what the model sees, and the spec's\n\
         //! `set_state` and `save_response_as` still apply to what you return.\n\n",
    );
    if tools.is_empty() {
        out.push_str(
            "use gemini_adk_fluent_rs::spec::SpecResources;\n\n\
             /// The implementations passed to `SessionSpec::apply`. The spec\n\
             /// declares no mock tools, so there are none.\n\
             pub fn resources() -> SpecResources {\n    SpecResources::default()\n}\n",
        );
        return out;
    }
    out.push_str(
        "use std::future::Future;\n\n\
         use gemini_adk_fluent_rs::prelude::*;\n\
         use gemini_adk_fluent_rs::spec::SpecResources;\n\
         use serde::Deserialize;\n\
         use serde::de::DeserializeOwned;\n\
         use serde_json::{Value, json};\n",
    );
    for tool in tools {
        let args = format!("{}Args", pascal(&tool.name));
        let fields = fields(tool);
        let _ = writeln!(out, "\n/// Arguments of `{}`.", tool.name);
        out.push_str("#[derive(Debug, Clone, Deserialize)]\n");
        if !fields.is_empty() {
            out.push_str("#[allow(dead_code, reason = \"read once the body is real\")]\n");
        }
        if fields.is_empty() {
            let _ = writeln!(out, "pub struct {args} {{}}\n");
        } else {
            let _ = writeln!(out, "pub struct {args} {{");
        }
        for field in &fields {
            if let Some(description) = &field.description {
                let _ = writeln!(out, "    /// {}", one_line(description));
            }
            if !field.choices.is_empty() {
                let _ = writeln!(out, "    /// One of: {}.", field.choices.join(", "));
            }
            let ident = rust_ident(&field.json);
            if ident != field.json {
                let _ = writeln!(out, "    #[serde(rename = {})]", quoted(&field.json));
            }
            let ty = rust_type(field.kind);
            if field.required {
                let _ = writeln!(out, "    pub {ident}: {ty},");
            } else {
                let _ = writeln!(out, "    #[serde(default)]\n    pub {ident}: Option<{ty}>,");
            }
        }
        if !fields.is_empty() {
            out.push_str("}\n\n");
        }
        if !tool.description.is_empty() {
            let _ = writeln!(out, "/// {}", one_line(&tool.description));
        }
        let ident = rust_ident(&tool.name);
        let signature =
            format!("pub async fn {ident}(args: {args}) -> Result<Value, ToolError> {{");
        let signature = if signature.len() > 100 {
            format!("pub async fn {ident}(\n    args: {args},\n) -> Result<Value, ToolError> {{")
        } else {
            signature
        };
        let _ = writeln!(
            out,
            "{signature}\n    \
             let _ = args;\n    \
             // The mock response from agent.json. Replace with the real call.\n    \
             Ok(json!({}))\n}}",
            mock_response(tool)
        );
    }
    out.push_str(
        "\n/// The implementations passed to `SessionSpec::apply`.\n\
         pub fn resources() -> SpecResources {\n    ",
    );
    let calls: Vec<String> = tools
        .iter()
        .map(|t| {
            format!(
                ".implement(typed({}, {}))",
                quoted(&t.name),
                rust_ident(&t.name)
            )
        })
        .collect();
    // rustfmt keeps a short chain on one line.
    let chain = format!("SpecResources::default(){}", calls.concat());
    if chain.len() <= 60 {
        out.push_str(&chain);
    } else {
        out.push_str("SpecResources::default()");
        for call in &calls {
            let _ = write!(out, "\n        {call}");
        }
    }
    out.push_str(
        "\n}\n\n\
         /// A tool that parses its arguments into `A` before calling `f`.\n\
         fn typed<A, F, Fut>(name: &str, f: F) -> SimpleTool\n\
         where\n    \
         A: DeserializeOwned,\n    \
         F: Fn(A) -> Fut + Send + Sync + 'static,\n    \
         Fut: Future<Output = Result<Value, ToolError>> + Send + 'static,\n\
         {\n    \
         SimpleTool::new(name, \"\", None, move |args| {\n        \
         let call = serde_json::from_value::<A>(args)\n            \
         .map(&f)\n            \
         .map_err(|e| ToolError::InvalidArgs(e.to_string()));\n        \
         async move { call?.await }\n    })\n}\n",
    );
    out
}

// ── Python ──────────────────────────────────────────────────────────────────

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

fn python_ident(name: &str) -> String {
    let ident = snake(name);
    if PYTHON_KEYWORDS.contains(&ident.as_str()) {
        format!("{ident}_")
    } else {
        ident
    }
}

fn python_type(field: &Field) -> String {
    if !field.choices.is_empty() && field.kind == Kind::Str {
        let choices: Vec<String> = field.choices.iter().map(|c| quoted(c)).collect();
        return format!("Literal[{}]", choices.join(", "));
    }
    match field.kind {
        Kind::Str => "str",
        Kind::Int => "int",
        Kind::Num => "float",
        Kind::Bool => "bool",
        Kind::List => "list[Any]",
        Kind::Object => "dict[str, Any]",
        Kind::Any => "Any",
    }
    .to_string()
}

/// A JSON value as a Python literal.
fn python_literal(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quoted(s),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(python_literal)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", quoted(k), python_literal(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn python_project(spec: &SessionSpec, tools: &[&ToolSpec]) -> Vec<ProjectFile> {
    let name = project_name(spec);
    vec![
        ProjectFile::new("agent.json", spec_bound_to(spec, Some(PYTHON_TOOL_SERVER))),
        ProjectFile::new("tools.py", python_tools(spec, tools)),
        ProjectFile::new("server.py", python_server(&name)),
        ProjectFile::new("test_tools.py", python_test()),
        ProjectFile::new(
            "pyproject.toml",
            format!(
                "[project]\nname = \"{name}-tools\"\nversion = \"0.1.0\"\n\
                 requires-python = \">=3.10\"\ndependencies = [\"mcp>=2.2,<3\"]\n\n\
                 [build-system]\nrequires = [\"setuptools>=69\"]\n\
                 build-backend = \"setuptools.build_meta\"\n\n\
                 [tool.setuptools]\npy-modules = [\"tools\", \"server\"]\n"
            ),
        ),
        ProjectFile::new("README.md", readme(spec, ProjectLanguage::Python, tools)),
        ProjectFile::new(".gitignore", "__pycache__/\n.venv/\n*.egg-info/\n".into()),
    ]
}

fn python_tools(spec: &SessionSpec, tools: &[&ToolSpec]) -> String {
    let mut body = String::new();
    let mut renamed = Vec::new();
    let mut uses_field = false;
    for tool in tools {
        let fields = fields(tool);
        let mut params = Vec::new();
        let mut names = Vec::new();
        for field in &fields {
            let ident = python_ident(&field.json);
            let mut ty = python_type(field);
            if ident != field.json {
                uses_field = true;
                names.push((field.json.clone(), ident.clone()));
                ty = format!("Annotated[{ty}, Field(alias={})]", quoted(&field.json));
            }
            if field.required {
                params.push(format!("{ident}: {ty}"));
            } else {
                params.push(format!("{ident}: {ty} | None = None"));
            }
        }
        if !names.is_empty() {
            renamed.push((tool.name.clone(), names));
        }
        let _ = write!(
            body,
            "\n\ndef {}({}) -> dict[str, Any]:\n",
            python_ident(&tool.name),
            params.join(", ")
        );
        let mut doc = one_line(&tool.description);
        if doc.is_empty() {
            doc = format!("The `{}` tool.", tool.name);
        }
        let _ = writeln!(body, "    {}", python_docstring(&doc));
        let response = mock_response(tool);
        let response = if response.is_object() {
            response
        } else {
            serde_json::json!({ "output": response })
        };
        let _ = writeln!(
            body,
            "    # The mock response from agent.json. Replace with the real call.\n    \
             return {}",
            python_literal(&response)
        );
    }

    let mut out = format!(
        "\"\"\"Tools for {}: one function per tool agent.json declares as a mock.\n\n\
         Each returns the spec's mock response until you replace its body. The\n\
         declaration in agent.json stays what the model sees, and the spec's\n\
         `set_state` and `save_response_as` still apply to what you return.\n\"\"\"\n\n",
        display_name(spec)
    );
    let mut typing = vec!["Any"];
    if uses_field {
        typing.insert(0, "Annotated");
    }
    if tools.iter().any(|t| {
        fields(t)
            .iter()
            .any(|f| !f.choices.is_empty() && f.kind == Kind::Str)
    }) {
        typing.push("Literal");
    }
    let _ = writeln!(out, "from typing import {}", typing.join(", "));
    if uses_field {
        out.push_str("\nfrom pydantic import Field\n");
    }
    out.push_str(&body);
    out.push_str("\n\n# Served by server.py, by the name agent.json declares.\nTOOLS = {\n");
    for tool in tools {
        let _ = writeln!(
            out,
            "    {}: {},",
            quoted(&tool.name),
            python_ident(&tool.name)
        );
    }
    out.push_str("}\n");
    out.push_str(
        "\n# Parameters whose names are not Python identifiers: the name the model\n\
         # sends, and the parameter it arrives as.\nRENAMED: dict[str, dict[str, str]] = {",
    );
    if renamed.is_empty() {
        out.push_str("}\n");
    } else {
        out.push('\n');
        for (tool, names) in renamed {
            let pairs: Vec<String> = names
                .iter()
                .map(|(json, ident)| format!("{}: {}", quoted(json), quoted(ident)))
                .collect();
            let _ = writeln!(out, "    {}: {{{}}},", quoted(&tool), pairs.join(", "));
        }
        out.push_str("}\n");
    }
    out
}

fn python_docstring(text: &str) -> String {
    format!(
        "\"\"\"{}\"\"\"",
        text.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"")
    )
}

fn python_server(name: &str) -> String {
    format!(
        "\"\"\"The MCP server agent.json points its tools at (stdio).\n\n\
         Run by the runtime as `{PYTHON_TOOL_SERVER}`. You don't need to edit this file.\n\"\"\"\n\n\
         import functools\n\n\
         from mcp.server.mcpserver import MCPServer\n\n\
         import tools\n\n\
         server = MCPServer({})\n\n\n\
         def _renamed(fn, names):\n    \
         \"\"\"Call `fn` with the parameters `names` maps, under their Python names.\"\"\"\n\n    \
         @functools.wraps(fn)\n    \
         def call(**kwargs):\n        \
         return fn(**{{names.get(k, k): v for k, v in kwargs.items()}})\n\n    \
         return call\n\n\n\
         for name, fn in tools.TOOLS.items():\n    \
         names = tools.RENAMED.get(name)\n    \
         server.add_tool(_renamed(fn, names) if names else fn, name=name)\n\n\
         if __name__ == \"__main__\":\n    server.run()\n",
        quoted(&format!("{name}-tools"))
    )
}

fn python_test() -> String {
    format!(
        "\"\"\"The server serves exactly the tools agent.json binds to it.\"\"\"\n\n\
         import asyncio\n\
         import json\n\
         import pathlib\n\
         import unittest\n\n\
         import server\n\n\
         SPEC = json.loads((pathlib.Path(__file__).parent / \"agent.json\").read_text())\n\n\n\
         class ServerMatchesSpec(unittest.TestCase):\n    \
         def test_serves_every_tool_bound_to_it(self):\n        \
         bound = {{t[\"name\"] for t in SPEC.get(\"tools\", []) if t.get(\"mcp\") == {}}}\n        \
         served = {{t.name for t in asyncio.run(server.server.list_tools())}}\n        \
         self.assertEqual(served, bound)\n\n\n\
         if __name__ == \"__main__\":\n    unittest.main()\n",
        quoted(PYTHON_TOOL_SERVER)
    )
}

// ── Go ──────────────────────────────────────────────────────────────────────

fn go_type(kind: Kind) -> &'static str {
    match kind {
        Kind::Str => "string",
        Kind::Int => "int64",
        Kind::Num => "float64",
        Kind::Bool => "bool",
        Kind::List => "[]any",
        Kind::Object => "map[string]any",
        Kind::Any => "any",
    }
}

/// A JSON value as a Go literal of type `any`.
fn go_literal(value: &Value) -> String {
    match value {
        Value::Null => "nil".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quoted(s),
        Value::Array(items) => format!(
            "[]any{{{}}}",
            items.iter().map(go_literal).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "map[string]any{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", quoted(k), go_literal(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn go_project(spec: &SessionSpec, tools: &[&ToolSpec]) -> Vec<ProjectFile> {
    let name = project_name(spec);
    vec![
        ProjectFile::new("agent.json", spec_bound_to(spec, Some(GO_TOOL_SERVER))),
        ProjectFile::new(
            "go.mod",
            format!(
                "module {name}-tools\n\ngo 1.25\n\n\
                 require github.com/modelcontextprotocol/go-sdk {GO_MCP_SDK}\n"
            ),
        ),
        ProjectFile::new("tools.go", go_tools(spec, tools)),
        ProjectFile::new("main.go", go_main(&name, tools)),
        ProjectFile::new("main_test.go", go_test()),
        ProjectFile::new("README.md", readme(spec, ProjectLanguage::Go, tools)),
        ProjectFile::new(".gitignore", format!("/{name}-tools\n")),
    ]
}

/// One struct field of a Go arguments type.
struct GoField {
    comments: Vec<String>,
    name: String,
    ty: &'static str,
    tag: String,
}

fn go_tools(spec: &SessionSpec, tools: &[&ToolSpec]) -> String {
    let mut out = format!(
        "// Tools for {}: one function per tool agent.json declares as a mock.\n//\n\
         // Each returns the spec's mock response until you replace its body. The\n\
         // declaration in agent.json stays what the model sees, and the spec's\n\
         // set_state and save_response_as still apply to what you return.\n\
         package main\n",
        display_name(spec)
    );
    if tools.is_empty() {
        return out;
    }
    out.push_str("\nimport \"context\"\n");
    for tool in tools {
        let fn_name = pascal(&tool.name);
        let _ = write!(
            out,
            "\n// {fn_name}Args are the arguments of {}.\ntype {fn_name}Args struct {{\n",
            tool.name
        );
        // As gofmt lays it out: names and types aligned within each run of
        // fields that no comment line interrupts.
        let mut runs: Vec<Vec<GoField>> = Vec::new();
        for field in fields(tool) {
            let mut comments = Vec::new();
            if let Some(description) = &field.description {
                comments.push(one_line(description));
            }
            if !field.choices.is_empty() {
                comments.push(format!("One of: {}.", field.choices.join(", ")));
            }
            let omit = if field.required { "" } else { ",omitempty" };
            let tag = format!("`json:{}`", quoted(&format!("{}{omit}", field.json)));
            if !comments.is_empty() || runs.is_empty() {
                runs.push(Vec::new());
            }
            if let Some(run) = runs.last_mut() {
                run.push(GoField {
                    comments,
                    name: pascal(&field.json),
                    ty: go_type(field.kind),
                    tag,
                });
            }
        }
        for run in &runs {
            let name_width = run.iter().map(|f| f.name.len()).max().unwrap_or(0);
            let type_width = run.iter().map(|f| f.ty.len()).max().unwrap_or(0);
            for GoField {
                comments,
                name,
                ty,
                tag,
            } in run
            {
                for comment in comments {
                    let _ = writeln!(out, "\t// {comment}");
                }
                let _ = writeln!(out, "\t{name:name_width$} {ty:type_width$} {tag}");
            }
        }
        out.push_str("}\n\n");
        let description = one_line(&tool.description);
        if description.is_empty() {
            let _ = writeln!(out, "// {fn_name} implements the {} tool.", tool.name);
        } else {
            let _ = writeln!(out, "// {fn_name}: {description}");
        }
        let response = mock_response(tool);
        let response = if response.is_object() {
            response
        } else {
            serde_json::json!({ "output": response })
        };
        let _ = writeln!(
            out,
            "func {fn_name}(ctx context.Context, args {fn_name}Args) (map[string]any, error) {{\n\t\
             // The mock response from agent.json. Replace with the real call.\n\t\
             return {}, nil\n}}",
            go_literal(&response)
        );
    }
    out
}

fn go_main(name: &str, tools: &[&ToolSpec]) -> String {
    let mut out = format!(
        "// The MCP server agent.json points its tools at (stdio). Run by the\n\
         // runtime as `{GO_TOOL_SERVER}`. You don't need to edit this file.\n\
         package main\n\n\
         import (\n\t\"context\"\n\t\"log\"\n\n\t\"github.com/modelcontextprotocol/go-sdk/mcp\"\n)\n\n\
         func main() {{\n\t\
         if err := newServer().Run(context.Background(), &mcp.StdioTransport{{}}); err != nil {{\n\t\t\
         log.Fatal(err)\n\t}}\n}}\n\n\
         // newServer serves each tool by the name agent.json declares.\n\
         func newServer() *mcp.Server {{\n\t\
         server := mcp.NewServer(&mcp.Implementation{{Name: {}, Version: \"v0.1.0\"}}, nil)\n",
        quoted(&format!("{name}-tools"))
    );
    for tool in tools {
        let _ = writeln!(
            out,
            "\tmcp.AddTool(server, &mcp.Tool{{Name: {}}}, serve({}))",
            quoted(&tool.name),
            pascal(&tool.name)
        );
    }
    out.push_str(
        "\treturn server\n}\n\n\
         // serve adapts a tool function to MCP; its result is the structured content.\n\
         func serve[In any](fn func(context.Context, In) (map[string]any, error)) mcp.ToolHandlerFor[In, map[string]any] {\n\t\
         return func(ctx context.Context, _ *mcp.CallToolRequest, in In) (*mcp.CallToolResult, map[string]any, error) {\n\t\t\
         out, err := fn(ctx, in)\n\t\t\
         return nil, out, err\n\t}\n}\n",
    );
    out
}

fn go_test() -> String {
    format!(
        "package main\n\n\
         import (\n\t\"context\"\n\t\"encoding/json\"\n\t\"os\"\n\t\"sort\"\n\t\"testing\"\n\n\t\
         \"github.com/modelcontextprotocol/go-sdk/mcp\"\n)\n\n\
         // The server serves exactly the tools agent.json binds to it.\n\
         func TestServesEveryToolBoundToIt(t *testing.T) {{\n\t\
         data, err := os.ReadFile(\"agent.json\")\n\t\
         if err != nil {{\n\t\tt.Fatal(err)\n\t}}\n\t\
         var spec struct {{\n\t\tTools []struct {{\n\t\t\tName string `json:\"name\"`\n\t\t\t\
         MCP  string `json:\"mcp\"`\n\t\t}} `json:\"tools\"`\n\t}}\n\t\
         if err := json.Unmarshal(data, &spec); err != nil {{\n\t\tt.Fatal(err)\n\t}}\n\t\
         var bound []string\n\t\
         for _, tool := range spec.Tools {{\n\t\t\
         if tool.MCP == {} {{\n\t\t\tbound = append(bound, tool.Name)\n\t\t}}\n\t}}\n\n\t\
         ctx := context.Background()\n\t\
         clientTransport, serverTransport := mcp.NewInMemoryTransports()\n\t\
         if _, err := newServer().Connect(ctx, serverTransport, nil); err != nil {{\n\t\tt.Fatal(err)\n\t}}\n\t\
         client := mcp.NewClient(&mcp.Implementation{{Name: \"test\", Version: \"v0.0.0\"}}, nil)\n\t\
         session, err := client.Connect(ctx, clientTransport, nil)\n\t\
         if err != nil {{\n\t\tt.Fatal(err)\n\t}}\n\t\
         defer session.Close()\n\t\
         listed, err := session.ListTools(ctx, nil)\n\t\
         if err != nil {{\n\t\tt.Fatal(err)\n\t}}\n\t\
         var served []string\n\t\
         for _, tool := range listed.Tools {{\n\t\tserved = append(served, tool.Name)\n\t}}\n\n\t\
         sort.Strings(bound)\n\t\
         sort.Strings(served)\n\t\
         if len(bound) != len(served) {{\n\t\t\
         t.Fatalf(\"agent.json binds %v, the server serves %v\", bound, served)\n\t}}\n\t\
         for i := range bound {{\n\t\t\
         if bound[i] != served[i] {{\n\t\t\t\
         t.Fatalf(\"agent.json binds %v, the server serves %v\", bound, served)\n\t\t}}\n\t}}\n}}\n",
        quoted(GO_TOOL_SERVER)
    )
}

// ── README ──────────────────────────────────────────────────────────────────

fn display_name(spec: &SessionSpec) -> String {
    if spec.name.is_empty() {
        "agent".into()
    } else {
        one_line(&spec.name)
    }
}

fn readme(spec: &SessionSpec, language: ProjectLanguage, tools: &[&ToolSpec]) -> String {
    let mut out = format!("# {}\n\n", display_name(spec));
    if !spec.description.is_empty() {
        let _ = writeln!(out, "{}\n", one_line(&spec.description));
    }
    out.push_str(
        "`agent.json` is the agent: model, instruction, conversation, tool\n\
         declarations and tests. Edit it in Flow Studio or by hand.\n\n",
    );
    let file = match language {
        ProjectLanguage::Rust => "`src/tools.rs`",
        ProjectLanguage::Python => "`tools.py`",
        ProjectLanguage::Go => "`tools.go`",
    };
    if tools.is_empty() {
        out.push_str("The spec declares no mock tools, so there is nothing to implement.\n\n");
    } else {
        let names: Vec<String> = tools.iter().map(|t| format!("`{}`", t.name)).collect();
        let _ = writeln!(
            out,
            "{file} has one function per tool the spec declares as a mock ({}). Each\n\
             returns the spec's mock response until you replace its body. What the\n\
             model sees stays the declaration in `agent.json`, and the spec's\n\
             `set_state` and `save_response_as` still apply to what you return.\n",
            names.join(", ")
        );
    }
    match language {
        ProjectLanguage::Rust => out.push_str(
            "```bash\ncargo test   # the spec validates; its tests and scenarios pass\n\
             cargo run    # a live session (GEMINI_API_KEY, or Vertex AI settings)\n```\n",
        ),
        ProjectLanguage::Python => {
            let _ = write!(
                out,
                "The tools run as an MCP server: `agent.json` binds each of them to\n\
                 `{PYTHON_TOOL_SERVER}`, which a runtime starts from this directory.\n\n\
                 ```bash\npython3 -m venv .venv && . .venv/bin/activate\npip install -e .\n\
                 python -m unittest              # the server serves what agent.json binds\n\
                 adk spec test agent.json        # the spec's tests and scenarios\n\
                 adk spec call agent.json <tool> '{{\"arg\": 1}}'   # one call, through the server\n\
                 adk spec run agent.json         # a live session\n```\n"
            );
        }
        ProjectLanguage::Go => {
            let _ = write!(
                out,
                "The tools run as an MCP server: `agent.json` binds each of them to\n\
                 `{GO_TOOL_SERVER}`, which a runtime starts from this directory. For\n\
                 production, build a binary and point the bindings at it.\n\n\
                 ```bash\ngo mod tidy\n\
                 go test ./...                   # the server serves what agent.json binds\n\
                 adk spec test agent.json        # the spec's tests and scenarios\n\
                 adk spec call agent.json <tool> '{{\"arg\": 1}}'   # one call, through the server\n\
                 adk spec run agent.json         # a live session\n```\n"
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> SessionSpec {
        SessionSpec::from_value(json!({
            "name": "Table Booking",
            "description": "Books tables.",
            "tools": [
                {
                    "name": "book_table",
                    "description": "Book a table.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "party_size": { "type": "integer", "description": "Guests." },
                            "from": { "type": "string" },
                            "seating": { "type": "string", "enum": ["inside", "terrace"] },
                            "notes": { "type": ["string", "null"] },
                            "type": { "type": "string" }
                        },
                        "required": ["party_size", "from"]
                    },
                    "response": { "confirmation": "MOCK-1", "held": true },
                    "set_state": { "booked": true }
                },
                { "name": "lookup", "http": { "url": "https://api.example.com/x" } },
                { "name": "ping", "response": "pong" }
            ],
            "flow": { "steps": [{ "id": "s", "allow": ["book_table", "lookup", "ping"], "terminal": true }] }
        }))
        .unwrap()
    }

    fn file<'a>(files: &'a [ProjectFile], path: &str) -> &'a str {
        &files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} missing"))
            .contents
    }

    #[test]
    fn a_rust_project_implements_the_mocks_in_process() {
        let files = spec().to_project(ProjectLanguage::Rust);
        let tools = file(&files, "src/tools.rs");
        assert!(tools.contains("pub struct BookTableArgs"));
        assert!(tools.contains("pub party_size: i64,"));
        assert!(tools.contains("pub from: String,"));
        assert!(tools.contains(
            "#[serde(rename = \"type\")]\n    #[serde(default)]\n    pub type_: Option<String>,"
        ));
        assert!(tools.contains("pub notes: Option<String>,"));
        assert!(tools.contains(r#"Ok(json!({"confirmation":"MOCK-1","held":true}))"#));
        assert!(tools.contains(".implement(typed(\"ping\", ping))"));
        // A tool with its own binding keeps it.
        assert!(!tools.contains("lookup"));
        let cargo = file(&files, "Cargo.toml");
        assert!(cargo.contains(&format!("version = \"{}\"", env!("CARGO_PKG_VERSION"))));
        assert!(cargo.contains("\"http-tools\""));
        assert!(cargo.contains("name = \"table-booking\""));

        let local = spec().to_project_with(
            ProjectLanguage::Rust,
            &ProjectOptions {
                sdk: SdkSource::Path("/src/gemini-rs/".into()),
            },
        );
        assert!(
            file(&local, "Cargo.toml")
                .contains("path = \"/src/gemini-rs/crates/gemini-adk-fluent-rs\"")
        );
        // agent.json is the spec unchanged.
        let agent: Value = serde_json::from_str(file(&files, "agent.json")).unwrap();
        assert!(agent["tools"][0].get("mcp").is_none());
    }

    #[test]
    fn a_python_project_serves_the_mocks_over_mcp() {
        let files = spec().to_project(ProjectLanguage::Python);
        let tools = file(&files, "tools.py");
        assert!(tools.contains(
            "def book_table(from_: Annotated[str, Field(alias=\"from\")], party_size: int, \
             notes: str | None = None, seating: Literal[\"inside\", \"terrace\"] | None = None, \
             type: str | None = None) -> dict[str, Any]:"
        ));
        assert!(tools.contains("return {\"confirmation\": \"MOCK-1\", \"held\": True}"));
        assert!(tools.contains("return {\"output\": \"pong\"}"));
        assert!(tools.contains("\"book_table\": {\"from\": \"from_\"},"));
        let agent: Value = serde_json::from_str(file(&files, "agent.json")).unwrap();
        assert_eq!(agent["tools"][0]["mcp"], PYTHON_TOOL_SERVER);
        assert!(
            agent["tools"][1].get("mcp").is_none(),
            "an HTTP tool keeps its binding"
        );
        // The bound spec still validates and keeps its mock for offline tests.
        let bound = SessionSpec::from_value(agent).unwrap();
        let validation = bound.validate();
        assert!(
            validation.errors.iter().all(|e| e.contains("http-tools")),
            "{:?}",
            validation.errors
        );
        assert!(bound.tools[0].response.is_some());
    }

    #[test]
    fn a_go_project_serves_the_mocks_over_mcp() {
        let files = spec().to_project(ProjectLanguage::Go);
        let tools = file(&files, "tools.go");
        // Aligned as gofmt aligns them: per run of fields between comments.
        assert!(tools.contains(
            "\t// Guests.\n\tPartySize int64  `json:\"party_size\"`\n\tNotes     string `json:\"notes,omitempty\"`\n"
        ));
        assert!(
            tools.contains(
                "return map[string]any{\"confirmation\": \"MOCK-1\", \"held\": true}, nil"
            )
        );
        let main = file(&files, "main.go");
        assert!(
            main.contains("mcp.AddTool(server, &mcp.Tool{Name: \"book_table\"}, serve(BookTable))")
        );
        let agent: Value = serde_json::from_str(file(&files, "agent.json")).unwrap();
        assert_eq!(agent["tools"][2]["mcp"], GO_TOOL_SERVER);
    }

    #[test]
    fn names_become_identifiers() {
        assert_eq!(snake("bookTable"), "book_table");
        assert_eq!(snake("book-table"), "book_table");
        assert_eq!(pascal("book_table"), "BookTable");
        assert_eq!(pascal("2fa"), "T2fa");
        assert_eq!(rust_ident("type"), "type_");
        assert_eq!(python_ident("class"), "class_");
        assert_eq!(project_name(&SessionSpec::default()), "agent");
        assert_eq!("PY".parse::<ProjectLanguage>(), Ok(ProjectLanguage::Python));
    }
}
