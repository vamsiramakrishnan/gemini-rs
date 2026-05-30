//! MCP session management — connection params, tool discovery, and tool invocation.
//!
//! Implements a real MCP (Model Context Protocol) client speaking JSON-RPC 2.0.
//! The primary transport is **stdio** (newline-delimited JSON over a subprocess's
//! stdin/stdout), which works on default features. An optional **HTTP** transport
//! (single-shot JSON-RPC POST) is available behind the `mcp-http` feature.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// MCP protocol version advertised during the handshake.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

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

/// Live stdio connection state: the child process plus framed I/O handles.
struct StdioConnection {
    /// The child process. Kept alive so the pipes stay open; killed on drop.
    #[allow(dead_code)]
    child: Child,
    /// Subprocess stdin (we write requests here).
    stdin: ChildStdin,
    /// Buffered subprocess stdout (we read newline-delimited responses here).
    stdout: BufReader<ChildStdout>,
}

/// Manages the MCP client session lifecycle.
pub struct McpSessionManager {
    params: McpConnectionParams,
    /// Lazily-established stdio connection (None until first use, then reused).
    stdio: Mutex<Option<StdioConnection>>,
    /// Monotonic JSON-RPC request id counter.
    next_id: AtomicU64,
}

impl McpSessionManager {
    /// Create a new MCP session manager with the given connection params.
    pub fn new(params: McpConnectionParams) -> Self {
        Self {
            params,
            stdio: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    /// Get the connection parameters.
    pub fn params(&self) -> &McpConnectionParams {
        &self.params
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// List available tools from the MCP server.
    ///
    /// Connects (and performs the MCP handshake) lazily on first use, then issues
    /// a `tools/list` JSON-RPC request and maps the result into [`McpToolInfo`]s.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        match &self.params {
            McpConnectionParams::Stdio { .. } => self.stdio_list_tools().await,
            McpConnectionParams::Sse { .. } => self.http_list_tools().await,
        }
    }

    /// Call a tool on the MCP server via `tools/call`.
    ///
    /// Returns the JSON-RPC `result` object on success. Returns
    /// [`McpError::ToolCallFailed`] on a JSON-RPC error or when the result has
    /// `isError: true`.
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value, McpError> {
        match &self.params {
            McpConnectionParams::Stdio { .. } => self.stdio_call_tool(name, args).await,
            McpConnectionParams::Sse { .. } => self.http_call_tool(name, args).await,
        }
    }

    // ------------------------------------------------------------------
    // stdio transport
    // ------------------------------------------------------------------

    async fn stdio_list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let timeout = self.stdio_timeout();
        let mut guard = self.stdio.lock().await;
        self.ensure_connected(&mut guard, timeout).await?;
        let conn = guard.as_mut().expect("connection established above");

        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {},
        });
        let result = stdio_request(conn, id, &req, timeout).await?;
        parse_tools_list(&result)
    }

    async fn stdio_call_tool(&self, name: &str, args: Value) -> Result<Value, McpError> {
        let timeout = self.stdio_timeout();
        let mut guard = self.stdio.lock().await;
        self.ensure_connected(&mut guard, timeout).await?;
        let conn = guard.as_mut().expect("connection established above");

        let id = self.next_id();
        let arguments = if args.is_null() { json!({}) } else { args };
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let result = stdio_request(conn, id, &req, timeout).await?;
        check_tool_result(&result, name)
    }

    fn stdio_timeout(&self) -> Option<Duration> {
        match &self.params {
            McpConnectionParams::Stdio { timeout, .. } => *timeout,
            _ => None,
        }
    }

    /// Ensure a stdio connection exists and has completed the MCP handshake.
    async fn ensure_connected(
        &self,
        guard: &mut Option<StdioConnection>,
        timeout: Option<Duration>,
    ) -> Result<(), McpError> {
        if guard.is_some() {
            return Ok(());
        }
        let (command, args) = match &self.params {
            McpConnectionParams::Stdio { command, args, .. } => (command.clone(), args.clone()),
            _ => {
                return Err(McpError::ConnectionFailed(
                    "stdio transport requested for non-stdio params".to_string(),
                ))
            }
        };

        let mut child = Command::new(&command)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                McpError::ConnectionFailed(format!("failed to spawn MCP server '{command}': {e}"))
            })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            McpError::ConnectionFailed("MCP server stdin not available".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            McpError::ConnectionFailed("MCP server stdout not available".to_string())
        })?;

        let mut conn = StdioConnection {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        };

        // --- Handshake: initialize -> read result -> notifications/initialized ---
        let id = self.next_id();
        let init = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "gemini-adk-rs", "version": "0.6.0" },
            },
        });
        stdio_request(&mut conn, id, &init, timeout)
            .await
            .map_err(|e| McpError::ConnectionFailed(format!("MCP initialize failed: {e}")))?;

        let initialized = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        });
        stdio_write(&mut conn, &initialized).await.map_err(|e| {
            McpError::ConnectionFailed(format!("MCP initialized notify failed: {e}"))
        })?;

        *guard = Some(conn);
        Ok(())
    }

    // ------------------------------------------------------------------
    // HTTP transport (feature-gated)
    // ------------------------------------------------------------------

    #[cfg(feature = "mcp-http")]
    async fn http_list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {},
        });
        let result = self.http_request(id, &req).await?;
        parse_tools_list(&result)
    }

    #[cfg(feature = "mcp-http")]
    async fn http_call_tool(&self, name: &str, args: Value) -> Result<Value, McpError> {
        let id = self.next_id();
        let arguments = if args.is_null() { json!({}) } else { args };
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let result = self.http_request(id, &req).await?;
        check_tool_result(&result, name)
    }

    #[cfg(feature = "mcp-http")]
    async fn http_request(&self, id: u64, req: &Value) -> Result<Value, McpError> {
        let (url, headers) = match &self.params {
            McpConnectionParams::Sse { url, headers } => (url.clone(), headers.clone()),
            _ => {
                return Err(McpError::ConnectionFailed(
                    "HTTP transport requested for non-SSE params".to_string(),
                ))
            }
        };

        let client = reqwest::Client::new();
        let mut builder = client
            .post(&url)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .json(req);
        if let Some(hdrs) = headers {
            for (k, v) in hdrs {
                builder = builder.header(k, v);
            }
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| McpError::ConnectionFailed(format!("MCP HTTP request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(McpError::ConnectionFailed(format!(
                "MCP HTTP request returned status {}",
                resp.status()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| McpError::Other(format!("invalid MCP HTTP response body: {e}")))?;
        extract_result(&body, id)
    }

    #[cfg(not(feature = "mcp-http"))]
    async fn http_list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        Err(McpError::ConnectionFailed(
            "mcp-http feature not enabled".to_string(),
        ))
    }

    #[cfg(not(feature = "mcp-http"))]
    async fn http_call_tool(&self, _name: &str, _args: Value) -> Result<Value, McpError> {
        Err(McpError::ConnectionFailed(
            "mcp-http feature not enabled".to_string(),
        ))
    }
}

