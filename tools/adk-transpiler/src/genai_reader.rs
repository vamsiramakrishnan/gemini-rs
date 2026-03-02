//! Reader for the @google/genai (js-genai) TypeScript SDK.
//!
//! Extracts all exported types, enums, type aliases, and helper functions
//! from the js-genai source tree. Maps each extracted type to its
//! rs-genai Rust equivalent where one exists.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use walkdir::WalkDir;

use crate::reader;
use crate::schema::{
    GenaiEnumDef, GenaiHelperDef, GenaiSchema, GenaiTypeAlias, GenaiTypeCategory, GenaiTypeDef,
    SourceInfo,
};

// ---------------------------------------------------------------------------
// Wire crate type mapping
// ---------------------------------------------------------------------------

/// Comprehensive mapping of js-genai type names → rs-genai Rust paths.
/// Built from comparing the js-genai exports against our wire crate's public API.
fn wire_type_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();

    // Content & Parts
    m.insert("Content", "rs_genai::prelude::Content");
    m.insert("Part", "rs_genai::prelude::Part");
    m.insert("Blob", "rs_genai::prelude::Blob");
    m.insert("FileData", "rs_genai::prelude::FileData");
    m.insert("ExecutableCode", "rs_genai::prelude::ExecutableCode");
    m.insert(
        "CodeExecutionResult",
        "rs_genai::prelude::CodeExecutionResult",
    );

    // Function calling
    m.insert("FunctionCall", "rs_genai::prelude::FunctionCall");
    m.insert(
        "FunctionResponse",
        "rs_genai::prelude::FunctionResponse",
    );
    m.insert(
        "FunctionDeclaration",
        "rs_genai::prelude::FunctionDeclaration",
    );
    m.insert("Tool", "rs_genai::prelude::Tool");
    m.insert("ToolConfig", "rs_genai::prelude::ToolConfig");
    m.insert(
        "FunctionCallingConfig",
        "rs_genai::prelude::FunctionCallingConfig",
    );

    // Enums
    m.insert("Modality", "rs_genai::prelude::Modality");
    m.insert("Role", "rs_genai::prelude::Role");

    // Configuration
    m.insert(
        "GenerationConfig",
        "rs_genai::prelude::GenerationConfig",
    );
    m.insert("SpeechConfig", "rs_genai::prelude::SpeechConfig");
    m.insert("VoiceConfig", "rs_genai::prelude::VoiceConfig");
    m.insert(
        "PrebuiltVoiceConfig",
        "rs_genai::prelude::PrebuiltVoiceConfig",
    );
    m.insert("ThinkingConfig", "rs_genai::prelude::ThinkingConfig");
    m.insert(
        "RealtimeInputConfig",
        "rs_genai::prelude::RealtimeInputConfig",
    );
    m.insert(
        "AutomaticActivityDetection",
        "rs_genai::prelude::AutomaticActivityDetection",
    );
    m.insert(
        "SessionResumptionConfig",
        "rs_genai::prelude::SessionResumptionConfig",
    );
    m.insert(
        "ContextWindowCompressionConfig",
        "rs_genai::prelude::ContextWindowCompressionConfig",
    );
    m.insert(
        "SlidingWindow",
        "rs_genai::prelude::SlidingWindow",
    );
    m.insert(
        "ProactivityConfig",
        "rs_genai::prelude::ProactivityConfig",
    );
    m.insert(
        "InputAudioTranscription",
        "rs_genai::prelude::InputAudioTranscription",
    );
    m.insert(
        "OutputAudioTranscription",
        "rs_genai::prelude::OutputAudioTranscription",
    );

    // Metadata
    m.insert("UsageMetadata", "rs_genai::prelude::UsageMetadata");
    m.insert(
        "GroundingMetadata",
        "rs_genai::prelude::GroundingMetadata",
    );
    m.insert(
        "UrlContextMetadata",
        "rs_genai::prelude::UrlContextMetadata",
    );

    // Session/Live API messages (mapped to wire message types)
    m.insert(
        "LiveServerMessage",
        "rs_genai::prelude::ServerMessage",
    );
    m.insert(
        "LiveServerSetupComplete",
        "rs_genai::prelude::SetupCompletePayload",
    );
    m.insert(
        "LiveServerContent",
        "rs_genai::prelude::ServerContentPayload",
    );
    m.insert(
        "LiveServerToolCall",
        "rs_genai::prelude::ToolCallPayload",
    );
    m.insert(
        "LiveServerToolCallCancellation",
        "rs_genai::prelude::ToolCallCancellationPayload",
    );
    m.insert(
        "LiveServerGoAway",
        "rs_genai::prelude::GoAwayPayload",
    );
    m.insert(
        "LiveServerSessionResumptionUpdate",
        "rs_genai::prelude::SessionResumptionUpdatePayload",
    );
    m.insert(
        "LiveClientContent",
        "rs_genai::prelude::ClientContentPayload",
    );
    m.insert(
        "LiveClientRealtimeInput",
        "rs_genai::prelude::RealtimeInputPayload",
    );
    m.insert(
        "LiveClientToolResponse",
        "rs_genai::prelude::ToolResponsePayload",
    );
    m.insert(
        "LiveClientSetup",
        "rs_genai::prelude::SetupPayload",
    );
    m.insert(
        "ActivityStart",
        "rs_genai::prelude::ActivityStart",
    );
    m.insert("ActivityEnd", "rs_genai::prelude::ActivityEnd");

    // Session abstraction
    m.insert("Session", "rs_genai::prelude::SessionHandle");

    // Transcription
    m.insert(
        "Transcription",
        "rs_genai::prelude::TranscriptionPayload",
    );

    m
}

