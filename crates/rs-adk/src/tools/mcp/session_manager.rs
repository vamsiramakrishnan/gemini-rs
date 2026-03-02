//! MCP session management — connection params, tool discovery, and tool invocation.

use std::collections::HashMap;
use std::time::Duration;

/// Connection parameters for an MCP server.
#[derive(Debug, Clone)]
pub enum McpConnectionParams {
    /// Connect via stdio (subprocess).
    Stdio {
        command: String,
        args: Vec<String>,
        timeout: Option<Duration>,
    },
    /// Connect via SSE/StreamableHTTP.
    Sse {
        url: String,
        headers: Option<HashMap<String, String>>,
    },
}

/// Manages the MCP client session lifecycle.
pub struct McpSessionManager {
    params: McpConnectionParams,
}

impl McpSessionManager {
    pub fn new(params: McpConnectionParams) -> Self {
        Self { params }
    }

    pub fn params(&self) -> &McpConnectionParams {
        &self.params
    }

    /// List available tools from the MCP server.
    /// Stub: returns empty list. Full implementation awaits MCP client integration.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        // TODO: Connect to MCP server and list tools
        Ok(vec![])
    }

    /// Call a tool on the MCP server.
    /// Stub: returns error. Full implementation awaits MCP client integration.
    pub async fn call_tool(
        &self,
        name: &str,
        _args: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        Err(McpError::NotConnected(format!(
            "MCP session not connected — call_tool({}) requires MCP client integration",
            name
        )))
    }
}

/// Information about an MCP tool.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// MCP-related errors.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Not connected: {0}")]
    NotConnected(String),
    #[error("Tool call failed: {0}")]
    ToolCallFailed(String),
    #[error("{0}")]
    Other(String),
}
