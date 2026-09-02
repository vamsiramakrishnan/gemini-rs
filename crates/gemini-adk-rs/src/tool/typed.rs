//! Type-safe function tool with auto-generated JSON Schema.

use std::marker::PhantomData;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::error::ToolError;

use super::ToolFunction;

/// Type-safe function tool with auto-generated JSON Schema.
///
/// Unlike [`super::SimpleTool`] which takes raw `serde_json::Value` arguments and
/// requires a manually written schema, `TypedTool` auto-generates the JSON
/// Schema from a struct that derives [`schemars::JsonSchema`] and deserializes
/// the arguments into that struct before calling the handler.
///
/// # Example
///
/// ```ignore
/// use schemars::JsonSchema;
/// use serde::Deserialize;
///
/// #[derive(Deserialize, JsonSchema)]
/// struct WeatherArgs {
///     /// The city to get weather for
///     city: String,
/// }
///
/// let tool = TypedTool::new::<WeatherArgs>(
///     "get_weather",
///     "Get current weather for a city",
///     |args: WeatherArgs| async move {
///         Ok(serde_json::json!({ "temp": 22, "city": args.city }))
///     },
/// );
/// ```
pub struct TypedTool<T: DeserializeOwned + JsonSchema + Send + Sync + 'static> {
    name: String,
    description: String,
    schema: serde_json::Value,
    #[allow(clippy::type_complexity)]
    handler: Box<
        dyn Fn(
                T,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send>,
            > + Send
            + Sync,
    >,
    _phantom: PhantomData<T>,
}

/// Derive a JSON Schema in the shape the Gemini API will actually enforce.
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
/// The result is then narrowed to the API's schema subset by
/// [`narrow_to_api_subset`], which draft-07 is broader than in two ways that
/// matter.
fn wire_schema_for<T: JsonSchema>() -> serde_json::Value {
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
/// Two rewrites, both verified against the live endpoint:
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

impl<T: DeserializeOwned + JsonSchema + Send + Sync + 'static> TypedTool<T> {
    /// Create a new typed function tool with auto-generated schema.
    ///
    /// The JSON Schema is derived from `T`'s [`JsonSchema`] implementation,
    /// including any doc-comment descriptions on fields.
    pub fn new<F, Fut>(name: impl Into<String>, description: impl Into<String>, handler: F) -> Self
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send + 'static,
    {
        let schema = wire_schema_for::<T>();

        Self {
            name: name.into(),
            description: description.into(),
            schema,
            handler: Box::new(move |args| Box::pin(handler(args))),
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<T: DeserializeOwned + JsonSchema + Send + Sync + 'static> ToolFunction for TypedTool<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(self.schema.clone())
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let typed_args: T = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArgs(format!("Failed to deserialize arguments: {e}")))?;
        (self.handler)(typed_args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// A nested type is what triggers `definitions` + `$ref`.
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    enum Scope {
        Recent,
        Persistent,
    }

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct Args {
        /// What to look for.
        query: String,
        /// Which slice to search.
        scope: Scope,
        /// Optional — the construct the API rejects outright.
        note: Option<String>,
    }

    fn schema_of<T: DeserializeOwned + JsonSchema + Send + Sync + 'static>() -> serde_json::Value {
        TypedTool::<T>::new("probe", "Probe", |_: T| async { Ok(serde_json::json!({})) })
            .parameters()
            .expect("typed tools declare parameters")
    }

    /// A declaration that points outside itself is not merely useless — the
    /// Live endpoint closes the connection during setup rather than accept it,
    /// and the batch endpoints ignore the constraint and let the model invent
    /// enum variants that will not deserialize.
    #[test]
    fn a_nested_type_does_not_leak_refs_into_the_declaration() {
        let rendered = schema_of::<Args>().to_string();

        assert!(
            !rendered.contains("$ref"),
            "the API does not resolve $ref, so the schema silently stops \
             constraining: {rendered}"
        );
        assert!(
            !rendered.contains("definitions"),
            "schema leaks definitions: {rendered}"
        );
        assert!(
            !rendered.contains("$schema"),
            "schema leaks its meta-schema: {rendered}"
        );
    }

    /// Inlining must preserve the constraint, not just remove the pointer.
    #[test]
    fn the_inlined_schema_still_carries_the_enum_variants() {
        let schema = schema_of::<Args>();
        let scope = &schema["properties"]["scope"];

        assert_eq!(
            scope["type"], "string",
            "a fieldless enum must narrow to a plain string type: {scope}"
        );
        assert_eq!(
            scope["enum"],
            serde_json::json!(["Recent", "Persistent"]),
            "flattening dropped the variants it was supposed to preserve: {scope}"
        );
        assert!(
            scope.get("oneOf").is_none(),
            "the API does not understand `oneOf` here, so the constraint would \
             silently stop applying: {scope}"
        );
    }

    /// The one that closes a Live session mid-handshake: `Option<String>`
    /// derives `"type": ["string", "null"]`, and the API's `Schema.type` is a
    /// single value. It rejects the whole request rather than ignoring it.
    #[test]
    fn an_optional_field_does_not_declare_a_union_type() {
        let schema = schema_of::<Args>();
        let note = &schema["properties"]["note"];

        assert_eq!(
            note["type"], "string",
            "a union type is rejected outright by the API: {note}"
        );
        assert!(
            !schema["required"]
                .as_array()
                .expect("required list")
                .iter()
                .any(|r| r == "note"),
            "optionality must still be carried by absence from `required`: {schema}"
        );
    }
}
