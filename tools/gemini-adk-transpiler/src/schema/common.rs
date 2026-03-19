use serde::{Deserialize, Serialize};

/// Source metadata for any schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Framework identifier, e.g. "adk-js", "js-genai"
    pub framework: String,
    /// Path to the source directory that was scanned
    pub source_dir: String,
    /// ISO 8601 timestamp of when the extraction was performed
    pub extracted_at: String,
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