/// Map of js-genai helper functions → wire crate equivalents.
fn helper_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("createPartFromText", "Part::text");
    m.insert("createPartFromBase64", "Part::inline_data");
    m.insert("createPartFromFunctionCall", "Part::function_call");
    m.insert("createUserContent", "Content::user");
    m.insert("createModelContent", "Content::model");
    m.insert(
        "createPartFromFunctionResponse",
        "Content::function_response",
    );
    m
}

/// Map of js-genai enum names → wire crate equivalents.
fn enum_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("Modality", "rs_genai::prelude::Modality");
    m.insert("MediaResolution", "rs_genai::prelude::MediaResolution");
    m.insert("Type", "rs_genai::prelude::SchemaType"); // JSON Schema Type enum
    m
}

// ---------------------------------------------------------------------------
// Category classification
// ---------------------------------------------------------------------------

fn classify_genai_type(name: &str) -> GenaiTypeCategory {
    match name {
        // Content types
        "Content" | "Part" | "Blob" | "FileData" | "VideoMetadata" => GenaiTypeCategory::Content,

        // Function calling
        n if n.contains("Function") || n == "Tool" || n == "ToolConfig" || n == "Schema" => {
            GenaiTypeCategory::FunctionCalling
        }

        // Live API
        n if n.starts_with("Live") || n == "Session" || n == "Transcription" => {
            GenaiTypeCategory::LiveApi
        }

        // Configuration
        n if n.contains("Config") || n.contains("Speech") || n.contains("Voice") => {
            GenaiTypeCategory::Config
        }

        // Metadata
        n if n.contains("Metadata") || n.contains("Usage") || n.contains("Grounding") => {
            GenaiTypeCategory::Metadata
        }

        // Messages
        n if n.contains("Message") || n.contains("Activity") => GenaiTypeCategory::Message,

        _ => GenaiTypeCategory::Other,
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Read all TypeScript files from a js-genai source tree and extract
/// type definitions into a GenaiSchema with wire crate mappings.
pub fn read_genai_source(source_dir: &Path) -> Result<GenaiSchema, String> {
    let source_dir_str = source_dir
        .to_str()
        .ok_or_else(|| "Invalid UTF-8 in source path".to_string())?
        .to_string();

    let wire_types = wire_type_map();
    let helpers = helper_map();
    let enums_map = enum_map();

    let mut types = Vec::new();
    let mut enums = Vec::new();
    let mut type_aliases = Vec::new();
    let mut helper_defs = Vec::new();

    // Collect all .ts files
    let ts_files: Vec<_> = WalkDir::new(source_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let p = e.path();
            p.extension().is_some_and(|ext| ext == "ts")
                && !p
                    .file_name()
                    .unwrap_or_default()
                    .to_str()
                    .unwrap_or("")
                    .ends_with(".d.ts")
        })
        .collect();

    for entry in &ts_files {
        let path = entry.path();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

        // Extract interfaces
        let interfaces = reader::extract_interfaces_pub(&content);
        for iface in &interfaces {
            let has_wire = wire_types.contains_key(iface.name.as_str());
            let wire_type = wire_types.get(iface.name.as_str()).map(|s| s.to_string());

            let (fields, _callbacks) = reader::parse_fields_pub(&iface.body);

            types.push(GenaiTypeDef {
                name: iface.name.clone(),
                category: classify_genai_type(&iface.name),
                description: iface.jsdoc.clone(),
                fields,
                extends: iface.extends.clone(),
                wire_type,
                has_wire_equivalent: has_wire,
            });
        }

        // Extract enums
        let extracted_enums = reader::extract_enums_pub(&content);
        for (name, jsdoc, variants) in &extracted_enums {
            let has_wire = enums_map.contains_key(name.as_str());
            let wire_type = enums_map.get(name.as_str()).map(|s| s.to_string());

            enums.push(GenaiEnumDef {
                name: name.clone(),
                variants: variants.clone(),
                description: jsdoc.clone(),
                wire_type,
                has_wire_equivalent: has_wire,
            });
        }

        // Extract type aliases
        let aliases = reader::extract_type_aliases_pub(&content);
        for (name, ts_def) in &aliases {
            let rust_type = map_genai_alias_to_rust(ts_def, &wire_types);
            type_aliases.push(GenaiTypeAlias {
                name: name.clone(),
                ts_definition: ts_def.clone(),
                rust_type,
            });
        }

        // Extract helper functions (export function createPartFromText, etc.)
        let extracted_helpers = extract_helper_functions(&content);
        for (name, signature) in &extracted_helpers {
            let wire_equiv = helpers.get(name.as_str()).map(|s| s.to_string());
            helper_defs.push(GenaiHelperDef {
                name: name.clone(),
                ts_signature: signature.clone(),
                description: None,
                wire_equivalent: wire_equiv,
            });
        }
    }

    // Deduplicate by name
    dedup_by_name(&mut types);
    dedup_enums_by_name(&mut enums);
    dedup_aliases_by_name(&mut type_aliases);
    dedup_helpers_by_name(&mut helper_defs);

    // Sort for deterministic output
    types.sort_by(|a, b| a.name.cmp(&b.name));
    enums.sort_by(|a, b| a.name.cmp(&b.name));
    type_aliases.sort_by(|a, b| a.name.cmp(&b.name));
    helper_defs.sort_by(|a, b| a.name.cmp(&b.name));

    let now = reader::chrono_like_now_pub();

    Ok(GenaiSchema {
        source: SourceInfo {
            framework: "js-genai".to_string(),
            source_dir: source_dir_str,
            extracted_at: now,
        },
        types,
        enums,
        type_aliases,
        helpers: helper_defs,
    })
}

/// Build a type-resolution lookup from a GenaiSchema.
/// Returns a map: js-genai type name → wire crate Rust type path.
pub fn build_type_lookup(schema: &GenaiSchema) -> HashMap<String, String> {
    let mut lookup = HashMap::new();

    // Types with wire equivalents
    for t in &schema.types {
        if let Some(ref wire) = t.wire_type {
            lookup.insert(t.name.clone(), wire.clone());
        }
    }

    // Enums with wire equivalents
    for e in &schema.enums {
        if let Some(ref wire) = e.wire_type {
            lookup.insert(e.name.clone(), wire.clone());
        }
    }

    // Type aliases — resolved rust_type
    for a in &schema.type_aliases {
        if !a.rust_type.contains("serde_json::Value") && !a.rust_type.starts_with("/* ") {
            lookup.insert(a.name.clone(), a.rust_type.clone());
        }
    }

    lookup
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a js-genai type alias to a Rust type.
fn map_genai_alias_to_rust(
    ts_def: &str,
    wire_types: &HashMap<&str, &str>,
) -> String {
    let ts = ts_def.trim();

    // Union types: pick the primary concrete type
    if ts.contains('|') {
        let parts: Vec<&str> = ts.split('|').map(|s| s.trim()).collect();

        // If one part has a wire equivalent, use it
        for part in &parts {
            let cleaned = part.trim_end_matches("[]");
            if let Some(wire) = wire_types.get(cleaned) {
                if part.ends_with("[]") {
                    return format!("Vec<{}>", wire);
                }
                return wire.to_string();
            }
        }

        // String unions
        if parts
            .iter()
            .all(|p| p.starts_with('\'') || p.starts_with('"'))
        {
            return "String".to_string();
        }

        // Fallback: use first concrete type
        if let Some(first) = parts.first() {
            return reader::map_ts_to_rust(first);
        }
    }

    // Direct type name check
    if let Some(wire) = wire_types.get(ts) {
        return wire.to_string();
    }

    reader::map_ts_to_rust(ts)
}

/// Extract `export function name(...)` declarations.
fn extract_helper_functions(source: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let func_re = regex::Regex::new(
        r"(?m)^export\s+function\s+(\w+)\s*\(([^)]*)\)\s*:\s*([^\{;]+)",
    )
    .unwrap();

    for cap in func_re.captures_iter(source) {
        let name = cap[1].to_string();
        let params = cap[2].trim().to_string();
        let return_type = cap[3].trim().to_string();
        let signature = format!("({}) => {}", params, return_type);
        results.push((name, signature));
    }
    results
}

fn dedup_by_name(types: &mut Vec<GenaiTypeDef>) {
    let mut seen = std::collections::HashSet::new();
    types.retain(|t| seen.insert(t.name.clone()));
}

fn dedup_enums_by_name(enums: &mut Vec<GenaiEnumDef>) {
    let mut seen = std::collections::HashSet::new();
    enums.retain(|e| seen.insert(e.name.clone()));
}

fn dedup_aliases_by_name(aliases: &mut Vec<GenaiTypeAlias>) {
    let mut seen = std::collections::HashSet::new();
    aliases.retain(|a| seen.insert(a.name.clone()));
}

fn dedup_helpers_by_name(helpers: &mut Vec<GenaiHelperDef>) {
    let mut seen = std::collections::HashSet::new();
    helpers.retain(|h| seen.insert(h.name.clone()));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_type_map_covers_core_types() {
        let map = wire_type_map();
        assert!(map.contains_key("Content"));
        assert!(map.contains_key("Part"));
        assert!(map.contains_key("FunctionCall"));
        assert!(map.contains_key("FunctionResponse"));
        assert!(map.contains_key("Tool"));
        assert!(map.contains_key("Blob"));
        assert!(map.contains_key("Session"));
        assert!(map.contains_key("Modality"));
        assert!(map.contains_key("SpeechConfig"));
        assert!(map.contains_key("LiveServerMessage"));
    }

    #[test]
    fn helper_map_covers_factory_functions() {
        let map = helper_map();
        assert_eq!(map.get("createPartFromText"), Some(&"Part::text"));
        assert_eq!(map.get("createUserContent"), Some(&"Content::user"));
        assert_eq!(map.get("createModelContent"), Some(&"Content::model"));
    }

    #[test]
    fn classify_content_types() {
        assert_eq!(classify_genai_type("Content"), GenaiTypeCategory::Content);
        assert_eq!(classify_genai_type("Part"), GenaiTypeCategory::Content);
        assert_eq!(classify_genai_type("Blob"), GenaiTypeCategory::Content);
    }

    #[test]
    fn classify_live_api_types() {
        assert_eq!(
            classify_genai_type("LiveConnectConfig"),
            GenaiTypeCategory::LiveApi
        );
        assert_eq!(classify_genai_type("Session"), GenaiTypeCategory::LiveApi);
    }

    #[test]
    fn classify_function_types() {
        assert_eq!(
            classify_genai_type("FunctionCall"),
            GenaiTypeCategory::FunctionCalling
        );
        assert_eq!(classify_genai_type("Tool"), GenaiTypeCategory::FunctionCalling);
    }

    #[test]
    fn map_alias_with_wire_type() {
        let wire = wire_type_map();
        let result = map_genai_alias_to_rust("Content | Part[] | string", &wire);
        assert_eq!(result, "rs_genai::prelude::Content");
    }

    #[test]
    fn map_alias_string_union() {
        let wire = wire_type_map();
        let result = map_genai_alias_to_rust("'TEXT' | 'AUDIO' | 'IMAGE'", &wire);
        assert_eq!(result, "String");
    }

    #[test]
    fn extract_helpers() {
        let source = r#"
export function createPartFromText(text: string): Part {
    return { text };
}

export function createUserContent(parts: Part[]): Content {
    return { role: 'user', parts };
}
"#;
        let helpers = extract_helper_functions(source);
        assert_eq!(helpers.len(), 2);
        assert_eq!(helpers[0].0, "createPartFromText");
        assert_eq!(helpers[1].0, "createUserContent");
    }

    #[test]
    fn build_lookup_from_schema() {
        let schema = GenaiSchema {
            source: SourceInfo {
                framework: "js-genai".to_string(),
                source_dir: "/tmp/test".to_string(),
                extracted_at: "2026-01-01T00:00:00Z".to_string(),
            },
            types: vec![GenaiTypeDef {
                name: "Content".to_string(),
                category: GenaiTypeCategory::Content,
                description: None,
                fields: vec![],
                extends: None,
                wire_type: Some("rs_genai::prelude::Content".to_string()),
                has_wire_equivalent: true,
            }],
            enums: vec![GenaiEnumDef {
                name: "Modality".to_string(),
                variants: vec!["TEXT".to_string(), "AUDIO".to_string()],
                description: None,
                wire_type: Some("rs_genai::prelude::Modality".to_string()),
                has_wire_equivalent: true,
            }],
            type_aliases: vec![],
            helpers: vec![],
        };

        let lookup = build_type_lookup(&schema);
        assert_eq!(
            lookup.get("Content"),
            Some(&"rs_genai::prelude::Content".to_string())
        );
        assert_eq!(
            lookup.get("Modality"),
            Some(&"rs_genai::prelude::Modality".to_string())
        );
    }
}
