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
    async fn session_manager_list_tools_unconnectable_server_errors() {
        // A bogus command cannot be spawned, so the lazy connect fails. The real
        // client surfaces this as a ConnectionFailed error rather than returning
        // an empty tool list.
        let manager = McpSessionManager::new(McpConnectionParams::Stdio {
            command: "definitely_not_a_real_mcp_server_binary_xyz".to_string(),
            args: vec![],
            timeout: Some(Duration::from_secs(2)),
        });
        let result = manager.list_tools().await;
        assert!(result.is_err());
        match result.unwrap_err() {
            McpError::ConnectionFailed(msg) => {
                assert!(msg.contains("definitely_not_a_real_mcp_server_binary_xyz"));
            }
            other => panic!("expected McpError::ConnectionFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn session_manager_call_tool_unconnectable_server_errors() {
        // `echo` ignores stdin and exits, closing its stdout before answering the
        // initialize handshake. The real client reports a connection failure.
        let manager = McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: Some(Duration::from_secs(2)),
        });
        let result = manager.call_tool("some_tool", json!({})).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            McpError::ConnectionFailed(_) => {}
            other => panic!("expected McpError::ConnectionFailed, got: {other}"),
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
        // `echo` is not a real MCP server, so the handshake fails. The McpTool
        // wraps the session manager's McpError into a ToolError::ExecutionFailed.
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: Some(Duration::from_secs(2)),
        }));
        let tool = McpTool::new("my_tool", "desc", None, manager);

        let result = tool.call(json!({"key": "value"})).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("Connection failed") || msg.contains("connection"));
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
        assert!(toolset.tools().is_empty());
    }

    #[test]
    fn mcp_toolset_with_filter_stores_filter() {
        let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
            command: "echo".to_string(),
            args: vec![],
            timeout: None,
        }));
        let toolset =
            McpToolset::new(manager).with_filter(vec!["tool_a".to_string(), "tool_b".to_string()]);

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

    // --- Real stdio MCP client integration test ---

    /// Return true if `python3` is available on PATH.
    fn python3_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// A tiny MCP server over stdio: reads newline-delimited JSON-RPC requests,
    /// handles `initialize`, ignores the `notifications/initialized` notification,
    /// answers `tools/list` with one tool, and answers `tools/call` by echoing the
    /// arguments back as text content.
    const MOCK_MCP_SERVER: &str = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        resp = {"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mock-mcp", "version": "0.1.0"}}}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
    elif method == "notifications/initialized":
        # notification, no response
        continue
    elif method == "tools/list":
        resp = {"jsonrpc": "2.0", "id": mid, "result": {"tools": [
            {"name": "echo", "description": "Echo back the input",
             "inputSchema": {"type": "object",
                             "properties": {"text": {"type": "string"}},
                             "required": ["text"]}}]}}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
    elif method == "tools/call":
        args = msg.get("params", {}).get("arguments", {})
        resp = {"jsonrpc": "2.0", "id": mid, "result": {
            "content": [{"type": "text", "text": json.dumps(args)}],
            "isError": False}}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
    else:
        resp = {"jsonrpc": "2.0", "id": mid,
                "error": {"code": -32601, "message": "method not found"}}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
"#;

    #[tokio::test]
    async fn stdio_initialize_list_and_call_against_mock_server() {
        if !python3_available() {
            eprintln!("skipping: python3 not found on PATH");
            return;
        }

        let manager = McpSessionManager::new(McpConnectionParams::Stdio {
            command: "python3".to_string(),
            args: vec!["-c".to_string(), MOCK_MCP_SERVER.to_string()],
            timeout: Some(Duration::from_secs(10)),
        });

        // tools/list (triggers lazy initialize handshake first).
        let tools = manager
            .list_tools()
            .await
            .expect("tools/list should succeed");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "Echo back the input");
        assert_eq!(tools[0].input_schema["type"], "object");

        // tools/call echoes the arguments back as text content. The connection is
        // reused (no second handshake).
        let result = manager
            .call_tool("echo", json!({"text": "hello mcp"}))
            .await
            .expect("tools/call should succeed");
        assert_eq!(result["isError"], false);
        let content_text = result["content"][0]["text"].as_str().unwrap();
        let echoed: serde_json::Value = serde_json::from_str(content_text).unwrap();
        assert_eq!(echoed["text"], "hello mcp");
    }
}
