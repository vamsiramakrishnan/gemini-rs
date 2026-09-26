//! One schema pipeline from Rust types to the Gemini wire format.

use schemars::JsonSchema;

/// Derive a JSON Schema in the shape the Gemini API will actually enforce.
///
/// This is the one way a Rust type becomes a schema on the wire: `#[tool]`
/// parameters, [`TypedTool`](super::TypedTool) arguments, typed agent output
/// and turn extraction all go through it. Derive `JsonSchema` on the type
/// (doc comments on fields become descriptions) and call this instead of
/// `schemars::schema_for!`, whose output the API misreads in the ways below.
///
/// ```
/// use gemini_adk_rs::tool::wire_schema;
///
/// #[derive(schemars::JsonSchema)]
/// #[allow(dead_code)]
/// struct Lookup {
///     /// The city to look up.
///     city: String,
///     units: Option<String>,
/// }
///
/// let schema = wire_schema::<Lookup>();
/// assert_eq!(schema["properties"]["city"]["description"], "The city to look up.");
/// assert_eq!(schema["properties"]["units"]["type"], "string"); // not ["string", "null"]
/// assert_eq!(schema["required"], serde_json::json!(["city"]));
/// ```
///
/// `schemars::schema_for!` hoists every nested type into `definitions` and
/// points at it with `$ref`. The API does not resolve those references — it
/// ignores them, silently. A declaration carrying `$ref` therefore degrades to
/// "send some JSON": enum constraints stop applying and the model invents
/// variants the type cannot deserialize. On the Live endpoint the failure is
/// harsher still — the server closes the connection during setup rather than
/// accepting the declaration.
///
/// So subschemas are inlined and `$schema`/`definitions` are stripped, leaving
/// nothing that points outside the document. A schema that is ignored is worse
/// than one that is absent: it reads as a constraint and behaves like free-form
/// generation.
///
/// The result is then narrowed to the API's schema subset, which draft-07 is
/// broader than in ways that matter: a nullable union collapses to its one
/// type, and a `oneOf` over single-variant enums flattens into one `enum`.
pub fn wire_schema<T: JsonSchema + ?Sized>() -> serde_json::Value {
    let settings = schemars::r#gen::SchemaSettings::draft07().with(|s| {
        s.inline_subschemas = true;
        s.meta_schema = None;
    });
    let root = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(root).expect("schemars schema should serialize to JSON");
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
        object.remove("definitions");
    }
    narrow_to_api_subset(&mut value);
    value
}

/// Rewrite draft-07 constructs the Gemini schema subset cannot express.
///
/// Three rewrites:
///
/// 1. **Nullable unions are collapsed.** `Option<String>` derives
///    `"type": ["string", "null"]`, but the API's `Schema.type` is a single
///    enum value, not a list. It does not ignore the list — it rejects the
///    whole request (`Unknown name "type"`), which on the Live endpoint means
///    the server closes the connection mid-handshake and the session never
///    comes up. Optionality is already carried by absence from `required`, so
///    dropping `"null"` loses nothing.
///
/// 2. **`oneOf` over single-variant enums is flattened** into one `type:
///    string` with the variants in `enum`. That is how a fieldless Rust enum
///    derives, and while the API tolerates the `oneOf` form it does not
///    understand it — so the variant constraint quietly stops applying and the
///    model is free to invent values that will not deserialize. Flattening
///    restores the constraint the type already declared.
///
/// 3. **A nullable composite collapses to the composite.** `Option<T>` for a
///    struct or enum `T` derives `anyOf: [T, {"type": "null"}]`. As in 1,
///    optionality is carried by `required`, so the `null` branch is dropped
///    and the declaration is `T` itself, keeping the field's own
///    `description`.
fn narrow_to_api_subset(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            // 1. `"type": [.., "null"]` → the single non-null member.
            if let Some(serde_json::Value::Array(members)) = object.get("type") {
                let mut kept = members
                    .iter()
                    .filter(|m| m.as_str() != Some("null"))
                    .cloned();
                if let (Some(only), None) = (kept.next(), kept.next()) {
                    object.insert("type".into(), only);
                }
            }

            // 3. `anyOf: [T, {type: null}]` → `T`, with the outer keys kept.
            let collapsed = object.get("anyOf").and_then(|any_of| {
                let mut kept = any_of
                    .as_array()?
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) != Some("null"));
                match (kept.next(), kept.next()) {
                    (Some(serde_json::Value::Object(only)), None) => Some(only.clone()),
                    _ => None,
                }
            });
            if let Some(only) = collapsed {
                object.remove("anyOf");
                for (key, value) in only {
                    object.entry(key).or_insert(value);
                }
            }

            // 2. `oneOf: [{enum: [a]}, {enum: [b]}]` → `type: string, enum: [a, b]`.
            let flattened = object.get("oneOf").and_then(|one_of| {
                let branches = one_of.as_array()?;
                if branches.is_empty() {
                    return None;
                }
                branches
                    .iter()
                    .map(|branch| {
                        let single = branch.get("enum")?.as_array()?;
                        match single.as_slice() {
                            [only] if only.is_string() => Some(only.clone()),
                            _ => None,
                        }
                    })
                    .collect::<Option<Vec<_>>>()
            });
            if let Some(variants) = flattened {
                object.remove("oneOf");
                object.insert("type".into(), serde_json::Value::String("string".into()));
                object.insert("enum".into(), serde_json::Value::Array(variants));
            }

            for nested in object.values_mut() {
                narrow_to_api_subset(nested);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                narrow_to_api_subset(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    enum Seating {
        Indoor,
        Outdoor,
    }

    /// A guest.
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Guest {
        name: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Booking {
        /// Where to sit.
        seating: Option<Seating>,
        guest: Option<Guest>,
    }

    #[test]
    fn an_optional_composite_declares_one_type() {
        let schema = wire_schema::<Booking>();
        let seating = &schema["properties"]["seating"];
        assert!(seating.get("anyOf").is_none(), "{seating}");
        assert_eq!(seating["type"], "string");
        assert_eq!(seating["enum"], json!(["Indoor", "Outdoor"]));
        assert_eq!(
            seating["description"], "Where to sit.",
            "the field's own description wins"
        );

        let guest = &schema["properties"]["guest"];
        assert!(guest.get("anyOf").is_none(), "{guest}");
        assert_eq!(guest["type"], "object");
        assert_eq!(guest["required"], json!(["name"]));
        assert!(
            schema.get("required").is_none_or(|r| r == &json!([])),
            "{schema}"
        );
    }
}
