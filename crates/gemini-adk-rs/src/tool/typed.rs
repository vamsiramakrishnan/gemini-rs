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
        let schema = super::wire_schema::<T>();

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
