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

// ---------------------------------------------------------------------------
// js-genai schema types
// ---------------------------------------------------------------------------

/// Schema for the @google/genai SDK type surface.
/// Maps js-genai types to their gemini-live-wire Rust equivalents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenaiSchema {
    /// Source information
    pub source: SourceInfo,
    /// Extracted type definitions (interfaces, classes)
    pub types: Vec<GenaiTypeDef>,
    /// Extracted enum definitions
    pub enums: Vec<GenaiEnumDef>,
    /// Type alias definitions (union types, etc.)
    pub type_aliases: Vec<GenaiTypeAlias>,
    /// Helper function signatures
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub helpers: Vec<GenaiHelperDef>,
}

/// A type from js-genai with its wire crate mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenaiTypeDef {
    /// Name of the type in js-genai (e.g. "Content", "Part", "FunctionCall")
    pub name: String,
    /// Category of the type
    pub category: GenaiTypeCategory,
    /// JSDoc description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fields (for interfaces/classes)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldDef>,
    /// Parent interface (extends)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// Wire crate equivalent (None = no direct equivalent, needs generated)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_type: Option<String>,
    /// Whether the wire crate already has this type
    pub has_wire_equivalent: bool,
}

/// Category of a js-genai type for codegen decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GenaiTypeCategory {
    /// Core content types: Content, Part, Blob, etc.
    Content,
    /// Function calling: FunctionCall, FunctionResponse, FunctionDeclaration, Tool
    FunctionCalling,
    /// Live API types: LiveConnectConfig, LiveServerContent, Session, etc.
    LiveApi,
    /// Configuration: GenerationConfig, SpeechConfig, etc.
    Config,
    /// Metadata: UsageMetadata, GroundingMetadata, etc.
    Metadata,
    /// Client/server messages: LiveClientMessage, LiveServerMessage, etc.
    Message,
    /// Other types
    Other,
}

/// An enum from js-genai.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenaiEnumDef {
    /// Name of the enum
    pub name: String,
    /// Variants
    pub variants: Vec<String>,
    /// JSDoc description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Wire crate equivalent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_type: Option<String>,
    /// Whether the wire crate already has this type
    pub has_wire_equivalent: bool,
}

/// A type alias from js-genai (e.g. `type ContentUnion = Content | Part[] | string`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenaiTypeAlias {
    /// Name of the type alias
    pub name: String,
    /// Original TypeScript definition
    pub ts_definition: String,
    /// Resolved Rust equivalent
    pub rust_type: String,
}

/// A helper function from js-genai (e.g. `createPartFromText`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenaiHelperDef {
    /// Function name
    pub name: String,
    /// TypeScript signature
    pub ts_signature: String,
    /// JSDoc description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Wire crate equivalent (e.g. "Part::text")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_equivalent: Option<String>,
}

/// Combined schema for both js-genai and ADK-JS sources.
/// Used by the unified transpiler to generate comprehensive Rust code.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedSchema {
    /// js-genai SDK types (foundation layer → wire crate)
    pub genai: GenaiSchema,
    /// ADK-JS agent/tool types (framework layer → runtime crate)
    pub adk: AdkSchema,
}
