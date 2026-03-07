//! Reader for ADK-JS TypeScript source.
//!
//! Extracts agent definitions, tool definitions, and general type definitions
//! from the ADK-JS source tree.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use walkdir::WalkDir;

use crate::schema::{AdkSchema, AgentDef, AgentKind, SourceInfo, ToolDef, TypeDef};

use super::typescript::{self as ts, RawInterface};

/// Read all TypeScript files from a directory tree and extract agent/tool/type
/// definitions into an AdkSchema.
pub fn read_source_dir(source_dir: &Path) -> Result<AdkSchema, String> {
    let source_dir_str = source_dir
        .to_str()
        .ok_or_else(|| "Invalid UTF-8 in source path".to_string())?
        .to_string();

    let mut agents = Vec::new();
    let mut tools = Vec::new();
    let mut types = Vec::new();

    let ts_files: Vec<_> = WalkDir::new(source_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "ts" || ext == "tsx")
        })
        .collect();

    for entry in &ts_files {
        let path = entry.path();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

        let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

        let module = ts::module_from_path(source_dir, path);

        let interfaces = ts::extract_interfaces(&content);

        for iface in &interfaces {
            let kind = classify_agent(&iface.name, file_stem);
            let is_tool = is_tool_interface(&iface.name, file_stem);

            if is_tool {
                tools.push(build_tool_def(iface));
            } else if let Some(agent_kind) = kind {
                agents.push(build_agent_def(iface, agent_kind));
            } else {
                types.push(build_type_def_from_interface(iface, &module));
            }
        }

        let enums = ts::extract_enums(&content);
        for (name, jsdoc, variants) in &enums {
            types.push(TypeDef {
                name: name.clone(),
                module: module.clone(),
                description: jsdoc.clone(),
                fields: Vec::new(),
                extends: None,
                is_enum: true,
                variants: variants.clone(),
            });
        }

        let type_aliases = ts::extract_type_aliases(&content);
        for (name, value) in &type_aliases {
            if ts::is_string_union(value) {
                let variants = ts::parse_string_union_variants(value);
                types.push(TypeDef {
                    name: name.clone(),
                    module: module.clone(),
                    description: None,
                    fields: Vec::new(),
                    extends: None,
                    is_enum: true,
                    variants,
                });
            }
        }
    }

    // Deduplicate types by name (keep the one with more fields)
    {
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut deduped_types: Vec<TypeDef> = Vec::new();
        for type_def in types {
            if let Some(&existing_idx) = seen.get(&type_def.name) {
                if type_def.fields.len() > deduped_types[existing_idx].fields.len() {
                    deduped_types[existing_idx] = type_def;
                }
            } else {
                seen.insert(type_def.name.clone(), deduped_types.len());
                deduped_types.push(type_def);
            }
        }
        types = deduped_types;
    }

    agents.sort_by(|a, b| a.name.cmp(&b.name));
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    types.sort_by(|a, b| a.name.cmp(&b.name));

    let now = ts::chrono_like_now();

    Ok(AdkSchema {
        source: SourceInfo {
            framework: "adk-js".to_string(),
            source_dir: source_dir_str,
            extracted_at: now,
        },
        agents,
        tools,
        types,
    })
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

fn classify_agent(iface_name: &str, file_stem: &str) -> Option<AgentKind> {
    let name_lower = iface_name.to_lowercase();
    let stem_lower = file_stem.to_lowercase();

    if name_lower == "baseagentconfig"
        || stem_lower == "base_agent" && name_lower.contains("config")
    {
        return Some(AgentKind::Base);
    }
    if name_lower == "llmagentconfig"
        || (stem_lower == "llm_agent" && name_lower.contains("config"))
    {
        return Some(AgentKind::Llm);
    }
    if name_lower == "sequentialagentconfig"
        || stem_lower == "sequential_agent" && name_lower.contains("config")
    {
        return Some(AgentKind::Sequential);
    }
    if name_lower == "parallelagentconfig"
        || stem_lower == "parallel_agent" && name_lower.contains("config")
    {
        return Some(AgentKind::Parallel);
    }
    if name_lower == "loopagentconfig"
        || (stem_lower == "loop_agent" && name_lower.contains("config"))
    {
        return Some(AgentKind::Loop);
    }

    if name_lower.ends_with("agentconfig") || name_lower.ends_with("agent_config") {
        return Some(AgentKind::Custom(iface_name.to_string()));
    }

    None
}

fn is_tool_interface(iface_name: &str, file_stem: &str) -> bool {
    let name_lower = iface_name.to_lowercase();
    let stem_lower = file_stem.to_lowercase();

    if name_lower.contains("tool")
        && (name_lower.contains("param")
            || name_lower.contains("config")
            || name_lower.contains("option"))
    {
        return true;
    }
    if stem_lower.contains("tool")
        && (name_lower.ends_with("params")
            || name_lower.ends_with("config")
            || name_lower.ends_with("request")
            || name_lower.ends_with("options"))
    {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Build schema definitions
// ---------------------------------------------------------------------------

fn build_agent_def(iface: &RawInterface, kind: AgentKind) -> AgentDef {
    let (fields, callbacks) = ts::parse_fields(&iface.body);
    AgentDef {
        name: iface.name.clone(),
        kind,
        description: iface.jsdoc.clone(),
        fields,
        callbacks,
        extends: iface.extends.clone(),
    }
}

fn build_tool_def(iface: &RawInterface) -> ToolDef {
    let (fields, _callbacks) = ts::parse_fields(&iface.body);
    ToolDef {
        name: iface.name.clone(),
        description: iface.jsdoc.clone(),
        fields,
        extends: iface.extends.clone(),
    }
}

fn build_type_def_from_interface(iface: &RawInterface, module: &str) -> TypeDef {
    let (fields, _callbacks) = ts::parse_fields(&iface.body);
    TypeDef {
        name: iface.name.clone(),
        module: module.to_string(),
        description: iface.jsdoc.clone(),
        fields,
        extends: iface.extends.clone(),
        is_enum: false,
        variants: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_agent() {
        assert_eq!(
            classify_agent("BaseAgentConfig", "base_agent"),
            Some(AgentKind::Base)
        );
        assert_eq!(
            classify_agent("LlmAgentConfig", "llm_agent"),
            Some(AgentKind::Llm)
        );
        assert_eq!(
            classify_agent("LoopAgentConfig", "loop_agent"),
            Some(AgentKind::Loop)
        );
        assert!(classify_agent("SomeRandomInterface", "utils").is_none());
    }

    #[test]
    fn test_is_tool_interface() {
        assert!(is_tool_interface("BaseToolParams", "base_tool"));
        assert!(is_tool_interface("AgentToolConfig", "agent_tool"));
        assert!(!is_tool_interface("BaseAgentConfig", "base_agent"));
    }

    #[test]
    fn test_full_base_agent_extraction() {
        let source = r#"
/**
 * The config of a base agent.
 */
export interface BaseAgentConfig {
  name: string;
  description?: string;
  parentAgent?: BaseAgent;
  subAgents?: BaseAgent[];
  beforeAgentCallback?: BeforeAgentCallback;
  afterAgentCallback?: AfterAgentCallback;
}
"#;
        let interfaces = ts::extract_interfaces(source);
        assert_eq!(interfaces.len(), 1);
        let iface = &interfaces[0];
        let def = build_agent_def(iface, AgentKind::Base);

        assert_eq!(def.name, "BaseAgentConfig");
        assert_eq!(def.kind, AgentKind::Base);
        assert_eq!(def.fields.len(), 4);
        assert_eq!(def.callbacks.len(), 2);
    }
}
