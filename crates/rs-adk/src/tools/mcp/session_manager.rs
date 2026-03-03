//! MCP session management — connection params, tool discovery, and tool invocation.

use std::collections::HashMap;
use std::time::Duration;

/// Connection parameters for an MCP server.
#[derive(Debug, Clone)]
pub enum McpConnectionParams {
    /// Connect via stdio (subprocess).
    Stdio {
        /// The command to execute.
        command: String,
        /// Arguments passed to the command.
        args: Vec<String>,
        /// Connection timeout.
        timeout: Option<Duration>,
    },
    /// Connect via SSE/StreamableHTTP.
    Sse {
        /// The URL of the MCP server.
        url: String,
        /// Optional HTTP headers for authentication.
        headers: Option<HashMap<String, String>>,
    },
}

/// Manages the MCP client session lifecycle.
pub struct McpSessionManager {
    params: McpConnectionParams,
}

impl McpSessionManager {
    /// Create a new MCP session manager with the given connection params.
    pub fn new(params: McpConnectionParams) -> Self {
        Self { params }
    }

    /// Get the connection parameters.
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
    /// Tool name.
    pub name: String,
    /// Human-readable tool description.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
}

/// MCP-related errors.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Failed to connect to the MCP server.
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    /// The MCP session is not connected.
    #[error("Not connected: {0}")]
    NotConnected(String),
    /// A tool call to the MCP server failed.
    #[error("Tool call failed: {0}")]
    ToolCallFailed(String),
    /// A catch-all for other MCP errors.
    #[error("{0}")]
    Other(String),
}
