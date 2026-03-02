use serde::{Deserialize, Serialize};

/// Root schema document -- output of the reader, input to codegen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdkSchema {
    /// Source information
    pub source: SourceInfo,
    /// Extracted agent definitions
    pub agents: Vec<AgentDef>,
    /// Extracted tool definitions
    pub tools: Vec<ToolDef>,
    /// All other extracted type definitions (interfaces, enums, type aliases)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TypeDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Framework identifier, e.g. "adk-js"
    pub framework: String,
    /// Path to the source directory that was scanned
    pub source_dir: String,
    /// ISO 8601 timestamp of when the extraction was performed
    pub extracted_at: String,
}

/// Universal agent definition extracted from TypeScript source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDef {
    /// Name of the agent config interface (e.g. "LlmAgentConfig")
    pub name: String,
    /// Classification of the agent type
    pub kind: AgentKind,
    /// JSDoc description if present
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fields declared in the interface
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldDef>,
    /// Callback-typed fields
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callbacks: Vec<CallbackDef>,
    /// Parent interface this extends (e.g. "BaseAgentConfig")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
}

/// Classification of agent types found in the ADK-JS source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Base,
    Llm,
    Sequential,
    Parallel,
    Loop,
    Custom(String),
}

/// A single field from a TypeScript interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    /// Field name as it appears in TypeScript
    pub name: String,
    /// Original TypeScript type string
    pub ts_type: String,
    /// Mapped Rust type equivalent
    pub rust_type: String,
    /// Whether the field is optional (has `?:`)
    pub optional: bool,
    /// Default value if specified in JSDoc or code
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// JSDoc description if present
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A callback-typed field extracted from the interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallbackDef {
    /// Callback field name (e.g. "beforeAgentCallback")
    pub name: String,
    /// Original TypeScript type signature
    pub ts_signature: String,
    /// JSDoc description if present
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A tool definition extracted from TypeScript source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    /// Name of the tool interface/class (e.g. "BaseTool", "FunctionTool")
    pub name: String,
    /// JSDoc description if present
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fields declared in the tool's config/params interface
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldDef>,
    /// Parent interface/class this extends
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
}

/// A general type definition extracted from TypeScript source.
/// Captures interfaces, type aliases, and enums that aren't agents or tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDef {
    /// Name of the type
    pub name: String,
    /// Module/directory it was found in (e.g. "events", "sessions", "models")
    pub module: String,
    /// JSDoc description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fields (for interfaces)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldDef>,
    /// Parent interface (extends)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// Whether this is an enum
    #[serde(default)]
    pub is_enum: bool,
    /// Enum variants (if is_enum)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
}
