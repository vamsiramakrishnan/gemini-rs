//! A strict YAML subset for OKF front matter.
//!
//! The engine both writes and reads this front matter, so it needs exactly the
//! shape it emits — block mappings, block sequences of scalars, and scalars —
//! and nothing else. Implementing that subset directly (rather than pulling a
//! general YAML parser) keeps the canonical format dependency-free and lets
//! parse failures name the offending line.
//!
//! Supported:
//!
//! ```yaml
//! key: scalar          # comments, after a value or on their own line
//! nested:
//!   key: value
//! list:
//!   - item
//!   - item
//! empty_list: []
//! quoted: "a: value with punctuation"
//! nothing: null
//! ```
//!
//! Not supported (and rejected with a diagnostic): flow mappings, anchors,
//! aliases, multi-line scalars, tabs for indentation, and documents whose root
//! is not a mapping.

use std::fmt::Write as _;

/// A parsed YAML-subset value.
#[derive(Debug, Clone, PartialEq)]
pub enum Yaml {
    /// `null`, `~`, or an empty value.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A floating-point number.
    Float(f64),
    /// A string, quoted or bare.
    Str(String),
    /// A block sequence.
    Seq(Vec<Yaml>),
    /// A block mapping. Order is preserved so emitted files are stable.
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    /// Look up a key in a mapping.
    pub fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Self::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Borrow as a string, accepting any scalar and rendering it.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Read a string, or `None` for null/missing.
    pub fn as_string(&self) -> Option<String> {
        match self {
            Self::Str(s) => Some(s.clone()),
            Self::Bool(b) => Some(b.to_string()),
            Self::Int(i) => Some(i.to_string()),
            Self::Float(f) => Some(f.to_string()),
            _ => None,
        }
    }

    /// Read a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Read a number as `f64`, accepting integers.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            Self::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// Read a number as `u64`.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Int(i) if *i >= 0 => Some(*i as u64),
            Self::Float(f) if *f >= 0.0 => Some(*f as u64),
            _ => None,
        }
    }

    /// Borrow as a sequence.
    pub fn as_seq(&self) -> Option<&[Yaml]> {
        match self {
            Self::Seq(items) => Some(items),
            _ => None,
        }
    }

    /// Read a sequence of strings, tolerating a missing or null value.
    pub fn as_string_list(&self) -> Vec<String> {
        match self {
            Self::Seq(items) => items.iter().filter_map(Yaml::as_string).collect(),
            Self::Null => Vec::new(),
            other => other.as_string().into_iter().collect(),
        }
    }

    /// Whether the value is null.
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl From<&str> for Yaml {
    fn from(value: &str) -> Self {
        Yaml::Str(value.to_string())
    }
}

impl From<String> for Yaml {
    fn from(value: String) -> Self {
        Yaml::Str(value)
    }
}

impl From<bool> for Yaml {
    fn from(value: bool) -> Self {
        Yaml::Bool(value)
    }
}

impl From<u32> for Yaml {
    fn from(value: u32) -> Self {
        Yaml::Int(i64::from(value))
    }
}

impl From<u64> for Yaml {
    fn from(value: u64) -> Self {
        Yaml::Int(value as i64)
    }
}

impl From<f32> for Yaml {
    fn from(value: f32) -> Self {
        Yaml::Float(f64::from(value))
    }
}

/// A YAML-subset parse failure, with the line it occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError {
    /// 1-based line number.
    pub line: usize,
    /// What went wrong.
    pub message: String,
}

impl std::fmt::Display for YamlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for YamlError {}

/// One significant (non-blank, non-comment) input line.
struct Line {
    number: usize,
    indent: usize,
    content: String,
}

/// Parse a YAML-subset document. The root must be a mapping.
pub fn parse(input: &str) -> Result<Yaml, YamlError> {
    let lines = significant_lines(input)?;
    if lines.is_empty() {
        return Ok(Yaml::Map(Vec::new()));
    }
    let mut cursor = 0usize;
    let base = lines[0].indent;
    let value = parse_block(&lines, &mut cursor, base)?;
    if cursor < lines.len() {
        return Err(YamlError {
            line: lines[cursor].number,
            message: "unexpected dedent — inconsistent indentation".to_string(),
        });
    }
    match value {
        Yaml::Map(_) => Ok(value),
        _ => Err(YamlError {
            line: lines[0].number,
            message: "OKF front matter must be a mapping".to_string(),
        }),
    }
}

