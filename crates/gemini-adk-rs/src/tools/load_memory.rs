//! Load memory tool — allows agents to search their memory store.
//!
//! Mirrors ADK-Python's `load_memory_tool`. Provides the model with a tool to
//! search session memory using a query string. This tool is *local*: it simply
//! delegates to whatever [`MemoryService`] is wired into the session, mirroring
//! ADK's `load_memory` which calls `tool_context.search_memory(query)`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ToolError;
use crate::memory::MemoryService;
use crate::tool::ToolFunction;

/// Scope used when delegating to the [`MemoryService`] for a search.
#[derive(Debug, Clone, Default)]
struct MemoryScope {
    session_id: String,
}

/// Tool that searches the agent's memory store.
///
/// When the model needs to recall previously stored information, it can call
/// this tool with a search query. If a [`MemoryService`] is wired via
/// [`with_memory_service`](LoadMemoryTool::with_memory_service), the call is
/// delegated to it; otherwise a placeholder response is returned (matching the
/// "runtime intercepts the call" model).
#[derive(Clone, Default)]
pub struct LoadMemoryTool {
    service: Option<Arc<dyn MemoryService>>,
    scope: MemoryScope,
}

impl std::fmt::Debug for LoadMemoryTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadMemoryTool")
            .field("has_service", &self.service.is_some())
            .field("session_id", &self.scope.session_id)
            .finish()
    }
}

impl LoadMemoryTool {
    /// Create a new load memory tool with no memory service wired.
    ///
    /// Without a service, [`call`](ToolFunction::call) returns a placeholder
    /// indicating the query was received (the runtime is expected to intercept).
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire a [`MemoryService`] that this tool delegates searches to.
    pub fn with_memory_service(mut self, service: Arc<dyn MemoryService>) -> Self {
        self.service = Some(service);
        self
    }

    /// Set the session ID used to scope memory searches.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.scope.session_id = session_id.into();
        self
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
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

        match &self.service {
            // Delegate to the wired MemoryService, mirroring ADK's
            // `tool_context.search_memory(query)`.
            Some(service) => {
                let memories = service
                    .search(&self.scope.session_id, query)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

                Ok(serde_json::json!({
                    "memories": memories,
                }))
            }
            // No service wired — the runtime is expected to intercept this call
            // and route it to the MemoryService.
            None => Ok(serde_json::json!({
                "status": "memory_search_requested",
                "query": query,
                "results": []
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{InMemoryMemoryService, MemoryEntry};
    use serde_json::json;

    #[test]
    fn tool_metadata() {
        let tool = LoadMemoryTool::new();
        assert_eq!(tool.name(), "load_memory");
        assert!(tool.description().contains("memory"));
        assert!(tool.parameters().is_some());
    }

    #[tokio::test]
    async fn call_with_query_no_service() {
        let tool = LoadMemoryTool::new();
        let result = tool
            .call(json!({"query": "user preferences"}))
            .await
            .unwrap();
        assert_eq!(result["query"], "user preferences");
        assert_eq!(result["status"], "memory_search_requested");
    }

    #[tokio::test]
    async fn call_delegates_to_memory_service() {
        let svc = Arc::new(InMemoryMemoryService::new());
        svc.store(
            "s1",
            MemoryEntry::new("rust_topic", json!("Rust programming")),
        )
        .await
        .unwrap();

        let tool = LoadMemoryTool::new()
            .with_memory_service(svc)
            .with_session_id("s1");

        let result = tool.call(json!({"query": "rust"})).await.unwrap();
        let memories = result["memories"].as_array().expect("memories array");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0]["key"], "rust_topic");
    }
}
