//! Base retrieval tool trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ToolError;

/// A single retrieval result — a document chunk with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalResult {
    /// The retrieved text content.
    pub content: String,
    /// Source identifier (e.g., file path, URL, document ID).
    pub source: String,
    /// Relevance score (0.0–1.0, higher is more relevant).
    pub score: f64,
    /// Optional metadata about the retrieved chunk.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Trait for retrieval tools that fetch relevant context.
///
/// Implementations search over a corpus and return ranked results
/// that can be injected into the LLM context.
#[async_trait]
pub trait BaseRetrievalTool: Send + Sync {
    /// The name of this retrieval tool.
    fn name(&self) -> &str;

    /// Search the corpus with a query and return ranked results.
    async fn retrieve(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<RetrievalResult>, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_object_safe(_: &dyn BaseRetrievalTool) {}

    #[test]
    fn retrieval_result_serde() {
        let result = RetrievalResult {
            content: "Test content".into(),
            source: "doc.txt".into(),
            score: 0.95,
            metadata: serde_json::json!({"page": 1}),
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: RetrievalResult = serde_json::from_str(&json).unwrap();
        assert!((deserialized.score - 0.95).abs() < f64::EPSILON);
    }
}
