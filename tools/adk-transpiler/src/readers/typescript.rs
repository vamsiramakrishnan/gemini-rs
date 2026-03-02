//! Shared TypeScript source parsing utilities.
//!
//! Functions for extracting interfaces, enums, type aliases, fields,
//! and JSDoc comments from TypeScript source files.

use regex::Regex;

use crate::schema::{CallbackDef, FieldDef};

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

// ---------------------------------------------------------------------------
// Raw interface type
// ---------------------------------------------------------------------------

/// A raw interface parsed from TypeScript source.
#[derive(Debug)]
pub struct RawInterface {
    pub name: String,
    pub extends: Option<String>,
    pub jsdoc: Option<String>,
    pub body: String,
}

// ---------------------------------------------------------------------------
// Interface extraction
// ---------------------------------------------------------------------------

/// Extract all `export interface Foo extends Bar { ... }` blocks from source.
pub fn extract_interfaces(source: &str) -> Vec<RawInterface> {
    let mut results = Vec::new();

    let iface_re = Regex::new(
        r"(?m)(?:^|\n)\s*(?:export\s+)?(?:declare\s+)?interface\s+(\w+)(?:\s+extends\s+(\w+))?\s*\{"
    ).unwrap();

    for cap in iface_re.captures_iter(source) {
        let name = cap[1].to_string();
        let extends = cap.get(2).map(|m| m.as_str().to_string());
        let match_start = cap.get(0).unwrap().start();

        let jsdoc = extract_preceding_jsdoc(source, match_start);

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
pub fn extract_type_aliases(source: &str) -> Vec<(String, String)> {
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
pub fn extract_enums(source: &str) -> Vec<(String, Option<String>, Vec<String>)> {
    let mut results = Vec::new();

    let enum_re = Regex::new(
        r"(?m)(?:^|\n)\s*(?:export\s+)?(?:declare\s+)?(?:const\s+)?enum\s+(\w+)\s*\{"
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
                .map(|s| {
                    if let Some(eq_pos) = s.find('=') {
                        s[..eq_pos].trim().to_string()
                    } else {
                        s.to_string()
                    }
                })
                .filter(|s| !s.starts_with("//") && !s.starts_with("/*") && !s.is_empty())
                .collect();

            if !variants.is_empty() {
                results.push((name, jsdoc, variants));
            }
        }
    }

    results
}

/// Extract `export function name(...)` declarations.
pub fn extract_helper_functions(source: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let func_re = Regex::new(
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

// ---------------------------------------------------------------------------
// Field extraction from interface body
// ---------------------------------------------------------------------------

/// Parse fields from an interface body string.
pub fn parse_fields(body: &str) -> (Vec<FieldDef>, Vec<CallbackDef>) {
    let mut fields = Vec::new();
    let mut callbacks = Vec::new();

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

// ---------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------

/// Map a TypeScript type string to an approximate Rust type.
pub fn map_ts_to_rust(ts_type: &str) -> String {
    let ts = ts_type.trim();

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

    // Union types with classes
    if ts.contains('|') {
        let parts: Vec<&str> = ts.split('|').map(|s| s.trim()).collect();
        if parts.contains(&"string") {
            return "String".to_string();
        }
        if let Some(arr) = parts.iter().find(|p| p.ends_with("[]")) {
            return map_ts_to_rust(arr);
        }
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
            if ts.chars().next().is_some_and(|c| c.is_uppercase()) {
                ts.to_string()
            } else {
                format!("/* {} */ String", ts)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check if a TypeScript type name refers to a callback.
fn is_callback_type(ts_type: &str) -> bool {
    let normalized = ts_type.trim().trim_end_matches("[]");

    if normalized.contains('|') {
        let parts: Vec<&str> = normalized.split('|').map(|s| s.trim()).collect();
        return parts.iter().all(|p| {
            let p = p.trim_end_matches("[]");
            CALLBACK_TYPE_NAMES.contains(&p) || p.contains("=>")
        });
    }

    for &cb in CALLBACK_TYPE_NAMES {
        if normalized == cb {
            return true;
        }
    }

    if normalized.contains("=>") {
        return true;
    }
    false
}

/// Check if a type alias value is a string literal union.
pub fn is_string_union(value: &str) -> bool {
    let parts: Vec<&str> = value.split('|').map(|s| s.trim()).collect();
    parts.len() >= 2 && parts.iter().all(|p| {
        let p = p.trim();
        (p.starts_with('\'') && p.ends_with('\''))
            || (p.starts_with('"') && p.ends_with('"'))
    })
}

/// Parse string union variants from a type alias value.
pub fn parse_string_union_variants(value: &str) -> Vec<String> {
    value
        .split('|')
        .map(|s| s.trim())
        .map(|s| s.trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract the JSDoc block comment preceding a given byte position.
pub fn extract_preceding_jsdoc(source: &str, pos: usize) -> Option<String> {
    let before = &source[..pos];
    let trimmed = before.trim_end();
    if trimmed.ends_with("*/") {
        if let Some(start) = trimmed.rfind("/**") {
            let comment = &trimmed[start..];
            let cleaned = clean_jsdoc(comment);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    None
}

/// Clean a JSDoc comment into plain text.
pub fn clean_jsdoc(comment: &str) -> String {
    let lines: Vec<&str> = comment.lines().collect();
    let mut result = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "/**" || trimmed == "*/" {
            continue;
        }
        let trimmed = if let Some(rest) = trimmed.strip_prefix("/**") {
            if let Some(inner) = rest.strip_suffix("*/") {
                inner.trim()
            } else {
                rest.trim()
            }
        } else {
            trimmed
        };
        let content = if let Some(rest) = trimmed.strip_prefix("* ") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix('*') {
            rest
        } else {
            trimmed
        };
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

/// Extract the content between matching braces starting at `start`.
pub fn extract_brace_block(source: &str, start: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut depth = 1i32;
    let mut i = start;
    while i < len && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b'/' if i + 1 < len && bytes[i + 1] == b'/' => {
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < len {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'"' | b'`' => {
                let quote = bytes[i];
                i += 1;
                while i < len && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
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
                    i = j;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if depth == 0 {
        Some(source[start..i - 1].to_string())
    } else {
        None
    }
}

/// Produce a simple ISO 8601 timestamp without pulling in the chrono crate.
pub fn chrono_like_now() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let days = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

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

/// Determine the module name from a file path relative to the source directory.
pub fn module_from_path(source_dir: &std::path::Path, file_path: &std::path::Path) -> String {
    if let Ok(relative) = file_path.strip_prefix(source_dir) {
        if let Some(first) = relative.components().next() {
            let first_str = first.as_os_str().to_str().unwrap_or("root");
            if relative.components().count() == 1 {
                return "root".to_string();
            }
            return first_str.to_string();
        }
    }
    "root".to_string()
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
        assert_eq!(fields[0].rust_type, "String");
        assert!(!fields[0].optional);
        assert_eq!(fields[1].name, "description");
        assert!(fields[1].optional);
        assert_eq!(fields[2].name, "subAgents");
        assert_eq!(fields[2].rust_type, "Vec<AgentRef>");
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
        assert_eq!(enums[0].2, vec!["Active", "Closed", "Pending"]);
    }

    #[test]
    fn test_is_string_union() {
        assert!(is_string_union("'a' | 'b' | 'c'"));
        assert!(!is_string_union("string | number"));
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
            module_from_path(&source_dir, &PathBuf::from("/tmp/adk-js/core/src/index.ts")),
            "root"
        );
    }
}
