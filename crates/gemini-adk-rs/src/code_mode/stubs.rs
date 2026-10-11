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
}
