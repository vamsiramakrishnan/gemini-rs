use serde::{Deserialize, Serialize};

use super::common::{FieldDef, SourceInfo};

/// Schema for the @google/genai SDK type surface.
/// Maps js-genai types to their gemini-live Rust equivalents.
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
    /// REST API module definitions
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rest_modules: Vec<RestModuleDef>,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
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
    /// generateContent / generateContentStream types
    Generate,
    /// embedContent types
    Embed,
    /// File upload/management types
    Files,
    /// Model listing/info types
    Models,
    /// Token counting types
    Tokens,
    /// Cached content types
    Caches,
    /// Fine-tuning types
    Tunings,
    /// Batch job types
    Batches,
    /// Chat session types (stateful wrapper over generate)
    Chats,
    /// Other types
    Other,
}

impl GenaiTypeCategory {
    /// Returns the Rust module name for this category.
    pub fn module_name(&self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::FunctionCalling => "function_calling",
            Self::LiveApi => "live",
            Self::Config => "config",
            Self::Metadata => "metadata",
            Self::Message => "messages",
            Self::Generate => "generate",
            Self::Embed => "embed",
            Self::Files => "files",
            Self::Models => "models",
            Self::Tokens => "tokens",
            Self::Caches => "caches",
            Self::Tunings => "tunings",
            Self::Batches => "batches",
            Self::Chats => "chats",
            Self::Other => "other",
        }
    }
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

// ── REST Module Schema ──────────────────────────────────────────────────────

/// HTTP method for REST operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

/// A single REST method extracted from a module class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestMethodDef {
    /// TypeScript method name (e.g., "get", "list", "create")
    pub ts_name: String,
    /// Rust method name (e.g., "get_file", "list_files")
    pub rust_name: String,
    /// HTTP method (GET, POST, PATCH, DELETE)
    pub http_method: HttpMethod,
    /// Return type name (e.g., "File", "ListFilesResponse")
    pub return_type: String,
    /// JSDoc description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether this is a special method needing manual implementation (upload/download)
    #[serde(default)]
    pub is_special: bool,
    /// Whether the method returns void/empty (delete, cancel)
    #[serde(default)]
    pub returns_void: bool,
}

/// REST API module extracted from a js-genai class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestModuleDef {
    /// Module name (e.g., "files", "caches")
    pub name: String,
    /// Class name in TypeScript (e.g., "Files", "Caches")
    pub class_name: String,
    /// ServiceEndpoint variant name (e.g., "Files", "CachedContents")
    pub service_endpoint: String,
    /// Extracted public methods
    pub methods: Vec<RestMethodDef>,
    /// Rust error enum name (e.g., "FilesError")
    pub error_type: String,
}

/// Combined schema for both js-genai and ADK-JS sources.
/// Used by the unified transpiler to generate comprehensive Rust code.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedSchema {
    /// js-genai SDK types (foundation layer → wire crate)
    pub genai: GenaiSchema,
    /// ADK-JS agent/tool types (framework layer → runtime crate)
    pub adk: super::adk::AdkSchema,
}
