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
        dyn Fn(T) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send>>
            + Send
            + Sync,
    >,
    _phantom: PhantomData<T>,
}

impl<T: DeserializeOwned + JsonSchema + Send + Sync + 'static> TypedTool<T> {
    /// Create a new typed function tool with auto-generated schema.
    ///
    /// The JSON Schema is derived from `T`'s [`JsonSchema`] implementation,
    /// including any doc-comment descriptions on fields.
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: F,
    ) -> Self
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send + 'static,
    {
        let root_schema = schemars::schema_for!(T);
        let schema = serde_json::to_value(root_schema)
            .expect("schemars schema should serialize to JSON");

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
        let typed_args: T = serde_json::from_value(args).map_err(|e| {
            ToolError::InvalidArgs(format!("Failed to deserialize arguments: {e}"))
        })?;
        (self.handler)(typed_args).await
    }
}
