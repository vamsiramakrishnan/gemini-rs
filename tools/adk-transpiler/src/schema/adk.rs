use serde::{Deserialize, Serialize};

use super::common::{CallbackDef, FieldDef, SourceInfo};

/// Root schema document for ADK-JS -- output of the reader, input to codegen.
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
