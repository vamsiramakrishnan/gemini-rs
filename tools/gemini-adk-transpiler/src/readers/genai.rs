//! Reader for the @google/genai (js-genai) TypeScript SDK.
//!
//! Extracts all exported types, enums, type aliases, and helper functions
//! from the js-genai source tree. Maps each extracted type to its
//! gemini-live Rust equivalent where one exists.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use walkdir::WalkDir;

use crate::schema::{
    GenaiEnumDef, GenaiHelperDef, GenaiSchema, GenaiTypeAlias, GenaiTypeCategory, GenaiTypeDef,
    SourceInfo,
};

use super::typescript as ts;

// ---------------------------------------------------------------------------
// Wire crate type mapping
// ---------------------------------------------------------------------------

/// Comprehensive mapping of js-genai type names → gemini-live Rust paths.
fn wire_type_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();

    // Content & Parts
    m.insert("Content", "gemini_live::prelude::Content");
    m.insert("Part", "gemini_live::prelude::Part");
    m.insert("Blob", "gemini_live::prelude::Blob");
    m.insert("FileData", "gemini_live::prelude::FileData");
    m.insert("ExecutableCode", "gemini_live::prelude::ExecutableCode");
    m.insert(
        "CodeExecutionResult",
        "gemini_live::prelude::CodeExecutionResult",
    );

    // Function calling
    m.insert("FunctionCall", "gemini_live::prelude::FunctionCall");
    m.insert("FunctionResponse", "gemini_live::prelude::FunctionResponse");
    m.insert(
        "FunctionDeclaration",
        "gemini_live::prelude::FunctionDeclaration",
    );
    m.insert("Tool", "gemini_live::prelude::Tool");
    m.insert("ToolConfig", "gemini_live::prelude::ToolConfig");
    m.insert(
        "FunctionCallingConfig",
        "gemini_live::prelude::FunctionCallingConfig",
    );

    // Enums
    m.insert("Modality", "gemini_live::prelude::Modality");
    m.insert("Role", "gemini_live::prelude::Role");

    // Configuration
    m.insert("GenerationConfig", "gemini_live::prelude::GenerationConfig");
    m.insert("SpeechConfig", "gemini_live::prelude::SpeechConfig");
    m.insert("VoiceConfig", "gemini_live::prelude::VoiceConfig");
    m.insert(
        "PrebuiltVoiceConfig",
        "gemini_live::prelude::PrebuiltVoiceConfig",
    );
    m.insert("ThinkingConfig", "gemini_live::prelude::ThinkingConfig");
    m.insert(
        "RealtimeInputConfig",
        "gemini_live::prelude::RealtimeInputConfig",
    );
    m.insert(
        "AutomaticActivityDetection",
        "gemini_live::prelude::AutomaticActivityDetection",
    );
    m.insert(
        "SessionResumptionConfig",
        "gemini_live::prelude::SessionResumptionConfig",
    );
    m.insert(
        "ContextWindowCompressionConfig",
        "gemini_live::prelude::ContextWindowCompressionConfig",
    );
    m.insert("SlidingWindow", "gemini_live::prelude::SlidingWindow");
    m.insert(
        "ProactivityConfig",
        "gemini_live::prelude::ProactivityConfig",
    );
    m.insert(
        "InputAudioTranscription",
        "gemini_live::prelude::InputAudioTranscription",
    );
    m.insert(
        "OutputAudioTranscription",
        "gemini_live::prelude::OutputAudioTranscription",
    );

    // Metadata
    m.insert("UsageMetadata", "gemini_live::prelude::UsageMetadata");
    m.insert(
        "GroundingMetadata",
        "gemini_live::prelude::GroundingMetadata",
    );
    m.insert(
        "UrlContextMetadata",
        "gemini_live::prelude::UrlContextMetadata",
    );

    // Session/Live API messages
    m.insert("LiveServerMessage", "gemini_live::prelude::ServerMessage");
    m.insert(
        "LiveServerSetupComplete",
        "gemini_live::prelude::SetupCompletePayload",
    );
    m.insert(
        "LiveServerContent",
        "gemini_live::prelude::ServerContentPayload",
    );
    m.insert(
        "LiveServerToolCall",
        "gemini_live::prelude::ToolCallPayload",
    );
    m.insert(
        "LiveServerToolCallCancellation",
        "gemini_live::prelude::ToolCallCancellationPayload",
    );
    m.insert("LiveServerGoAway", "gemini_live::prelude::GoAwayPayload");
    m.insert(
        "LiveServerSessionResumptionUpdate",
        "gemini_live::prelude::SessionResumptionUpdatePayload",
    );
    m.insert(
        "LiveClientContent",
        "gemini_live::prelude::ClientContentPayload",
    );
    m.insert(
        "LiveClientRealtimeInput",
        "gemini_live::prelude::RealtimeInputPayload",
    );
    m.insert(
        "LiveClientToolResponse",
        "gemini_live::prelude::ToolResponsePayload",
    );
    m.insert("LiveClientSetup", "gemini_live::prelude::SetupPayload");
    m.insert("ActivityStart", "gemini_live::prelude::ActivityStart");
    m.insert("ActivityEnd", "gemini_live::prelude::ActivityEnd");

    // Session abstraction
    m.insert("Session", "gemini_live::prelude::SessionHandle");

    // Transcription
    m.insert(
        "Transcription",
        "gemini_live::prelude::TranscriptionPayload",
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
    m.insert("Modality", "gemini_live::prelude::Modality");
    m.insert("MediaResolution", "gemini_live::prelude::MediaResolution");
    m.insert("Type", "gemini_live::prelude::SchemaType");
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

        // Generate API types
        n if n.starts_with("Generate")
            || n == "Candidate"
            || n == "FinishReason"
            || n.starts_with("SafetyRating")
            || n.starts_with("Citation")
            || n == "PromptFeedback" =>
        {
            GenaiTypeCategory::Generate
        }

        // Embed API types
        n if n.starts_with("Embed") || n.starts_with("ContentEmbedding") => {
            GenaiTypeCategory::Embed
        }

        // File API types
        n if n.starts_with("File") && n != "FileData" => GenaiTypeCategory::Files,

        // Model API types
        n if n.starts_with("Model") && n != "Modality" => GenaiTypeCategory::Models,

        // Token counting types
        n if n.starts_with("CountToken") || n.starts_with("ComputeToken") => {
            GenaiTypeCategory::Tokens
        }

        // Cached content types
        n if n.starts_with("CachedContent") || n.starts_with("Cache") => GenaiTypeCategory::Caches,

        // Tuning types
        n if n.starts_with("Tuning") || n.starts_with("TunedModel") || n == "Hyperparameters" => {
            GenaiTypeCategory::Tunings
        }

        // Batch types
        n if n.starts_with("Batch") => GenaiTypeCategory::Batches,

        // Chat session types
        n if n.starts_with("Chat") || n == "SendMessageConfig" => GenaiTypeCategory::Chats,

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

        let interfaces = ts::extract_interfaces(&content);
        for iface in &interfaces {
            let has_wire = wire_types.contains_key(iface.name.as_str());
            let wire_type = wire_types.get(iface.name.as_str()).map(|s| s.to_string());
            let (fields, _callbacks) = ts::parse_fields(&iface.body);

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

        let extracted_enums = ts::extract_enums(&content);
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

        let aliases = ts::extract_type_aliases(&content);
        for (name, ts_def) in &aliases {
            let rust_type = map_genai_alias_to_rust(ts_def, &wire_types);
            type_aliases.push(GenaiTypeAlias {
                name: name.clone(),
                ts_definition: ts_def.clone(),
                rust_type,
            });
        }

        let extracted_helpers = ts::extract_helper_functions(&content);
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

    dedup_by_name(&mut types);
    dedup_enums_by_name(&mut enums);
    dedup_aliases_by_name(&mut type_aliases);
    dedup_helpers_by_name(&mut helper_defs);

    types.sort_by(|a, b| a.name.cmp(&b.name));
    enums.sort_by(|a, b| a.name.cmp(&b.name));
    type_aliases.sort_by(|a, b| a.name.cmp(&b.name));
    helper_defs.sort_by(|a, b| a.name.cmp(&b.name));

    let now = ts::chrono_like_now();

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
        rest_modules: vec![],
    })
}

