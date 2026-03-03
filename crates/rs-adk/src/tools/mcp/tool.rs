//! Individual MCP tool — wraps an MCP session manager to implement ToolFunction.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::ToolFunction;

use super::session_manager::McpSessionManager;

/// An individual MCP tool, wrapping an MCP session manager.
pub struct McpTool {
    name: String,
    description: String,
    input_schema: Option<serde_json::Value>,
    session_manager: Arc<McpSessionManager>,
}

impl McpTool {
    /// Create a new MCP tool proxy with the given name, description, and session.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Option<serde_json::Value>,
        session_manager: Arc<McpSessionManager>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            session_manager,
        }
    }
}

#[async_trait]
impl ToolFunction for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Option<serde_json::Value> {
        self.input_schema.clone()
    }
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        self.session_manager
            .call_tool(&self.name, args)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }
}