fn significant_lines(input: &str) -> Result<Vec<Line>, YamlError> {
    let mut out = Vec::new();
    for (idx, raw) in input.lines().enumerate() {
        let number = idx + 1;
        if raw.contains('\t') && raw.trim_start().len() != raw.len() {
            return Err(YamlError {
                line: number,
                message: "tabs may not be used for indentation".to_string(),
            });
        }
        let trimmed = raw.trim_end();
        let indent = trimmed.len() - trimmed.trim_start().len();
        let content = trimmed.trim_start().to_string();
        if content.is_empty() || content.starts_with('#') || content == "---" {
            continue;
        }
        out.push(Line {
            number,
            indent,
            content,
        });
    }
    Ok(out)
}

fn parse_block(lines: &[Line], cursor: &mut usize, indent: usize) -> Result<Yaml, YamlError> {
    if *cursor >= lines.len() {
        return Ok(Yaml::Null);
    }
    if lines[*cursor].content.starts_with("- ") || lines[*cursor].content == "-" {
        parse_sequence(lines, cursor, indent)
    } else {
        parse_mapping(lines, cursor, indent)
    }
}

fn parse_sequence(lines: &[Line], cursor: &mut usize, indent: usize) -> Result<Yaml, YamlError> {
    let mut items = Vec::new();
    while *cursor < lines.len() && lines[*cursor].indent == indent {
        let line = &lines[*cursor];
        let Some(rest) = line
            .content
            .strip_prefix("- ")
            .or_else(|| (line.content == "-").then_some(""))
        else {
            break;
        };
        *cursor += 1;
        let scalar = strip_comment(rest.trim());
        if scalar.is_empty() {
            // A nested block under a bare `-`.
            if *cursor < lines.len() && lines[*cursor].indent > indent {
                let child_indent = lines[*cursor].indent;
                items.push(parse_block(lines, cursor, child_indent)?);
            } else {
                items.push(Yaml::Null);
            }
        } else {
            items.push(parse_scalar(scalar));
        }
    }
    Ok(Yaml::Seq(items))
}

fn parse_mapping(lines: &[Line], cursor: &mut usize, indent: usize) -> Result<Yaml, YamlError> {
    let mut entries: Vec<(String, Yaml)> = Vec::new();
    while *cursor < lines.len() && lines[*cursor].indent == indent {
        let line = &lines[*cursor];
        if line.content.starts_with("- ") {
            break;
        }
        let (key, rest) = split_key(&line.content).ok_or_else(|| YamlError {
            line: line.number,
            message: format!("expected `key: value`, found `{}`", line.content),
        })?;
        let line_number = line.number;
        *cursor += 1;

        let inline = strip_comment(rest.trim());
        let value = if inline.is_empty() {
            if *cursor < lines.len() && lines[*cursor].indent > indent {
                let child_indent = lines[*cursor].indent;
                parse_block(lines, cursor, child_indent)?
            } else {
                Yaml::Null
            }
        } else {
            parse_scalar(inline)
        };

        if entries.iter().any(|(k, _)| k == &key) {
            return Err(YamlError {
                line: line_number,
                message: format!("duplicate key `{key}`"),
            });
        }
        entries.push((key, value));
    }
    Ok(Yaml::Map(entries))
}

/// Split `key: value`, respecting quoted keys and not splitting inside quotes.
fn split_key(content: &str) -> Option<(String, &str)> {
    let bytes = content.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if *b == q => quote = None,
            Some(_) => {}
            None if *b == b'"' || *b == b'\'' => quote = Some(*b),
            None if *b == b':' => {
                let is_last = i + 1 == bytes.len();
                if is_last || bytes[i + 1] == b' ' {
                    let key = unquote(content[..i].trim());
                    return Some((key, &content[i + 1..]));
                }
            }
            None => {}
        }
    }
    None
}

/// Remove a trailing `# comment` when it is not inside quotes.
fn strip_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if *b == q => quote = None,
            Some(_) => {}
            None if *b == b'"' || *b == b'\'' => quote = Some(*b),
            None if *b == b'#' && (i == 0 || bytes[i - 1] == b' ') => {
                return value[..i].trim_end();
            }
            None => {}
        }
    }
    value
}