/// Build a type-resolution lookup from a GenaiSchema.
pub fn build_type_lookup(schema: &GenaiSchema) -> HashMap<String, String> {
    let mut lookup = HashMap::new();

    for t in &schema.types {
        if let Some(ref wire) = t.wire_type {
            lookup.insert(t.name.clone(), wire.clone());
        }
    }

    for e in &schema.enums {
        if let Some(ref wire) = e.wire_type {
            lookup.insert(e.name.clone(), wire.clone());
        }
    }

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

fn map_genai_alias_to_rust(ts_def: &str, wire_types: &HashMap<&str, &str>) -> String {
    let trimmed = ts_def.trim();

    if trimmed.contains('|') {
        let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();

        for part in &parts {
            let cleaned = part.trim_end_matches("[]");
            if let Some(wire) = wire_types.get(cleaned) {
                if part.ends_with("[]") {
                    return format!("Vec<{}>", wire);
                }
                return wire.to_string();
            }
        }

        if parts
            .iter()
            .all(|p| p.starts_with('\'') || p.starts_with('"'))
        {
            return "String".to_string();
        }

        if let Some(first) = parts.first() {
            return ts::map_ts_to_rust(first);
        }
    }

    if let Some(wire) = wire_types.get(trimmed) {
        return wire.to_string();
    }

    ts::map_ts_to_rust(trimmed)
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
        assert!(map.contains_key("Tool"));
        assert!(map.contains_key("LiveServerMessage"));
    }

    #[test]
    fn classify_content_types() {
        assert_eq!(classify_genai_type("Content"), GenaiTypeCategory::Content);
        assert_eq!(classify_genai_type("Part"), GenaiTypeCategory::Content);
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
    fn classify_generate_types() {
        assert_eq!(
            classify_genai_type("GenerateContentResponse"),
            GenaiTypeCategory::Generate
        );
        assert_eq!(
            classify_genai_type("Candidate"),
            GenaiTypeCategory::Generate
        );
    }

    #[test]
    fn classify_new_api_categories() {
        assert_eq!(
            classify_genai_type("EmbedContentRequest"),
            GenaiTypeCategory::Embed
        );
        assert_eq!(
            classify_genai_type("FileUploadResponse"),
            GenaiTypeCategory::Files
        );
        assert_eq!(classify_genai_type("ModelInfo"), GenaiTypeCategory::Models);
        assert_eq!(
            classify_genai_type("CountTokensRequest"),
            GenaiTypeCategory::Tokens
        );
        assert_eq!(
            classify_genai_type("CachedContentConfig"),
            GenaiTypeCategory::Caches
        );
        assert_eq!(classify_genai_type("TuningJob"), GenaiTypeCategory::Tunings);
        assert_eq!(classify_genai_type("BatchJob"), GenaiTypeCategory::Batches);
        assert_eq!(classify_genai_type("ChatSession"), GenaiTypeCategory::Chats);
    }

    #[test]
    fn map_alias_with_wire_type() {
        let wire = wire_type_map();
        let result = map_genai_alias_to_rust("Content | Part[] | string", &wire);
        assert_eq!(result, "gemini_live::prelude::Content");
    }

    #[test]
    fn build_lookup_from_schema() {
        let schema = GenaiSchema {
            source: SourceInfo {
                framework: "js-genai".to_string(),
                source_dir: "/tmp/test".to_string(),
                extracted_at: "2026-01-01T00:00:00Z".to_string(),
            },
            rest_modules: vec![],
            types: vec![GenaiTypeDef {
                name: "Content".to_string(),
                category: GenaiTypeCategory::Content,
                description: None,
                fields: vec![],
                extends: None,
                wire_type: Some("gemini_live::prelude::Content".to_string()),
                has_wire_equivalent: true,
            }],
            enums: vec![],
            type_aliases: vec![],
            helpers: vec![],
        };

        let lookup = build_type_lookup(&schema);
        assert_eq!(
            lookup.get("Content"),
            Some(&"gemini_live::prelude::Content".to_string())
        );
    }
}
