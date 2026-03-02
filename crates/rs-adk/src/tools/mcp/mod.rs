//! MCP (Model Context Protocol) toolset — connect to MCP servers and use their tools.

pub mod session_manager;
pub mod tool;
pub mod toolset;

pub use session_manager::{McpConnectionParams, McpError, McpSessionManager, McpToolInfo};
pub use tool::McpTool;
pub use toolset::McpToolset;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ToolError;
    use crate::tool::ToolFunction;
    use crate::toolset::Toolset;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    // --- McpConnectionParams tests ---

    #[test]
    fn connection_params_stdio() {
        let params = McpConnectionParams::Stdio {
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            timeout: Some(Duration::from_secs(10)),
        };
        match &params {
            McpConnectionParams::Stdio {
                command,
                args,
                timeout,
            } => {
                assert_eq!(command, "node");
                assert_eq!(args, &["server.js"]);
                assert_eq!(*timeout, Some(Duration::from_secs(10)));
            }
            _ => panic!("expected Stdio variant"),
        }
    }

    #[test]
    fn connection_params_sse() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer token".to_string());
        let params = McpConnectionParams::Sse {
            url: "http://localhost:8080/sse".to_string(),
            headers: Some(headers.clone()),
        };
        match &params {
            McpConnectionParams::Sse { url, headers: h } => {
                assert_eq!(url, "http://localhost:8080/sse");
                let h = h.as_ref().unwrap();
                assert_eq!(h.get("Authorization").unwrap(), "Bearer token");
            }
            _ => panic!("expected Sse variant"),
        }
    }

    #[test]
    fn connection_params_stdio_no_timeout() {
        let params = McpConnectionParams::Stdio {
            command: "python".to_string(),
            args: vec![],
            timeout: None,
        };
        match &params {
            McpConnectionParams::Stdio { timeout, .. } => {
                assert!(timeout.is_none());
            }
            _ => panic!("expected Stdio variant"),
        }
    }

    #[test]
    fn connection_params_sse_no_headers() {
        let params = McpConnectionParams::Sse {
            url: "http://localhost:3000".to_string(),
            headers: None,
        };
        match &params {
            McpConnectionParams::Sse { headers, .. } => {
                assert!(headers.is_none());
            }
            _ => panic!("expected Sse variant"),
        }
    }

    // --- McpSessionManager tests ---

    #[tokio::test]
    async fn session_manager_list_tools_returns_empty() {
        let manager = McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        });
        let tools = manager.list_tools().await.unwrap();
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn session_manager_call_tool_returns_not_connected() {
        let manager = McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        });
        let result = manager.call_tool("some_tool", json!({})).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match &err {
            McpError::NotConnected(msg) => {
                assert!(msg.contains("some_tool"));
            }
            other => panic!("expected McpError::NotConnected, got: {other}"),
        }
    }

    #[test]
    fn session_manager_params_accessor() {
        let params = McpConnectionParams::Sse {
            url: "http://example.com".to_string(),
            headers: None,
        };
        let manager = McpSessionManager::new(params);
        match manager.params() {
            McpConnectionParams::Sse { url, .. } => {
                assert_eq!(url, "http://example.com");
            }
            _ => panic!("expected Sse variant"),
        }
    }

    // --- McpTool tests ---

    #[test]
    fn mcp_tool_name_description_parameters() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        }));
        let schema = json!({"type": "object", "properties": {"query": {"type": "string"}}});
        let tool = McpTool::new("search", "Search for things", Some(schema.clone()), manager);

        assert_eq!(tool.name(), "search");
        assert_eq!(tool.description(), "Search for things");
        assert_eq!(tool.parameters(), Some(schema));
    }

    #[test]
    fn mcp_tool_no_schema() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        }));
        let tool = McpTool::new("ping", "Ping the server", None, manager);

        assert_eq!(tool.name(), "ping");
        assert!(tool.parameters().is_none());
    }

    #[tokio::test]
    async fn mcp_tool_call_delegates_to_session_manager() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        }));
        let tool = McpTool::new("my_tool", "desc", None, manager);

        let result = tool.call(json!({"key": "value"})).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("my_tool"));
                assert!(msg.contains("not connected") || msg.contains("Not connected"));
            }
            other => panic!("expected ToolError::ExecutionFailed, got: {other:?}"),
        }
    }

    // --- McpToolset tests ---

    #[test]
    fn mcp_toolset_get_tools_returns_empty() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        }));
        let toolset = McpToolset::new(manager);
        assert!(toolset.get_tools().is_empty());
    }

    #[test]
    fn mcp_toolset_with_filter_stores_filter() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        }));
        let toolset = McpToolset::new(manager)
            .with_filter(vec!["tool_a".to_string(), "tool_b".to_string()]);

        let filter = toolset.filter().unwrap();
        assert_eq!(filter.len(), 2);
        assert_eq!(filter[0], "tool_a");
        assert_eq!(filter[1], "tool_b");
    }

    #[test]
    fn mcp_toolset_no_filter_by_default() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        }));
        let toolset = McpToolset::new(manager);
        assert!(toolset.filter().is_none());
    }

    #[tokio::test]
    async fn mcp_toolset_close_is_noop() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        }));
        let toolset = McpToolset::new(manager);
        toolset.close().await; // Should not panic
    }

    #[test]
    fn mcp_toolset_session_manager_accessor() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Sse {
            url: "http://localhost:9090".to_string(),
            headers: None,
        }));
        let toolset = McpToolset::new(manager.clone());
        // Verify the session manager is accessible
        match toolset.session_manager().params() {
            McpConnectionParams::Sse { url, .. } => {
                assert_eq!(url, "http://localhost:9090");
            }
            _ => panic!("expected Sse variant"),
        }
    }

    // --- McpError display tests ---

    #[test]
    fn mcp_error_display() {
        let err = McpError::ConnectionFailed("timeout".to_string());
        assert_eq!(err.to_string(), "Connection failed: timeout");

        let err = McpError::NotConnected("no session".to_string());
        assert_eq!(err.to_string(), "Not connected: no session");

        let err = McpError::ToolCallFailed("bad args".to_string());
        assert_eq!(err.to_string(), "Tool call failed: bad args");

        let err = McpError::Other("something".to_string());
        assert_eq!(err.to_string(), "something");
    }
}
