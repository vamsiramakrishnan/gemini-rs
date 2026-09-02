//! # 36 — MCP Tools: Model Context Protocol Integration
//!
//! The SDK speaks the Model Context Protocol (MCP) natively. Any MCP server
//! — whether a local subprocess or a remote HTTP service — becomes a set of
//! callable tools that the model can invoke during a Live or text-agent
//! session.
//!
//! Key concepts:
//! - `McpConnectionParams::Stdio { command, args, timeout }` — launch a
//!   subprocess and speak MCP JSON-RPC 2.0 over its stdin/stdout
//! - `McpConnectionParams::Sse { url, headers }` — connect to an HTTP MCP
//!   server (requires the `mcp-http` feature)
//! - `McpSessionManager::new(params)` — manages the lazy connection and
//!   monotonic request IDs; reuses the connection across calls
//! - Handshake: `initialize` \u{2192} `notifications/initialized` \u{2192} ready
//! - `manager.list_tools()` — issues `tools/list`; returns `Vec<McpToolInfo>`
//! - `manager.call_tool(name, args)` — issues `tools/call`; returns the
//!   JSON-RPC `result` object
//! - `McpToolset::new(Arc::new(manager))` — wraps a manager in a `Toolset`
//!   for registration with a `Live::builder()` or `ToolDispatcher`; optionally
//!   filter to a subset of tools with `.with_filter(vec![...])`
//! - `T::mcp(params_string)` — fluent shorthand for adding an MCP toolset
//!   entry to a `ToolComposite`
//!
//! This example is a **config/builder demo** — no real MCP server is started.
//! It constructs connection params and a toolset, explains the lifecycle, and
//! shows how you would register them in a Live session or ToolDispatcher.
//! A real connection would require a running MCP server (e.g. a Node.js or
//! Python script that speaks JSON-RPC 2.0 over stdio or HTTP).

use std::sync::Arc;
use std::time::Duration;

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::tools::Toolset;
use gemini_adk_rs::tools::mcp::{McpConnectionParams, McpSessionManager, McpToolset};

