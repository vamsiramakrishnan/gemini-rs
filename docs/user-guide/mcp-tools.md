# MCP Tools

The Model Context Protocol (MCP) is an open standard for connecting AI models
to external tools and data sources. Tools are exposed by an MCP server process;
clients discover them via a `tools/list` RPC and invoke them via `tools/call`.

The SDK implements a real MCP client (`McpSessionManager`) that speaks
JSON-RPC 2.0. You create an `McpToolset`, connect it to a `ToolDispatcher`, and
the dispatcher routes model function calls to the MCP server transparently.

---

## Connection transports

### Stdio (default, no feature flags)

The server runs as a subprocess. The SDK communicates over the subprocess's
`stdin`/`stdout` using newline-delimited JSON-RPC. This is the standard MCP
transport and requires no extra features.

```rust,ignore
use gemini_adk_rs::tools::mcp::{McpConnectionParams, McpSessionManager};
use std::sync::Arc;
use std::time::Duration;

let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
    command: "npx".to_string(),
    args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
    timeout: Some(Duration::from_secs(30)),
}));
```

`timeout` applies to each individual JSON-RPC request (including the `initialize`
handshake). Pass `None` to wait indefinitely.

### HTTP/SSE (behind `mcp-http` feature)

For MCP servers that accept HTTP POST requests (StreamableHTTP transport):

```toml
[dependencies]
gemini-adk-rs = { version = "2.0", features = ["mcp-http"] }
```

```rust,ignore
use std::collections::HashMap;

let mut headers = HashMap::new();
headers.insert("Authorization".to_string(), "Bearer my-token".to_string());

let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Sse {
    url: "https://mcp.example.com/rpc".to_string(),
    headers: Some(headers),
}));
```

Without the `mcp-http` feature, `McpConnectionParams::Sse` returns
`McpError::ConnectionFailed("mcp-http feature not enabled")` on any call.

---

## Connection lifecycle

The SDK connects lazily: the subprocess is only spawned (or the first HTTP
request only made) on the first `list_tools` or `call_tool` call. Once
connected, the stdio connection is reused across all subsequent calls.

### Handshake

For stdio connections, the SDK performs the MCP handshake automatically:

1. Sends `initialize` with `protocolVersion: "2024-11-05"` and client info
   `{ name: "gemini-adk-rs", version: "0.6.0" }`.
2. Reads the server's `initialize` result.
3. Sends `notifications/initialized` (no response expected).

After the handshake, the connection is considered ready and stored for reuse.
Stray non-JSON lines on stdout (e.g., server log output) are silently skipped.

---

## Discovering and registering tools

`McpToolset` wraps a session manager and implements the `Toolset` trait. The
tool list is discovered by calling `manager.list_tools()` and registering each
result as an `McpTool` in the dispatcher.

```rust,ignore
use gemini_adk_rs::tools::mcp::{McpConnectionParams, McpSessionManager, McpToolset};
use gemini_adk_rs::tool::ToolDispatcher;
use std::sync::Arc;
use std::time::Duration;

// 1. Create the session manager
let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
    command: "python3".to_string(),
    args: vec!["-m".to_string(), "my_mcp_server".to_string()],
    timeout: Some(Duration::from_secs(15)),
}));

// 2. Discover tools and register them
let tools = manager.list_tools().await?;
let mut dispatcher = ToolDispatcher::new();
for info in tools {
    dispatcher.register(gemini_adk_rs::tools::mcp::tool::McpTool::new(
        info.name,
        info.description,
        Some(info.input_schema),
        Arc::clone(&manager),
    ));
}

// 3. Pass the dispatcher to the Live builder
Live::builder()
    .dispatcher(dispatcher)
    .connect_from_env()
    .await?;
```

### Filtering exposed tools

`McpToolset::with_filter` restricts the tools registered from an MCP server to
a named subset:

```rust,ignore
let toolset = McpToolset::new(Arc::clone(&manager))
    .with_filter(vec!["read_file".to_string(), "list_directory".to_string()]);
```

---

## Tool invocation

When the model calls a tool registered from MCP, `McpTool::call` forwards the
invocation to `McpSessionManager::call_tool`, which sends a `tools/call`
JSON-RPC request and returns the result:

```json
{ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
  "params": { "name": "read_file", "arguments": { "path": "/etc/hosts" } } }
```

The server's response `result` object is returned as-is. If `isError: true`
is set in the result, the call is mapped to `McpError::ToolCallFailed`. JSON-RPC
`error` objects are also mapped to `McpError::ToolCallFailed`.

`McpTool` implements `ToolFunction`, so it behaves identically to any other tool
from the model's perspective. Errors from MCP are surfaced as
`ToolError::ExecutionFailed`.

---

## Error handling

```rust,ignore
use gemini_adk_rs::tools::mcp::session_manager::McpError;

match manager.list_tools().await {
    Ok(tools) => { /* ... */ }
    Err(McpError::ConnectionFailed(msg)) => {
        eprintln!("Could not connect to MCP server: {msg}");
    }
    Err(McpError::ToolCallFailed(msg)) => {
        eprintln!("Tool call failed: {msg}");
    }
    Err(McpError::NotConnected(msg)) => {
        eprintln!("Session not established: {msg}");
    }
    Err(McpError::Other(msg)) => {
        eprintln!("Other MCP error: {msg}");
    }
}
```

---

## Using T:: with MCP

`T::mcp(params)` is a deferred entry: `Live::connect` opens the MCP session
(an `http(s)://` URL becomes SSE, anything else a stdio command line), lists
its tools, and registers one `McpTool` per discovered tool on the session's
dispatcher before the Live handshake. A discovery failure is a connect error.

```rust,ignore
Live::builder()
    .tools(T::mcp("npx -y @modelcontextprotocol/server-filesystem") | T::google_search())
    .connect_from_env()
    .await?;
```

A text `AgentBuilder` cannot perform that handshake — `AgentBuilder::build`
rejects a composite containing `T::mcp` with a `ConfigError` naming the tool,
so the entry is never dropped silently. For a text agent, discover tools via
`McpSessionManager::list_tools` and register them on a `ToolDispatcher` as in
the previous section.

---

## Feature flags

| Feature | Effect |
|---------|--------|
| _(none)_ | Stdio transport only |
| `mcp-http` | Enables `McpConnectionParams::Sse` HTTP transport (adds `reqwest` dependency) |

---

## Example: filesystem MCP server

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_rs::tools::mcp::{McpConnectionParams, McpSessionManager};
use gemini_adk_rs::tools::mcp::tool::McpTool;
use gemini_adk_rs::tool::ToolDispatcher;
use std::sync::Arc;
use std::time::Duration;

let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
    command: "npx".to_string(),
    args: vec![
        "-y".to_string(),
        "@modelcontextprotocol/server-filesystem".to_string(),
        "/workspace".to_string(),
    ],
    timeout: Some(Duration::from_secs(30)),
}));

// Discover tools from the server
let tool_infos = manager.list_tools().await?;
let mut dispatcher = ToolDispatcher::new();
for info in tool_infos {
    dispatcher.register(McpTool::new(
        info.name,
        info.description,
        Some(info.input_schema),
        Arc::clone(&manager),
    ));
}

let handle = Live::builder()
    .instruction("You can read and list files on the server filesystem.")
    .dispatcher(dispatcher)
    .connect_from_env()
    .await?;

handle.send_text("List the files in the root of /workspace").await?;
```

## See also

- [Tools](./tools.md) — `SimpleTool`, `TypedTool`, and the `ToolDispatcher` registration API
- [Per-Tool Policies](./tool-policies.md) — timeout, caching, and confirmation policies for MCP tools
- [cookbook 36 — MCP tools](../../examples/cookbook/src/36_mcp_tools.rs)
