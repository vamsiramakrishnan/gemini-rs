//! Load memory tool — allows agents to search their memory store.
//!
//! Mirrors ADK-Python's `load_memory_tool`. Provides the model with
//! a tool to search session memory using a query string.

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::ToolFunction;

/// Tool that searches the agent's memory store.
///
/// When the model needs to recall previously stored information,
/// it can call this tool with a search query.
#[derive(Debug, Clone, Default)]
pub struct LoadMemoryTool;

impl LoadMemoryTool {
    /// Create a new load memory tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolFunction for LoadMemoryTool {
    fn name(&self) -> &str {
        "load_memory"
    }

    fn description(&self) -> &str {
        "Search and load relevant information from the agent's memory. \
         Call this function with a query to retrieve previously stored memories."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to find relevant memories."
                }
            },
            "required": ["query"]
        }))
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // The actual memory search is performed by the runtime context.
        // This tool returns a placeholder indicating the query was received.
        // In a real integration, the runtime intercepts this tool call
        // and routes it to the MemoryService.
        Ok(serde_json::json!({
            "status": "memory_search_requested",
            "query": query,
            "results": []
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_metadata() {
        let tool = LoadMemoryTool::new();
        assert_eq!(tool.name(), "load_memory");
        assert!(tool.description().contains("memory"));
        assert!(tool.parameters().is_some());
    }

    #[tokio::test]
    async fn call_with_query() {
        let tool = LoadMemoryTool::new();
        let result = tool
            .call(json!({"query": "user preferences"}))
            .await
            .unwrap();
        assert_eq!(result["query"], "user preferences");
    }
}