fn main() {
    println!("=== 36: MCP Tools \u{2014} Model Context Protocol Integration ===\n");

    // ────────────────────────────────────────────────────────────────────────
    // 1. Connection params
    // ────────────────────────────────────────────────────────────────────────
    println!("--- 1. McpConnectionParams ---\n");

    // Stdio: spawn a subprocess and speak MCP JSON-RPC over pipes.
    // The most common transport — works with any language runtime.
    let stdio_params = McpConnectionParams::Stdio {
        command: "node".to_string(),
        args: vec!["./my-mcp-server.js".to_string()],
        timeout: Some(Duration::from_secs(30)),
    };
    println!("  Stdio params:");
    if let McpConnectionParams::Stdio {
        command,
        args,
        timeout,
    } = &stdio_params
    {
        println!("    command  = {command:?}");
        println!("    args     = {args:?}");
        println!("    timeout  = {timeout:?}");
    }
    println!();

    // A Python-based server (e.g. using the `mcp` PyPI package).
    let python_params = McpConnectionParams::Stdio {
        command: "python3".to_string(),
        args: vec!["-m".to_string(), "my_mcp_server".to_string()],
        timeout: Some(Duration::from_secs(10)),
    };
    println!("  Python stdio params:");
    if let McpConnectionParams::Stdio { command, args, .. } = &python_params {
        println!("    command = {command:?}, args = {args:?}");
    }
    println!();

    // SSE/StreamableHTTP: connect to a remote MCP server.
    // Requires the `mcp-http` feature (adds reqwest).
    let sse_params = McpConnectionParams::Sse {
        url: "https://mcp.example.com/sse".to_string(),
        headers: Some({
            let mut h = std::collections::HashMap::new();
            h.insert("Authorization".to_string(), "Bearer <token>".to_string());
            h
        }),
    };
    println!("  SSE params:");
    if let McpConnectionParams::Sse { url, headers } = &sse_params {
        println!("    url     = {url:?}");
        println!(
            "    headers = {:?}",
            headers.as_ref().map(|h| h.keys().collect::<Vec<_>>())
        );
    }
    println!("  (SSE transport requires feature = \"mcp-http\")\n");

    // ────────────────────────────────────────────────────────────────────────
    // 2. McpSessionManager — lazy connection + request dispatch
    // ────────────────────────────────────────────────────────────────────────
    println!("--- 2. McpSessionManager ---\n");

    // McpSessionManager holds connection params and manages the lazy stdio
    // connection. It does NOT connect on construction — the connection is
    // established on the first call to list_tools() or call_tool().
    let manager = McpSessionManager::new(McpConnectionParams::Stdio {
        command: "node".to_string(),
        args: vec!["./my-mcp-server.js".to_string()],
        timeout: Some(Duration::from_secs(30)),
    });

    // Verify the params are stored correctly.
    match manager.params() {
        McpConnectionParams::Stdio { command, args, .. } => {
            println!("  Manager holds Stdio params: command={command:?}, args={args:?}");
        }
        McpConnectionParams::Sse { url, .. } => {
            println!("  Manager holds SSE params: url={url:?}");
        }
    }
    println!();

    println!("  Lifecycle on first list_tools() or call_tool():");
    println!("    1. Spawn subprocess: Command::new(command).args(args).spawn()");
    println!("    2. Send initialize JSON-RPC request:");
    println!("         {{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",");
    println!("           \"params\":{{\"protocolVersion\":\"2024-11-05\",");
    println!("                    \"capabilities\":{{}},");
    println!("                    \"clientInfo\":{{\"name\":\"gemini-adk-rs\",...}}}}}}");
    println!("    3. Read and verify initialize response");
    println!("    4. Send notifications/initialized (no response expected):");
    println!("         {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}");
    println!("    5. Connection is now ready; reused for all subsequent calls\n");

    println!("  tools/list request:");
    println!("    {{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{{}}}}");
    println!("  Response \u{2192} Vec<McpToolInfo> {{name, description, input_schema}}\n");

    println!("  tools/call request:");
    println!("    {{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",");
    println!("      \"params\":{{\"name\":\"search\",\"arguments\":{{\"query\":\"rust\"}}}}}}");
    println!("  Response \u{2192} {{\"content\":[...],\"isError\":false}}\n");

    // ────────────────────────────────────────────────────────────────────────
    // 3. McpToolset — wraps a manager in the Toolset interface
    // ────────────────────────────────────────────────────────────────────────
    println!("--- 3. McpToolset ---\n");

    let manager_arc = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
        command: "node".to_string(),
        args: vec!["./my-mcp-server.js".to_string()],
        timeout: Some(Duration::from_secs(30)),
    }));

    // No filter — all tools from the server are exposed.
    let toolset_all = McpToolset::new(manager_arc.clone());
    println!(
        "  McpToolset (no filter):  {} tools pre-loaded",
        toolset_all.get_tools().len()
    );
    println!("  (Tools are loaded lazily on first list_tools() call)\n");

    // Filtered — only the named tools are passed to the model.
    let toolset_filtered = McpToolset::new(manager_arc.clone())
        .with_filter(vec!["search".to_string(), "summarize".to_string()]);

    match toolset_filtered.filter() {
        Some(names) => println!("  McpToolset (filtered):   {names:?}"),
        None => println!("  McpToolset (filtered):   no filter"),
    }
    println!();

    // Verify the session_manager accessor.
    if let McpConnectionParams::Stdio { command, .. } = toolset_all.session_manager().params() {
        println!("  \u{2713} session_manager() accessible: command={command:?}");
    }
    println!();

    // ────────────────────────────────────────────────────────────────────────
    // 4. Fluent T::mcp() shorthand
    // ────────────────────────────────────────────────────────────────────────
    println!("--- 4. T::mcp() fluent shorthand ---\n");

    // T::mcp() creates a ToolCompositeEntry::Mcp marker entry.
    // In a Live::builder() or AgentBuilder context, the runtime resolves
    // this entry at connect time, calling list_tools() and registering
    // each discovered tool as a FunctionDeclaration.
    let tools_composite = T::mcp("node ./my-mcp-server.js")
        | T::simple("local_echo", "Echo locally", |args| async move { Ok(args) });

    println!(
        "  T::mcp() | T::simple() composite: {} entries",
        tools_composite.len()
    );
    println!();

    // ────────────────────────────────────────────────────────────────────────
    // 5. Registering MCP tools in a Live session (config demo)
    // ────────────────────────────────────────────────────────────────────────
    println!("--- 5. Registering in a Live session ---\n");

    // In a real app with a running MCP server:
    //
    //   use std::sync::Arc;
    //   use gemini_adk_rs::tools::mcp::{McpConnectionParams, McpSessionManager, McpToolset};
    //   use std::time::Duration;
    //
    //   let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {
    //       command: "node".to_string(),
    //       args: vec!["./my-mcp-server.js".to_string()],
    //       timeout: Some(Duration::from_secs(30)),
    //   }));
    //
    //   // Discover tools at startup, before connecting the Live session.
    //   let tools = manager.list_tools().await?;
    //   println!("Discovered {} MCP tools", tools.len());
    //
    //   // Option A: T::mcp() fluent shorthand
    //   let handle = Live::builder()
    //       .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
    //       .with_tools(T::mcp("node ./my-mcp-server.js") | T::google_search())
    //       .connect_from_env()
    //       .await?;
    //
    //   // Option B: McpToolset for finer-grained control
    //   let toolset = McpToolset::new(manager).with_filter(vec!["search".to_string()]);
    //   let handle = Live::builder()
    //       .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
    //       // ... register toolset manually ...
    //       .connect_from_env()
    //       .await?;

    println!("  // With a running MCP server:");
    println!("  // let manager = Arc::new(McpSessionManager::new(McpConnectionParams::Stdio {{");
    println!("  //     command: \"node\".to_string(),");
    println!("  //     args: vec![\"./my-mcp-server.js\".to_string()],");
    println!("  //     timeout: Some(Duration::from_secs(30)),");
    println!("  // }}));");
    println!("  //");
    println!("  // let handle = Live::builder()");
    println!("  //     .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)");
    println!("  //     .with_tools(T::mcp(\"node ./my-mcp-server.js\"))");
    println!("  //     .connect_from_env()");
    println!("  //     .await?;");
    println!();

    // ────────────────────────────────────────────────────────────────────────
    // 6. Feature flags
    // ────────────────────────────────────────────────────────────────────────
    println!("--- 6. Feature flags ---\n");

    println!("  Default features:");
    println!("    MCP stdio transport is always available (no feature flag).");
    println!("    The SDK spawns a subprocess and speaks JSON-RPC over pipes.\n");

    println!("  Optional features:");
    println!("    mcp-http   \u{2192} enables SSE/StreamableHTTP transport via reqwest");
    println!(
        "               \u{2192} add to Cargo.toml: gemini-adk-rs = {{ features = [\"mcp-http\"] }}\n"
    );

    println!("  McpConnectionParams::Sse {{...}} always compiles, but calling");
    println!("  list_tools() / call_tool() on an Sse manager without the feature");
    println!("  returns McpError::ConnectionFailed(\"mcp-http feature not enabled\").\n");

    // ────────────────────────────────────────────────────────────────────────
    // Summary
    // ────────────────────────────────────────────────────────────────────────
    println!("--- Summary ---\n");
    println!("  McpConnectionParams::Stdio{{ command, args, timeout }}");
    println!("      \u{2192} subprocess stdio transport (always available)");
    println!("  McpConnectionParams::Sse{{ url, headers }}");
    println!("      \u{2192} HTTP transport (requires feature = \"mcp-http\")");
    println!("  McpSessionManager::new(params)");
    println!("      \u{2192} lazy connect + JSON-RPC 2.0 dispatch");
    println!("  manager.list_tools().await");
    println!("      \u{2192} initialize handshake + tools/list \u{2192} Vec<McpToolInfo>");
    println!("  manager.call_tool(name, args).await");
    println!("      \u{2192} tools/call \u{2192} JSON result object");
    println!("  McpToolset::new(Arc::new(manager)).with_filter(names)");
    println!("      \u{2192} Toolset wrapper; register with Live::builder() or ToolDispatcher");
    println!("  T::mcp(params_string) | T::google_search()");
    println!("      \u{2192} fluent ToolComposite for Live session tool composition");

    println!("\n=== Done ===");
}