fn unquote(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        raw[1..raw.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\n", "\n")
    } else {
        raw.to_string()
    }
}

fn parse_scalar(raw: &str) -> Yaml {
    if raw == "[]" {
        return Yaml::Seq(Vec::new());
    }
    if raw == "{}" {
        return Yaml::Map(Vec::new());
    }
    let bytes = raw.as_bytes();
    let quoted = bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''));
    if quoted {
        return Yaml::Str(unquote(raw));
    }
    match raw {
        "null" | "~" | "" => Yaml::Null,
        "true" => Yaml::Bool(true),
        "false" => Yaml::Bool(false),
        _ => {
            if let Ok(i) = raw.parse::<i64>() {
                Yaml::Int(i)
            } else if let Ok(f) = raw.parse::<f64>() {
                Yaml::Float(f)
            } else {
                Yaml::Str(raw.to_string())
            }
        }
    }
}

/// Render a value as YAML-subset text. The root must be a mapping.
pub fn emit(value: &Yaml) -> String {
    let mut out = String::new();
    emit_into(value, 0, &mut out);
    out
}

fn emit_into(value: &Yaml, indent: usize, out: &mut String) {
    match value {
        Yaml::Map(entries) => {
            for (key, child) in entries {
                let pad = " ".repeat(indent);
                match child {
                    Yaml::Map(inner) if inner.is_empty() => {
                        let _ = writeln!(out, "{pad}{key}: {{}}");
                    }
                    Yaml::Seq(items) if items.is_empty() => {
                        let _ = writeln!(out, "{pad}{key}: []");
                    }
                    Yaml::Map(_) | Yaml::Seq(_) => {
                        let _ = writeln!(out, "{pad}{key}:");
                        emit_into(child, indent + 2, out);
                    }
                    scalar => {
                        let _ = writeln!(out, "{pad}{key}: {}", emit_scalar(scalar));
                    }
                }
            }
        }
        Yaml::Seq(items) => {
            for item in items {
                let pad = " ".repeat(indent);
                match item {
                    Yaml::Map(_) | Yaml::Seq(_) => {
                        let _ = writeln!(out, "{pad}-");
                        emit_into(item, indent + 2, out);
                    }
                    scalar => {
                        let _ = writeln!(out, "{pad}- {}", emit_scalar(scalar));
                    }
                }
            }
        }
        scalar => {
            let pad = " ".repeat(indent);
            let _ = writeln!(out, "{pad}{}", emit_scalar(scalar));
        }
    }
}

fn emit_scalar(value: &Yaml) -> String {
    match value {
        Yaml::Null => "null".to_string(),
        Yaml::Bool(b) => b.to_string(),
        Yaml::Int(i) => i.to_string(),
        Yaml::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() {
                format!("{f:.1}")
            } else {
                format!("{f}")
            }
        }
        Yaml::Str(s) => quote_if_needed(s),
        // Nested collections are handled by `emit_into`; reaching here means an
        // empty collection was inlined.
        Yaml::Seq(_) => "[]".to_string(),
        Yaml::Map(_) => "{}".to_string(),
    }
}

/// Quote a string when leaving it bare would change how it re-parses.
fn quote_if_needed(raw: &str) -> String {
    let needs_quotes = raw.is_empty()
        || raw.trim() != raw
        || raw.contains(": ")
        || raw.ends_with(':')
        || raw.contains('\n')
        || raw.contains(" #")
        || raw.starts_with([
            '-', '?', '&', '*', '!', '|', '>', '%', '@', '`', '[', '{', '"', '\'',
        ])
        || matches!(raw, "null" | "true" | "false" | "~")
        || raw.parse::<f64>().is_ok();
    if needs_quotes {
        format!("\"{}\"", raw.replace('"', "\\\"").replace('\n', "\\n"))
    } else {
        raw.to_string()
    }
}

/// Build a mapping from an ordered list of entries, dropping `None` values.
pub fn map(entries: Vec<(&str, Option<Yaml>)>) -> Yaml {
    Yaml::Map(
        entries
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k.to_string(), v)))
            .collect(),
    )
}

