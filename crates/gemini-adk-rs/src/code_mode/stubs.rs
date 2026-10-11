//! Python stubs for tool declarations, and naming a call's arguments.
//!
//! A program sees each tool as a Python function. Its stub (signature and
//! docstring) is generated from the tool's function declaration: required
//! parameters first, in the order the declaration requires them, then the
//! optional ones with a `None` default. A call's positional arguments are
//! named in that same order, so `check_availability("2026-10-12", 4)` and
//! `check_availability(date="2026-10-12", party_size=4)` reach the tool alike.

use gemini_genai_rs::prelude::FunctionDeclaration;
use serde_json::{Map, Value};

/// One parameter of a declaration, as a program sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Name, as the declaration spells it.
    pub name: String,
    /// Python type annotation.
    pub annotation: String,
    /// Whether the declaration requires it.
    pub required: bool,
    /// The declaration's description of it, if any.
    pub description: Option<String>,
}

/// The parameters of `declaration` in stub order: required ones first, in
/// the order the declaration requires them, then the optional ones.
pub fn parameters(declaration: &FunctionDeclaration) -> Vec<Parameter> {
    let Some(schema) = declaration.parameters.as_ref() else {
        return Vec::new();
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out: Vec<Parameter> = Vec::new();
    let describe = |name: &str, required: bool| {
        let property = properties.and_then(|p| p.get(name));
        Parameter {
            name: name.to_string(),
            annotation: property.map_or_else(|| "object".to_string(), annotation),
            required,
            description: property
                .and_then(|p| p.get("description"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|d| !d.trim().is_empty()),
        }
    };
    for name in &required {
        if !out.iter().any(|p| p.name == *name) {
            out.push(describe(name, true));
        }
    }
    if let Some(properties) = properties {
        for name in properties.keys() {
            if !required.contains(&name.as_str()) {
                out.push(describe(name, false));
            }
        }
    }
    out
}

/// The Python annotation for a JSON Schema property.
fn annotation(schema: &Value) -> String {
    let kind = schema
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let base = match kind.as_deref() {
        Some("string") => "str".to_string(),
        Some("integer") => "int".to_string(),
        Some("number") => "float".to_string(),
        Some("boolean") => "bool".to_string(),
        Some("array") => match schema.get("items") {
            Some(items) => format!("list[{}]", annotation(items)),
            None => "list".to_string(),
        },
        Some("object") => "dict".to_string(),
        _ => "object".to_string(),
    };
    let nullable = schema.get("nullable").and_then(Value::as_bool) == Some(true);
    if nullable {
        format!("{base} | None")
    } else {
        base
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The Python stub for `declaration`: its signature and a docstring with the
/// tool's description, its parameters' descriptions and, when known, what it
/// returns.
pub fn python_stub(declaration: &FunctionDeclaration, returns: Option<&str>) -> String {
    let params = parameters(declaration);
    let signature = if params.iter().all(|p| is_identifier(&p.name)) {
        params
            .iter()
            .map(|p| {
                if p.required {
                    format!("{}: {}", p.name, p.annotation)
                } else {
                    format!("{}: {} | None = None", p.name, p.annotation)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        // A name Python cannot spell is passed as a keyword through **kwargs.
        "**kwargs".to_string()
    };
    let mut doc: Vec<String> = Vec::new();
    let description = declaration.description.trim();
    if !description.is_empty() {
        doc.push(description.to_string());
    }
    for p in params.iter().filter(|p| p.description.is_some()) {
        doc.push(format!(
            "{}: {}",
            p.name,
            p.description.as_deref().unwrap_or_default()
        ));
    }
    match returns.map(str::trim).filter(|r| !r.is_empty()) {
        Some(returns) => doc.push(format!("Returns {returns}")),
        None => doc.push("Returns the tool's JSON result.".to_string()),
    }
    format!(
        "def {}({signature}) -> dict:\n    \"\"\"{}\"\"\"",
        declaration.name,
        doc.join("\n    ").replace("\"\"\"", "'''")
    )
}

/// Name a call's arguments for `declaration`: positional values in stub
/// order, then the keywords. The errors are Python's, for the program.
pub fn arguments(
    declaration: &FunctionDeclaration,
    positional: Vec<Value>,
    keywords: Map<String, Value>,
) -> Result<Value, String> {
    let params = parameters(declaration);
    let name = &declaration.name;
    if positional.len() > params.len() {
        return Err(format!(
            "TypeError: {name}() takes {} positional argument{} but {} were given",
            params.len(),
            if params.len() == 1 { "" } else { "s" },
            positional.len()
        ));
    }
    let mut args = Map::new();
    for (param, value) in params.iter().zip(positional) {
        args.insert(param.name.clone(), value);
    }
    for (key, value) in keywords {
        if args.contains_key(&key) {
            return Err(format!(
                "TypeError: {name}() got multiple values for argument '{key}'"
            ));
        }
        args.insert(key, value);
    }
    // An optional parameter passed as None is left out, as the tool would
    // see it in a direct call that omits it.
    args.retain(|key, value| {
        !(value.is_null() && params.iter().any(|p| p.name == *key && !p.required))
    });
    let missing: Vec<&str> = params
        .iter()
        .filter(|p| p.required && !args.contains_key(&p.name))
        .map(|p| p.name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "TypeError: {name}() missing required argument{}: {}",
            if missing.len() == 1 { "" } else { "s" },
            missing
                .iter()
                .map(|m| format!("'{m}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(Value::Object(args))
}

/// `source` without its own stand-ins for the session's `functions`.
///
/// Shown Python stubs, a model sometimes copies them into its program
/// (`def check_availability(date, party_size): pass`) or imports them
/// (`from tools import check_availability`). Either shadows the real
/// function, so every call returns `None` and the program fails far from the
/// cause. A definition whose body does nothing (`pass`, `...`, a docstring)
/// and an import of only these names are removed; any other definition or
/// assignment of one of them is refused with the reason.
pub fn without_redefinitions(source: &str, functions: &[String]) -> Result<String, String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if let Some(name) = defined_name(trimmed).filter(|n| functions.iter().any(|f| f == n)) {
            // A signature with no end is left for the interpreter to report.
            let Some((end, inline)) = signature_end(&lines, i) else {
                kept.push(line);
                i += 1;
                continue;
            };
            let (body, next) = if strip_comment(inline).trim().is_empty() {
                let mut next = end + 1;
                while let Some(following) = lines.get(next) {
                    let following_indent = following.len() - following.trim_start().len();
                    if following.trim().is_empty() || following_indent > indent {
                        next += 1;
                    } else {
                        break;
                    }
                }
                (lines[end + 1..next].join("\n"), next)
            } else {
                // `def f(x): pass` on one line.
                (inline.to_string(), end + 1)
            };
            if !does_nothing(&body) {
                return Err(redefined(name));
            }
            i = next;
            continue;
        }
        if let Some(imported) = imported_names(trimmed)
            && imported.iter().all(|n| functions.iter().any(|f| f == n))
        {
            i += 1;
            continue;
        }
        if let Some(name) = assigned_name(trimmed).filter(|n| functions.iter().any(|f| f == n)) {
            return Err(redefined(name));
        }
        kept.push(line);
        i += 1;
    }
    Ok(kept.join("\n"))
}

/// Where the signature of the `def` on line `start` ends: the line holding
/// its closing `:` (the first outside brackets) and the text after it, an
/// inline body. `None` when the program ends first.
fn signature_end<'a>(lines: &[&'a str], start: usize) -> Option<(usize, &'a str)> {
    let mut depth = 0i32;
    for (index, line) in lines.iter().enumerate().skip(start) {
        let code = strip_comment(line);
        for (at, c) in code.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ':' if depth == 0 => return Some((index, &line[at + 1..])),
                _ => {}
            }
        }
    }
    None
}

fn redefined(name: &str) -> String {
    format!(
        "The program defines '{name}', which is already defined: it calls the \
         session's tool. Delete your definition and call {name}(...) directly."
    )
}

/// The name in `def name(`.
fn defined_name(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("def ")
        .or_else(|| line.strip_prefix("async def "))?;
    let name = rest.split('(').next()?.trim();
    is_identifier(name).then_some(name)
}

/// The names in `from module import a, b as c` (the local names).
fn imported_names(line: &str) -> Option<Vec<&str>> {
    let rest = line.strip_prefix("from ")?;
    let (_, names) = rest.split_once(" import ")?;
    let names = names.trim().trim_start_matches('(').trim_end_matches(')');
    Some(
        names
            .split(',')
            .map(|n| n.split(" as ").last().unwrap_or(n).trim())
            .filter(|n| !n.is_empty())
            .collect(),
    )
}

/// The name in `name = ...` (not `==`).
fn assigned_name(line: &str) -> Option<&str> {
    let (name, rest) = line.split_once('=')?;
    if rest.starts_with('=') {
        return None;
    }
    let name = name.trim();
    is_identifier(name).then_some(name)
}

fn strip_comment(line: &str) -> &str {
    line.split('#').next().unwrap_or(line)
}

/// Whether a function body does nothing: only `pass`, `...`, `return`,
/// `return None`, docstrings and comments.
fn does_nothing(body: &str) -> bool {
    let mut text = body.to_string();
    for quote in ["\"\"\"", "\'\'\'"] {
        while let Some(start) = text.find(quote) {
            let Some(len) = text[start + 3..].find(quote) else {
                return false;
            };
            text.replace_range(start..start + 3 + len + 3, "");
        }
    }
    text.lines()
        .map(|l| strip_comment(l).trim())
        .filter(|l| !l.is_empty())
        .all(|l| matches!(l, "pass" | "..." | "return" | "return None"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn availability() -> FunctionDeclaration {
        FunctionDeclaration {
            name: "check_availability".into(),
            description: "Open table times.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "party_size": {"type": "integer"},
                    "date": {"type": "string", "description": "YYYY-MM-DD"},
                    "seating": {"type": "string"},
                },
                "required": ["date", "party_size"],
            })),
            behavior: None,
        }
    }

    #[test]
    fn a_stub_puts_required_parameters_first_in_their_declared_order() {
        let stub = python_stub(&availability(), Some(r#"{"times": [str]}"#));
        assert_eq!(
            stub,
            "def check_availability(date: str, party_size: int, seating: str | None = None) -> dict:\n    \
             \"\"\"Open table times.\n    date: YYYY-MM-DD\n    Returns {\"times\": [str]}\"\"\""
        );
    }

    #[test]
    fn positional_arguments_are_named_in_stub_order() {
        let args = arguments(
            &availability(),
            vec![json!("2026-10-12"), json!(4)],
            Map::new(),
        )
        .unwrap();
        assert_eq!(args, json!({"date": "2026-10-12", "party_size": 4}));
    }

    #[test]
    fn argument_errors_read_as_python_type_errors() {
        let mut keywords = Map::new();
        keywords.insert("date".into(), json!("2026-10-12"));
        let twice = arguments(&availability(), vec![json!("2026-10-12")], keywords);
        assert_eq!(
            twice.unwrap_err(),
            "TypeError: check_availability() got multiple values for argument 'date'"
        );
        let missing = arguments(&availability(), vec![json!("2026-10-12")], Map::new());
        assert_eq!(
            missing.unwrap_err(),
            "TypeError: check_availability() missing required argument: 'party_size'"
        );
        let many = arguments(
            &availability(),
            vec![json!(1), json!(2), json!(3), json!(4)],
            Map::new(),
        );
        assert!(
            many.unwrap_err()
                .contains("takes 3 positional arguments but 4")
        );
    }

    #[test]
    fn an_optional_argument_passed_as_none_is_left_out() {
        let mut keywords = Map::new();
        keywords.insert("seating".into(), Value::Null);
        let args = arguments(&availability(), vec![json!("d"), json!(2)], keywords).unwrap();
        assert_eq!(args, json!({"date": "d", "party_size": 2}));
    }

    #[test]
    fn a_name_python_cannot_spell_goes_through_kwargs() {
        let declaration = FunctionDeclaration {
            name: "lookup".into(),
            description: String::new(),
            parameters: Some(json!({
                "type": "object",
                "properties": {"account-id": {"type": "STRING"}},
            })),
            behavior: None,
        };
        assert!(python_stub(&declaration, None).starts_with("def lookup(**kwargs) -> dict:"));
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_copied_stub_or_import_is_removed() {
        let source = "import json\nfrom tools import check_availability\n\
                      def check_availability(date: str,\n        party_size: int) -> dict:\n    \
                      \"\"\"Open table times.\"\"\"\n    pass  # defined by the system\n\n\
                      times = check_availability('2026-10-12', 4)\ntimes";
        let cleaned = without_redefinitions(source, &names(&["check_availability"])).unwrap();
        assert_eq!(
            cleaned,
            "import json\ntimes = check_availability('2026-10-12', 4)\ntimes"
        );
    }

    #[test]
    fn a_real_redefinition_is_refused() {
        let wraps = "def check_availability(date, party_size):\n    return {'times': []}\n";
        let err = without_redefinitions(wraps, &names(&["check_availability"])).unwrap_err();
        assert!(err.contains("Delete your definition"), "{err}");
        let mock = "check_availability = lambda d, p: {}\n";
        assert!(without_redefinitions(mock, &names(&["check_availability"])).is_err());
        // Other definitions and comparisons stay.
        let fine = "def helper(x):\n    return x\nok = 1 == 1\nhelper(2)";
        assert_eq!(
            without_redefinitions(fine, &names(&["check_availability"])).unwrap(),
            fine
        );
    }

    #[test]
    fn a_one_line_stub_is_removed_and_an_unfinished_one_left_alone() {
        let one_line = "def lookup(account_id: str) -> dict: pass  # the system's\nlookup('A1')";
        assert_eq!(
            without_redefinitions(one_line, &names(&["lookup"])).unwrap(),
            "lookup('A1')"
        );
        // From a live run: a stub whose signature never closes.
        let broken = "print(import json\ndef lookup(account_id: str -> dict\nlookup('A1')";
        assert_eq!(
            without_redefinitions(broken, &names(&["lookup"])).unwrap(),
            broken
        );
    }

    proptest::proptest! {
        #[test]
        fn no_program_text_makes_it_panic(source in "(def lookup|lookup|from t import lookup|[ ():\\n#'\"=.a-z]){0,40}") {
            let _ = without_redefinitions(&source, &names(&["lookup"]));
        }
    }
}
