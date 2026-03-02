use std::collections::HashMap;
use std::fs;
use std::path::Path;

use regex::Regex;
use walkdir::WalkDir;

use crate::schema::{
    AdkSchema, AgentDef, AgentKind, CallbackDef, FieldDef, SourceInfo, ToolDef, TypeDef,
};

/// Well-known callback type names that should be extracted as CallbackDef
/// rather than regular FieldDef.
const CALLBACK_TYPE_NAMES: &[&str] = &[
    "BeforeAgentCallback",
    "AfterAgentCallback",
    "BeforeModelCallback",
    "AfterModelCallback",
    "BeforeToolCallback",
    "AfterToolCallback",
    "SingleAgentCallback",
    "SingleBeforeModelCallback",
    "SingleAfterModelCallback",
    "SingleBeforeToolCallback",
    "SingleAfterToolCallback",
    "InstructionProvider",
];

/// Determine the module name from a file path relative to the source directory.
///
/// For example, if source_dir is `/tmp/adk-js/core/src/` and the file is
/// `/tmp/adk-js/core/src/events/event.ts`, the module is `"events"`.
/// Files directly in the source directory get module `"root"`.
fn module_from_path(source_dir: &Path, file_path: &Path) -> String {
    if let Ok(relative) = file_path.strip_prefix(source_dir) {
        // The first component of the relative path is the module directory
        if let Some(first) = relative.components().next() {
            let first_str = first.as_os_str().to_str().unwrap_or("root");
            // If the first component is also the file itself (no subdirectory), module is "root"
            if relative.components().count() == 1 {
                return "root".to_string();
            }
            return first_str.to_string();
        }
    }
    "root".to_string()
}

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

    // Collect all .ts files
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

        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        let module = module_from_path(source_dir, path);

        // Extract interfaces from this file
        let interfaces = extract_interfaces(&content);

        for iface in &interfaces {
            let kind = classify_agent(&iface.name, file_stem);
            let is_tool = is_tool_interface(&iface.name, file_stem);

            if is_tool {
                tools.push(build_tool_def(iface));
            } else if let Some(agent_kind) = kind {
                agents.push(build_agent_def(iface, agent_kind));
            } else {
                // All other exported interfaces become TypeDef entries
                types.push(build_type_def_from_interface(iface, &module));
            }
        }

        // Extract enums: `export enum Foo { A, B, C }`
        let enums = extract_enums(&content);
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

        // Extract string union type aliases as enums:
        // `export type Foo = 'a' | 'b' | 'c';`
        let type_aliases = extract_type_aliases(&content);
        for (name, value) in &type_aliases {
            if is_string_union(value) {
                let variants = parse_string_union_variants(value);
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
                // Keep the one with more fields
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

    // Sort for deterministic output
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    types.sort_by(|a, b| a.name.cmp(&b.name));

    let now = chrono_like_now();

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

/// Produce a simple ISO 8601 timestamp without pulling in the chrono crate.
fn chrono_like_now() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Simple formatting: seconds since epoch as a fallback
    // For a proper ISO 8601, we do basic arithmetic
    let days = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;

    // Calculate year/month/day from days since epoch (1970-01-01)
    let (year, month, day) = days_to_ymd(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let month_days: &[u64] = if is_leap(year) {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u64;
    for &md in month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ---------------------------------------------------------------------------
// Interface extraction
// ---------------------------------------------------------------------------

/// A raw interface parsed from TypeScript source.
#[derive(Debug)]
struct RawInterface {
    name: String,
    extends: Option<String>,
    jsdoc: Option<String>,
    body: String,
}

/// Extract all `export interface Foo extends Bar { ... }` blocks from source.
fn extract_interfaces(source: &str) -> Vec<RawInterface> {
    let mut results = Vec::new();

    // Pattern: optional `export` keyword, then `interface Name [extends Parent] {`
    // We use `(?m)` for multiline and allow leading whitespace instead of strict `^`.
    let iface_re = Regex::new(
        r"(?m)(?:^|\n)\s*(?:export\s+)?interface\s+(\w+)(?:\s+extends\s+(\w+))?\s*\{"
    ).unwrap();

    for cap in iface_re.captures_iter(source) {
        let name = cap[1].to_string();
        let extends = cap.get(2).map(|m| m.as_str().to_string());
        let match_start = cap.get(0).unwrap().start();

        // Find the JSDoc comment preceding this interface
        let jsdoc = extract_preceding_jsdoc(source, match_start);

        // Find the matching closing brace
        let body_start = cap.get(0).unwrap().end();
        if let Some(body) = extract_brace_block(source, body_start) {
            results.push(RawInterface {
                name,
                extends,
                jsdoc,
                body,
            });
        }
    }

    results
}

/// Extract type alias declarations: `export type Foo = ...;`
fn extract_type_aliases(source: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let type_re = Regex::new(
        r"(?m)^(?:export\s+)?type\s+(\w+)\s*(?:<[^>]*>)?\s*=\s*([^;]+);"
    ).unwrap();

    for cap in type_re.captures_iter(source) {
        let name = cap[1].to_string();
        let value = cap[2].trim().to_string();
        results.push((name, value));
    }
    results
}

/// Extract `export enum Foo { A, B, C }` blocks from TypeScript source.
/// Returns a list of (name, optional jsdoc, variants).
fn extract_enums(source: &str) -> Vec<(String, Option<String>, Vec<String>)> {
    let mut results = Vec::new();

    let enum_re = Regex::new(
        r"(?m)(?:^|\n)\s*(?:export\s+)?(?:const\s+)?enum\s+(\w+)\s*\{"
    ).unwrap();

    for cap in enum_re.captures_iter(source) {
        let name = cap[1].to_string();
        let match_start = cap.get(0).unwrap().start();
        let body_start = cap.get(0).unwrap().end();

        let jsdoc = extract_preceding_jsdoc(source, match_start);

        if let Some(body) = extract_brace_block(source, body_start) {
            let variants: Vec<String> = body
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                // Strip enum value assignments: `A = 'a'` -> `A`
                .map(|s| {
                    if let Some(eq_pos) = s.find('=') {
                        s[..eq_pos].trim().to_string()
                    } else {
                        s.to_string()
                    }
                })
                // Filter out comments and empty entries
                .filter(|s| !s.starts_with("//") && !s.starts_with("/*") && !s.is_empty())
                .collect();

            if !variants.is_empty() {
                results.push((name, jsdoc, variants));
            }
        }
    }

    results
}

/// Check if a type alias value is a string literal union (e.g. `'a' | 'b' | 'c'`).
fn is_string_union(value: &str) -> bool {
    let parts: Vec<&str> = value.split('|').map(|s| s.trim()).collect();
    parts.len() >= 2 && parts.iter().all(|p| {
        let p = p.trim();
        (p.starts_with('\'') && p.ends_with('\''))
            || (p.starts_with('"') && p.ends_with('"'))
    })
}

/// Parse string union variants from a type alias value.
/// E.g. `'a' | 'b' | 'c'` -> `["a", "b", "c"]`
fn parse_string_union_variants(value: &str) -> Vec<String> {
    value
        .split('|')
        .map(|s| s.trim())
        .map(|s| s.trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract the JSDoc block comment (`/** ... */`) immediately preceding a
/// given byte position in the source.
fn extract_preceding_jsdoc(source: &str, pos: usize) -> Option<String> {
    let before = &source[..pos];
    let trimmed = before.trim_end();
    if trimmed.ends_with("*/") {
        // Walk back to find the opening `/**`
        if let Some(start) = trimmed.rfind("/**") {
            let comment = &trimmed[start..];
            // Clean up: remove `/**`, `*/`, and leading ` * ` from each line
            let cleaned = clean_jsdoc(comment);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    None
}

/// Clean a JSDoc comment into plain text.
fn clean_jsdoc(comment: &str) -> String {
    let lines: Vec<&str> = comment.lines().collect();
    let mut result = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        // Skip opening/closing markers
        if trimmed == "/**" || trimmed == "*/" {
            continue;
        }
        // Handle single-line JSDoc: `/** text */`
        let trimmed = if let Some(rest) = trimmed.strip_prefix("/**") {
            if let Some(inner) = rest.strip_suffix("*/") {
                inner.trim()
            } else {
                rest.trim()
            }
        } else {
            trimmed
        };
        // Remove leading ` * ` or ` *`
        let content = if let Some(rest) = trimmed.strip_prefix("* ") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix('*') {
            rest
        } else {
            trimmed
        };
        // Skip @license, @param, @return, @yields, etc.
        if content.starts_with("@license")
            || content.starts_with("@param")
            || content.starts_with("@return")
            || content.starts_with("@yields")
            || content.starts_with("SPDX-License")
            || content.starts_with("Copyright")
        {
            continue;
        }
        if !content.is_empty() {
            result.push(content.to_string());
        }
    }
    result.join(" ").trim().to_string()
}

/// Extract the content between matching braces starting at `start` (which
/// should point just past the opening `{`). Returns the content without
/// the outer braces.
fn extract_brace_block(source: &str, start: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut depth = 1i32;
    let mut i = start;
    while i < len && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            // Skip line comments: `// ...`
            b'/' if i + 1 < len && bytes[i + 1] == b'/' => {
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            // Skip block comments: `/* ... */`
            b'/' if i + 1 < len && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < len {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 1; // point to '/', the outer i+=1 advances past it
                        break;
                    }
                    i += 1;
                }
            }
            // Skip string literals (but only outside comments)
            b'"' | b'`' => {
                let quote = bytes[i];
                i += 1;
                while i < len && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1; // skip escaped char
                    }
                    i += 1;
                }
            }
            // Single-quoted strings: only skip if it looks like a proper
            // string literal (not an apostrophe in English text).
            // A TS string literal `'...'` has the closing quote on the same line.
            b'\'' => {
                // Look ahead for the closing quote on the same line
                let mut j = i + 1;
                let mut found = false;
                while j < len && bytes[j] != b'\n' {
                    if bytes[j] == b'\'' {
                        found = true;
                        break;
                    }
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if found {
                    // Check that what's inside looks like a string literal
                    // (no spaces before the closing quote in short content,
                    // or it follows a : or = or | which is typical for TS)
                    i = j; // skip to closing quote
                }
                // Otherwise treat `'` as a regular character (apostrophe)
            }
            _ => {}
        }
        i += 1;
    }
    if depth == 0 {
        // i-1 is the closing brace
        Some(source[start..i - 1].to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Field extraction from interface body
// ---------------------------------------------------------------------------

/// Parse fields from an interface body string.
fn parse_fields(body: &str) -> (Vec<FieldDef>, Vec<CallbackDef>) {
    let mut fields = Vec::new();
    let mut callbacks = Vec::new();

    // Pattern for interface fields:
    //   optional JSDoc, then `fieldName?: TypeExpr;` or `fieldName: TypeExpr;`
    // We process line by line, accumulating JSDoc.
    let lines: Vec<&str> = body.lines().collect();
    let mut current_jsdoc: Vec<String> = Vec::new();
    let mut in_jsdoc = false;

    let field_re = Regex::new(
        r"^\s*(?:readonly\s+)?(\w+)(\??):\s*(.+?)\s*;?\s*$"
    ).unwrap();

    for line in &lines {
        let trimmed = line.trim();

        // Track JSDoc blocks
        if trimmed.starts_with("/**") && trimmed.ends_with("*/") {
            // Single-line JSDoc
            current_jsdoc = vec![clean_jsdoc(trimmed)];
            continue;
        }
        if trimmed.starts_with("/**") {
            in_jsdoc = true;
            current_jsdoc.clear();
            continue;
        }
        if in_jsdoc {
            if trimmed.ends_with("*/") {
                in_jsdoc = false;
            } else {
                let content = trimmed.strip_prefix("* ").or_else(|| trimmed.strip_prefix('*')).unwrap_or(trimmed);
                if !content.starts_with('@')
                    && !content.starts_with("Copyright")
                    && !content.starts_with("SPDX")
                    && !content.is_empty()
                {
                    current_jsdoc.push(content.to_string());
                }
            }
            continue;
        }

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if trimmed.is_empty() {
                // Reset JSDoc if there's a blank line gap
                current_jsdoc.clear();
            }
            continue;
        }

        // Try to match a field definition
        if let Some(cap) = field_re.captures(trimmed) {
            let name = cap[1].to_string();
            let optional = &cap[2] == "?";
            let ts_type = cap[3].trim_end_matches(';').trim().to_string();

            let description = if current_jsdoc.is_empty() {
                None
            } else {
                Some(current_jsdoc.join(" "))
            };

            // Check if this is a callback type
            if is_callback_type(&ts_type) {
                callbacks.push(CallbackDef {
                    name,
                    ts_signature: ts_type,
                    description,
                });
            } else {
                let rust_type = map_ts_to_rust(&ts_type);
                fields.push(FieldDef {
                    name,
                    ts_type,
                    rust_type,
                    optional,
                    default_value: None,
                    description,
                });
            }

            current_jsdoc.clear();
        }
    }

    // Deduplicate fields by name (keep the last occurrence)
    {
        let mut seen = std::collections::HashSet::new();
        let mut deduped = Vec::new();
        for field in fields.into_iter().rev() {
            if seen.insert(field.name.clone()) {
                deduped.push(field);
            }
        }
        deduped.reverse();
        fields = deduped;
    }

    (fields, callbacks)
}

/// Check if a TypeScript type name refers to a callback.
///
/// A type is considered a callback when the entire type (ignoring array
/// suffixes) matches a known callback name. Union types like
/// `string | InstructionProvider` are NOT classified as callbacks -- the
/// union will be handled by the type mapper instead.
fn is_callback_type(ts_type: &str) -> bool {
    let normalized = ts_type.trim().trim_end_matches("[]");

    // If the type is a union, check each alternative individually.
    // Only classify as callback if ALL alternatives are callback types or
    // callback arrays. In practice, ADK callbacks are either a single callback
    // type or `SingleCallback | SingleCallback[]`.
    if normalized.contains('|') {
        let parts: Vec<&str> = normalized.split('|').map(|s| s.trim()).collect();
        return parts.iter().all(|p| {
            let p = p.trim_end_matches("[]");
            CALLBACK_TYPE_NAMES.contains(&p) || p.contains("=>")
        });
    }

    // Direct match against known callback type names
    for &cb in CALLBACK_TYPE_NAMES {
        if normalized == cb {
            return true;
        }
    }

    // Inline function types
    if normalized.contains("=>") {
        return true;
    }
    false
}

/// Map a TypeScript type string to an approximate Rust type.
pub fn map_ts_to_rust(ts_type: &str) -> String {
    let ts = ts_type.trim();

    // Handle union types by taking the first concrete type
    // e.g. `string | BaseLlm` -> String
    // e.g. `'default' | 'none'` -> String (string literal union)

    // Direct mappings
    match ts {
        "string" => return "String".to_string(),
        "number" => return "f64".to_string(),
        "boolean" => return "bool".to_string(),
        "any" | "unknown" => return "serde_json::Value".to_string(),
        "void" => return "()".to_string(),
        "undefined" => return "()".to_string(),
        _ => {}
    }

    // Array types: `Foo[]` or `Array<Foo>`
    if let Some(inner) = ts.strip_suffix("[]") {
        let rust_inner = map_ts_to_rust(inner);
        return format!("Vec<{}>", rust_inner);
    }
    let array_re = Regex::new(r"^Array<(.+)>$").unwrap();
    if let Some(cap) = array_re.captures(ts) {
        let rust_inner = map_ts_to_rust(&cap[1]);
        return format!("Vec<{}>", rust_inner);
    }

    // String literal unions: `'default' | 'none'`
    if ts.contains('|') && ts.contains('\'') {
        return "String".to_string();
    }

    // Union types with classes: `string | BaseLlm`, `BaseTool | BaseToolset`
    if ts.contains('|') {
        let parts: Vec<&str> = ts.split('|').map(|s| s.trim()).collect();
        // If one side is `string`, map to String
        if parts.contains(&"string") {
            return "String".to_string();
        }
        // If it's `Example[] | BaseExampleProvider`, use Vec
        if let Some(arr) = parts.iter().find(|p| p.ends_with("[]")) {
            return map_ts_to_rust(arr);
        }
        // Otherwise take the first part
        return map_ts_to_rust(parts[0]);
    }

    // Known ADK types
    match ts {
        "BaseAgent" => "AgentRef".to_string(),
        "BaseTool" => "ToolRef".to_string(),
        "BaseToolset" => "ToolRef".to_string(),
        "ToolUnion" => "ToolRef".to_string(),
        "GenerateContentConfig" => "serde_json::Value".to_string(),
        "LlmAgentSchema" => "serde_json::Value".to_string(),
        "BaseCodeExecutor" => "CodeExecutorRef".to_string(),
        "Content" => "Content".to_string(),
        "Schema" => "serde_json::Value".to_string(),
        _ => {
            // If it looks like a TS class/interface name, keep it as-is
            if ts.chars().next().is_some_and(|c| c.is_uppercase()) {
                ts.to_string()
            } else {
                // Fallback
                format!("/* {} */ String", ts)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Classify an interface as an agent kind based on its name and file stem.
/// Returns None if the interface doesn't appear to be an agent config.
fn classify_agent(iface_name: &str, file_stem: &str) -> Option<AgentKind> {
    // Only interfaces ending in "Config" that relate to agents
    let name_lower = iface_name.to_lowercase();
    let stem_lower = file_stem.to_lowercase();

    // Check for known agent config interfaces
    if name_lower == "baseagentconfig" || stem_lower == "base_agent" && name_lower.contains("config") {
        return Some(AgentKind::Base);
    }
    if name_lower == "llmagentconfig" || (stem_lower == "llm_agent" && name_lower.contains("config")) {
        return Some(AgentKind::Llm);
    }
    if name_lower == "sequentialagentconfig" || stem_lower == "sequential_agent" && name_lower.contains("config") {
        return Some(AgentKind::Sequential);
    }
    if name_lower == "parallelagentconfig" || stem_lower == "parallel_agent" && name_lower.contains("config") {
        return Some(AgentKind::Parallel);
    }
    if name_lower == "loopagentconfig" || (stem_lower == "loop_agent" && name_lower.contains("config")) {
        return Some(AgentKind::Loop);
    }

    // Generic: any interface ending in "AgentConfig" from an agent file
    if name_lower.ends_with("agentconfig") || name_lower.ends_with("agent_config") {
        return Some(AgentKind::Custom(iface_name.to_string()));
    }

    None
}

/// Check if an interface is a tool-related config.
fn is_tool_interface(iface_name: &str, file_stem: &str) -> bool {
    let name_lower = iface_name.to_lowercase();
    let stem_lower = file_stem.to_lowercase();

    // Known tool config interfaces
    if name_lower.contains("tool") && (name_lower.contains("param") || name_lower.contains("config") || name_lower.contains("option")) {
        return true;
    }
    // Interfaces in tool files that look like params/config
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
    let (fields, callbacks) = parse_fields(&iface.body);
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
    let (fields, _callbacks) = parse_fields(&iface.body);
    ToolDef {
        name: iface.name.clone(),
        description: iface.jsdoc.clone(),
        fields,
        extends: iface.extends.clone(),
    }
}

fn build_type_def_from_interface(iface: &RawInterface, module: &str) -> TypeDef {
    let (fields, _callbacks) = parse_fields(&iface.body);
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
    fn test_map_ts_to_rust_basic() {
        assert_eq!(map_ts_to_rust("string"), "String");
        assert_eq!(map_ts_to_rust("number"), "f64");
        assert_eq!(map_ts_to_rust("boolean"), "bool");
        assert_eq!(map_ts_to_rust("void"), "()");
    }

    #[test]
    fn test_map_ts_to_rust_arrays() {
        assert_eq!(map_ts_to_rust("string[]"), "Vec<String>");
        assert_eq!(map_ts_to_rust("BaseAgent[]"), "Vec<AgentRef>");
        assert_eq!(map_ts_to_rust("ToolUnion[]"), "Vec<ToolRef>");
    }

    #[test]
    fn test_map_ts_to_rust_unions() {
        assert_eq!(map_ts_to_rust("string | BaseLlm"), "String");
        assert_eq!(map_ts_to_rust("'default' | 'none'"), "String");
    }

    #[test]
    fn test_map_ts_to_rust_known_types() {
        assert_eq!(map_ts_to_rust("BaseAgent"), "AgentRef");
        assert_eq!(map_ts_to_rust("GenerateContentConfig"), "serde_json::Value");
    }

    #[test]
    fn test_extract_interfaces_simple() {
        let source = r#"
/** The config of a base agent. */
export interface BaseAgentConfig {
    name: string;
    description?: string;
}
"#;
        let interfaces = extract_interfaces(source);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0].name, "BaseAgentConfig");
        assert!(interfaces[0].extends.is_none());
        assert!(interfaces[0].jsdoc.as_ref().unwrap().contains("config of a base agent"));
    }

    #[test]
    fn test_extract_interfaces_extends() {
        let source = r#"
/** LLM agent config. */
export interface LlmAgentConfig extends BaseAgentConfig {
    model?: string | BaseLlm;
    instruction?: string | InstructionProvider;
    tools?: ToolUnion[];
}
"#;
        let interfaces = extract_interfaces(source);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0].name, "LlmAgentConfig");
        assert_eq!(interfaces[0].extends.as_deref(), Some("BaseAgentConfig"));
    }

    #[test]
    fn test_parse_fields_basic() {
        let body = r#"
    name: string;
    description?: string;
    subAgents?: BaseAgent[];
"#;
        let (fields, callbacks) = parse_fields(body);
        assert_eq!(callbacks.len(), 0);
        assert_eq!(fields.len(), 3);

        assert_eq!(fields[0].name, "name");
        assert_eq!(fields[0].ts_type, "string");
        assert_eq!(fields[0].rust_type, "String");
        assert!(!fields[0].optional);

        assert_eq!(fields[1].name, "description");
        assert!(fields[1].optional);

        assert_eq!(fields[2].name, "subAgents");
        assert_eq!(fields[2].rust_type, "Vec<AgentRef>");
        assert!(fields[2].optional);
    }

    #[test]
    fn test_parse_fields_with_jsdoc() {
        let body = r#"
    /** The model to use for the agent. */
    model?: string | BaseLlm;
    /** Instructions for the LLM model, guiding the agent's behavior. */
    instruction?: string | InstructionProvider;
"#;
        let (fields, callbacks) = parse_fields(body);
        // `string | InstructionProvider` is a union where `string` is not a
        // callback, so the whole type is treated as a field, not a callback.
        assert_eq!(fields.len(), 2);
        assert_eq!(callbacks.len(), 0);

        assert_eq!(fields[0].name, "model");
        assert!(fields[0].description.as_ref().unwrap().contains("model to use"));

        assert_eq!(fields[1].name, "instruction");
        assert!(fields[1].description.as_ref().unwrap().contains("Instructions"));
    }

    #[test]
    fn test_parse_callbacks() {
        let body = r#"
    /** Callback before agent runs. */
    beforeAgentCallback?: BeforeAgentCallback;
    afterAgentCallback?: AfterAgentCallback;
"#;
        let (fields, callbacks) = parse_fields(body);
        assert_eq!(fields.len(), 0);
        assert_eq!(callbacks.len(), 2);
        assert_eq!(callbacks[0].name, "beforeAgentCallback");
        assert_eq!(callbacks[0].ts_signature, "BeforeAgentCallback");
    }

    #[test]
    fn test_classify_agent() {
        assert_eq!(classify_agent("BaseAgentConfig", "base_agent"), Some(AgentKind::Base));
        assert_eq!(classify_agent("LlmAgentConfig", "llm_agent"), Some(AgentKind::Llm));
        assert_eq!(classify_agent("LoopAgentConfig", "loop_agent"), Some(AgentKind::Loop));
        assert!(classify_agent("SomeRandomInterface", "utils").is_none());
    }

    #[test]
    fn test_is_tool_interface() {
        assert!(is_tool_interface("BaseToolParams", "base_tool"));
        assert!(is_tool_interface("AgentToolConfig", "agent_tool"));
        assert!(!is_tool_interface("BaseAgentConfig", "base_agent"));
    }

    #[test]
    fn test_extract_brace_block() {
        let source = "{ foo: 1; bar: 2; }";
        let body = extract_brace_block(source, 1).unwrap();
        assert_eq!(body.trim(), "foo: 1; bar: 2;");
    }

    #[test]
    fn test_extract_brace_block_nested() {
        let source = "{ outer: { inner: 1 }; }";
        let body = extract_brace_block(source, 1).unwrap();
        assert_eq!(body.trim(), "outer: { inner: 1 };");
    }

    #[test]
    fn test_clean_jsdoc() {
        let comment = "/** The config of a base agent. */";
        assert_eq!(clean_jsdoc(comment), "The config of a base agent.");
    }

    #[test]
    fn test_clean_jsdoc_multiline() {
        let comment = r#"/**
 * A shell agent that run its sub-agents in a loop.
 *
 * When sub-agent generates an event with escalate or max_iterations are
 * reached, the loop agent will stop.
 */"#;
        let cleaned = clean_jsdoc(comment);
        assert!(cleaned.contains("shell agent"));
        assert!(cleaned.contains("loop"));
    }

    #[test]
    fn test_extract_enums() {
        let source = r#"
/** Status of the session. */
export enum SessionStatus {
    Active = 'active',
    Closed = 'closed',
    Pending = 'pending',
}
"#;
        let enums = extract_enums(source);
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].0, "SessionStatus");
        assert!(enums[0].1.as_ref().unwrap().contains("Status of the session"));
        assert_eq!(enums[0].2, vec!["Active", "Closed", "Pending"]);
    }

    #[test]
    fn test_is_string_union() {
        assert!(is_string_union("'a' | 'b' | 'c'"));
        assert!(is_string_union("'default' | 'none'"));
        assert!(!is_string_union("string | number"));
        assert!(!is_string_union("BaseTool | BaseToolset"));
    }

    #[test]
    fn test_parse_string_union_variants() {
        let variants = parse_string_union_variants("'default' | 'none'");
        assert_eq!(variants, vec!["default", "none"]);
    }

    #[test]
    fn test_module_from_path() {
        use std::path::PathBuf;
        let source_dir = PathBuf::from("/tmp/adk-js/core/src");
        assert_eq!(
            module_from_path(&source_dir, &PathBuf::from("/tmp/adk-js/core/src/events/event.ts")),
            "events"
        );
        assert_eq!(
            module_from_path(&source_dir, &PathBuf::from("/tmp/adk-js/core/src/models/base_llm.ts")),
            "models"
        );
        assert_eq!(
            module_from_path(&source_dir, &PathBuf::from("/tmp/adk-js/core/src/index.ts")),
            "root"
        );
    }

    #[test]
    fn test_non_agent_non_tool_becomes_type_def() {
        let source = r#"
/** An event in the agent system. */
export interface Event {
    id: string;
    timestamp?: number;
    actions?: EventActions;
}
"#;
        let interfaces = extract_interfaces(source);
        assert_eq!(interfaces.len(), 1);
        let iface = &interfaces[0];

        // Not an agent or tool
        assert!(classify_agent(&iface.name, "event").is_none());
        assert!(!is_tool_interface(&iface.name, "event"));

        // Should be built as a TypeDef
        let type_def = build_type_def_from_interface(iface, "events");
        assert_eq!(type_def.name, "Event");
        assert_eq!(type_def.module, "events");
        assert!(!type_def.is_enum);
        assert!(type_def.fields.len() >= 2);
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
        let interfaces = extract_interfaces(source);
        assert_eq!(interfaces.len(), 1);
        let iface = &interfaces[0];
        let def = build_agent_def(iface, AgentKind::Base);

        assert_eq!(def.name, "BaseAgentConfig");
        assert_eq!(def.kind, AgentKind::Base);
        assert!(def.description.is_some());
        // name, description, parentAgent, subAgents are fields
        assert_eq!(def.fields.len(), 4);
        // beforeAgentCallback, afterAgentCallback are callbacks
        assert_eq!(def.callbacks.len(), 2);
    }

    #[test]
    fn test_full_llm_agent_extraction() {
        let source = r#"
/**
 * The configuration options for creating an LLM-based agent.
 */
export interface LlmAgentConfig extends BaseAgentConfig {
  /**
   * The model to use for the agent.
   */
  model?: string | BaseLlm;

  /** Instructions for the LLM model, guiding the agent's behavior. */
  instruction?: string | InstructionProvider;

  /** Tools available to this agent. */
  tools?: ToolUnion[];

  /**
   * The additional content generation configurations.
   */
  generateContentConfig?: GenerateContentConfig;

  /**
   * Disallows LLM-controlled transferring to the parent agent.
   */
  disallowTransferToParent?: boolean;

  /** Disallows LLM-controlled transferring to the peer agents. */
  disallowTransferToPeers?: boolean;

  /**
   * Controls content inclusion in model requests.
   */
  includeContents?: 'default' | 'none';

  /** The input schema when agent is used as a tool. */
  inputSchema?: LlmAgentSchema;

  /** The output schema when agent replies. */
  outputSchema?: LlmAgentSchema;

  /**
   * The key in session state to store the output of the agent.
   */
  outputKey?: string;

  /**
   * Callbacks to be called before calling the LLM.
   */
  beforeModelCallback?: BeforeModelCallback;

  /**
   * Callbacks to be called after calling the LLM.
   */
  afterModelCallback?: AfterModelCallback;

  /**
   * Callbacks to be called before calling the tool.
   */
  beforeToolCallback?: BeforeToolCallback;

  /**
   * Callbacks to be called after calling the tool.
   */
  afterToolCallback?: AfterToolCallback;

  /**
   * Processors to run before the LLM request is sent.
   */
  requestProcessors?: BaseLlmRequestProcessor[];

  /**
   * Processors to run after the LLM response is received.
   */
  responseProcessors?: BaseLlmResponseProcessor[];

  /**
   * Instructs the agent to make a plan and execute it step by step.
   */
  codeExecutor?: BaseCodeExecutor;
}
"#;
        let interfaces = extract_interfaces(source);
        assert_eq!(interfaces.len(), 1);
        let iface = &interfaces[0];
        let def = build_agent_def(iface, AgentKind::Llm);

        assert_eq!(def.name, "LlmAgentConfig");
        assert_eq!(def.extends.as_deref(), Some("BaseAgentConfig"));
        assert_eq!(def.kind, AgentKind::Llm);

        // Fields: model, instruction, tools, generateContentConfig,
        // disallowTransferToParent, disallowTransferToPeers, includeContents,
        // inputSchema, outputSchema, outputKey, requestProcessors,
        // responseProcessors, codeExecutor = 13 fields
        // Callbacks: beforeModelCallback, afterModelCallback,
        // beforeToolCallback, afterToolCallback = 4 callbacks
        assert!(def.fields.len() >= 12, "Expected >= 12 fields, got {}", def.fields.len());
        assert!(def.callbacks.len() >= 4, "Expected >= 4 callbacks, got {}", def.callbacks.len());
    }
}