/// Build a sequence of strings.
pub fn seq_of_strings<I, S>(items: I) -> Yaml
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    Yaml::Seq(items.into_iter().map(|s| Yaml::Str(s.into())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_okf_front_matter_shape() {
        let src = r#"
okf: memory/v1
id: mem_01K4D8P3
confidence: 1.0
source:
  type: explicit_user_statement
  turn_id: turn_17
retrieval:
  tags:
    - food
    - diet
  aliases: []
privacy:
  deletable: true
  sensitivity: normal
valid_to: null
"#;
        let parsed = parse(src).unwrap();
        assert_eq!(parsed.get("okf").unwrap().as_str(), Some("memory/v1"));
        assert_eq!(parsed.get("confidence").unwrap().as_f64(), Some(1.0));
        assert_eq!(
            parsed
                .get("source")
                .unwrap()
                .get("turn_id")
                .unwrap()
                .as_str(),
            Some("turn_17")
        );
        assert_eq!(
            parsed
                .get("retrieval")
                .unwrap()
                .get("tags")
                .unwrap()
                .as_string_list(),
            vec!["food", "diet"]
        );
        assert_eq!(
            parsed
                .get("retrieval")
                .unwrap()
                .get("aliases")
                .unwrap()
                .as_seq(),
            Some(&[][..])
        );
        assert_eq!(
            parsed
                .get("privacy")
                .unwrap()
                .get("deletable")
                .unwrap()
                .as_bool(),
            Some(true)
        );
        assert!(parsed.get("valid_to").unwrap().is_null());
    }

    #[test]
    fn round_trips_through_emit_and_parse() {
        let original = map(vec![
            ("okf", Some("memory/v1".into())),
            ("confidence", Some(Yaml::Float(0.85))),
            (
                "retrieval",
                Some(map(vec![
                    ("tags", Some(seq_of_strings(["food", "diet"]))),
                    ("aliases", Some(seq_of_strings(Vec::<String>::new()))),
                ])),
            ),
            ("valid_to", Some(Yaml::Null)),
        ]);
        let text = emit(&original);
        assert_eq!(parse(&text).unwrap(), original);
    }

    #[test]
    fn strings_that_would_re_parse_as_other_types_are_quoted() {
        let value = map(vec![
            ("a", Some("true".into())),
            ("b", Some("2026".into())),
            ("c", Some("a: b".into())),
            ("d", Some("- leading dash".into())),
        ]);
        let text = emit(&value);
        let reparsed = parse(&text).unwrap();
        assert_eq!(reparsed.get("a").unwrap().as_str(), Some("true"));
        assert_eq!(reparsed.get("b").unwrap().as_str(), Some("2026"));
        assert_eq!(reparsed.get("c").unwrap().as_str(), Some("a: b"));
        assert_eq!(reparsed.get("d").unwrap().as_str(), Some("- leading dash"));
    }

    #[test]
    fn comments_are_ignored_but_hashes_inside_quotes_survive() {
        let src = "# leading comment\nkey: value # trailing\nquoted: \"has # hash\"\n";
        let parsed = parse(src).unwrap();
        assert_eq!(parsed.get("key").unwrap().as_str(), Some("value"));
        assert_eq!(parsed.get("quoted").unwrap().as_str(), Some("has # hash"));
    }

    #[test]
    fn timestamps_stay_strings() {
        let parsed = parse("created_at: 2026-07-26T09:12:14Z\n").unwrap();
        assert_eq!(
            parsed.get("created_at").unwrap().as_str(),
            Some("2026-07-26T09:12:14Z")
        );
    }

    #[test]
    fn tabs_and_duplicate_keys_are_rejected_with_a_line_number() {
        let tabbed = parse("a:\n\tb: 1\n").unwrap_err();
        assert_eq!(tabbed.line, 2);

        let duped = parse("a: 1\na: 2\n").unwrap_err();
        assert_eq!(duped.line, 2);
        assert!(duped.message.contains("duplicate"));
    }

    #[test]
    fn a_non_mapping_root_is_rejected() {
        assert!(parse("- just\n- a list\n").is_err());
    }

    #[test]
    fn nested_maps_inside_sequences_round_trip() {
        let value = Yaml::Map(vec![(
            "items".to_string(),
            Yaml::Seq(vec![
                Yaml::Map(vec![("name".into(), "a".into())]),
                Yaml::Map(vec![("name".into(), "b".into())]),
            ]),
        )]);
        let text = emit(&value);
        assert_eq!(parse(&text).unwrap(), value);
    }
}
