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
