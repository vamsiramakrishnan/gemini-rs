//! Simple function tool that wraps an async closure.

use async_trait::async_trait;

use crate::error::ToolError;

use super::ToolFunction;

/// Simple function tool that wraps an async closure.
pub struct SimpleTool {
    name: String,
    description: String,
    parameters: Option<serde_json::Value>,
    #[allow(clippy::type_complexity)]
    handler: Box<
        dyn Fn(
                serde_json::Value,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send>,
            > + Send
            + Sync,
    >,
}

impl SimpleTool {
    /// Create a new simple function tool.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use gemini_adk_rs::tool::SimpleTool;
    /// use serde_json::json;
    ///
    /// let tool = SimpleTool::new(
    ///     "greet",
    ///     "Greet a user by name",
    ///     Some(json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]})),
    ///     |args| async move {
    ///         let name = args["name"].as_str().unwrap_or("World");
    ///         Ok(json!({"greeting": format!("Hello, {name}!")}))
    ///     },
    /// );
    /// ```
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Option<serde_json::Value>,
        handler: F,
    ) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            handler: Box::new(move |args| Box::pin(handler(args))),
        }
    }
}

#[async_trait]
impl ToolFunction for SimpleTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Option<serde_json::Value> {
        self.parameters.clone()
    }
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        (self.handler)(args).await
    }
}