// ----------------------------------------------------------------------
// stdio framing helpers
// ----------------------------------------------------------------------

/// Write a single JSON-RPC message as one compact, newline-terminated line.
async fn stdio_write(conn: &mut StdioConnection, msg: &Value) -> Result<(), McpError> {
    let mut line = serde_json::to_string(msg)
        .map_err(|e| McpError::Other(format!("failed to serialize JSON-RPC: {e}")))?;
    line.push('\n');
    conn.stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| McpError::ConnectionFailed(format!("failed to write to MCP server: {e}")))?;
    conn.stdin.flush().await.map_err(|e| {
        McpError::ConnectionFailed(format!("failed to flush MCP server stdin: {e}"))
    })?;
    Ok(())
}

/// Send a JSON-RPC request and read the matching response by `id`, skipping
/// notifications and unrelated messages. Honors the optional timeout.
async fn stdio_request(
    conn: &mut StdioConnection,
    id: u64,
    req: &Value,
    timeout: Option<Duration>,
) -> Result<Value, McpError> {
    let fut = async {
        stdio_write(conn, req).await?;
        loop {
            let mut line = String::new();
            let n = conn.stdout.read_line(&mut line).await.map_err(|e| {
                McpError::ConnectionFailed(format!("failed to read from MCP server: {e}"))
            })?;
            if n == 0 {
                // EOF: child closed stdout / exited before responding.
                return Err(McpError::ConnectionFailed(
                    "MCP server closed connection before responding".to_string(),
                ));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                // Non-JSON line (stray log output) — skip it.
                Err(_) => continue,
            };
            // Skip anything that isn't the response to our request id.
            match msg.get("id").and_then(value_id_as_u64) {
                Some(resp_id) if resp_id == id => return extract_result(&msg, id),
                _ => continue,
            }
        }
    };

    match timeout {
        Some(dur) => match tokio::time::timeout(dur, fut).await {
            Ok(res) => res,
            Err(_) => Err(McpError::ConnectionFailed(format!(
                "MCP request timed out after {dur:?}"
            ))),
        },
        None => fut.await,
    }
}

// ----------------------------------------------------------------------
// JSON-RPC / MCP result parsing (shared by both transports)
// ----------------------------------------------------------------------

/// Interpret a JSON-RPC `id` value (number or numeric string) as u64.
fn value_id_as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.parse::<u64>().ok())
}

/// Extract the `result` from a JSON-RPC response, mapping `error` to [`McpError`].
fn extract_result(msg: &Value, id: u64) -> Result<Value, McpError> {
    if let Some(err) = msg.get("error") {
        let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(McpError::ToolCallFailed(format!(
            "JSON-RPC error {code}: {message}"
        )));
    }
    match msg.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(McpError::Other(format!(
            "JSON-RPC response (id {id}) has neither result nor error"
        ))),
    }
}

/// Map a `tools/list` result into [`McpToolInfo`]s.
fn parse_tools_list(result: &Value) -> Result<Vec<McpToolInfo>, McpError> {
    let tools = result
        .get("tools")
        .and_then(|t| t.as_array())
        .ok_or_else(|| McpError::Other("tools/list result missing 'tools' array".to_string()))?;

    let mut out = Vec::with_capacity(tools.len());
    for t in tools {
        let name = t
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| McpError::Other("tool entry missing 'name'".to_string()))?
            .to_string();
        let description = t
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();
        let input_schema = t
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"}));
        out.push(McpToolInfo {
            name,
            description,
            input_schema,
        });
    }
    Ok(out)
}

/// Validate a `tools/call` result, surfacing `isError: true` as a failure.
fn check_tool_result(result: &Value, name: &str) -> Result<Value, McpError> {
    if result
        .get("isError")
        .and_then(|e| e.as_bool())
        .unwrap_or(false)
    {
        return Err(McpError::ToolCallFailed(format!(
            "tool '{name}' reported isError: {result}"
        )));
    }
    Ok(result.clone())
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
