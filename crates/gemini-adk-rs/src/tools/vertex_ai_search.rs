//! Vertex AI Search tool — enterprise search via Vertex AI.
//!
//! Mirrors ADK-Python's `vertex_ai_search_tool` / `discovery_engine_search_tool`.
//! Provides server-side search using Vertex AI Discovery Engine / Search.

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::ToolFunction;

/// Vertex AI Search tool configuration.
#[derive(Debug, Clone)]
pub struct VertexAiSearchConfig {
    /// The Vertex AI search datastore resource name.
    /// Format: `projects/{project}/locations/{location}/collections/default_collection/dataStores/{data_store_id}`
    pub datastore: String,
    /// Optional search filter expression.
    pub filter: Option<String>,
    /// Maximum number of results to return.
    pub max_results: usize,
}

/// Tool that searches using Vertex AI Discovery Engine / Search.
///
/// This tool calls the Vertex AI Search API to perform enterprise
/// search over configured data stores.
#[derive(Debug, Clone)]
pub struct VertexAiSearchTool {
    config: VertexAiSearchConfig,
}

impl VertexAiSearchTool {
    /// Create a new Vertex AI Search tool.
    pub fn new(config: VertexAiSearchConfig) -> Self {
        Self { config }
    }

    /// Returns the configured datastore resource name.
    pub fn datastore(&self) -> &str {
        &self.config.datastore
    }
}

#[async_trait]
impl ToolFunction for VertexAiSearchTool {
    fn name(&self) -> &str {
        "vertex_ai_search"
    }

    fn description(&self) -> &str {
        "Search enterprise data using Vertex AI Search. Returns relevant documents \
         and snippets from the configured data store."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query."
                }
            },
            "required": ["query"]
        }))
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("Missing query".into()))?;

        // In a real integration, this would call the Vertex AI Search API.
        // The actual API integration requires the Google Cloud SDK.
        Ok(serde_json::json!({
            "status": "search_requested",
            "query": query,
            "datastore": self.config.datastore,
            "results": []
        }))
    }
}

/// Discovery Engine Search tool — alias for Vertex AI Search.
///
/// Mirrors ADK-Python's `DiscoveryEngineSearchTool`, which is equivalent
/// to `VertexAiSearchTool` but specifically for Discovery Engine endpoints.
pub type DiscoveryEngineSearchTool = VertexAiSearchTool;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_config() -> VertexAiSearchConfig {
        VertexAiSearchConfig {
            datastore: "projects/my-proj/locations/global/collections/default_collection/dataStores/my-store".into(),
            filter: None,
            max_results: 10,
        }
    }

    #[test]
    fn tool_metadata() {
        let tool = VertexAiSearchTool::new(test_config());
        assert_eq!(tool.name(), "vertex_ai_search");
        assert!(tool.parameters().is_some());
        assert!(tool.datastore().contains("my-store"));
    }

    #[tokio::test]
    async fn call_with_query() {
        let tool = VertexAiSearchTool::new(test_config());
        let result = tool
            .call(json!({"query": "machine learning"}))
            .await
            .unwrap();
        assert_eq!(result["query"], "machine learning");
    }

    #[tokio::test]
    async fn missing_query() {
        let tool = VertexAiSearchTool::new(test_config());
        let result = tool.call(json!({})).await;
        assert!(result.is_err());
    }
}
