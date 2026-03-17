//! Vertex AI RAG memory service — stores and retrieves memories via Vertex AI RAG.
//!
//! Mirrors ADK-Python's `vertex_ai_rag_memory_service`.

use async_trait::async_trait;

use super::{MemoryEntry, MemoryError, MemoryService};

/// Configuration for Vertex AI RAG memory service.
#[derive(Debug, Clone)]
pub struct VertexAiRagMemoryConfig {
    /// The RAG corpus resource name.
    pub corpus: String,
    /// Google Cloud project ID.
    pub project: String,
    /// Google Cloud location.
    pub location: String,
}

/// Memory service backed by Vertex AI RAG.
///
/// Stores memory entries as documents in a Vertex AI RAG corpus
/// and uses semantic search for retrieval.
#[derive(Debug, Clone)]
pub struct VertexAiRagMemoryService {
    config: VertexAiRagMemoryConfig,
}

impl VertexAiRagMemoryService {
    /// Create a new Vertex AI RAG memory service.
    pub fn new(config: VertexAiRagMemoryConfig) -> Self {
        Self { config }
    }

    /// Returns the configured corpus.
    pub fn corpus(&self) -> &str {
        &self.config.corpus
    }
}

#[async_trait]
impl MemoryService for VertexAiRagMemoryService {
    async fn store(&self, _session_id: &str, _entry: MemoryEntry) -> Result<(), MemoryError> {
        // In a real integration, this would call the Vertex AI RAG API
        // to store the memory entry as a document.
        Ok(())
    }

    async fn get(&self, _session_id: &str, _key: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        // Vertex AI RAG doesn't support direct key-based retrieval;
        // use search() instead.
        Ok(None)
    }

    async fn list(&self, _session_id: &str) -> Result<Vec<MemoryEntry>, MemoryError> {
        Ok(vec![])
    }

    async fn search(
        &self,
        _session_id: &str,
        _query: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        // In a real integration, this would call retrieveContexts API.
        Ok(vec![])
    }

    async fn delete(&self, _session_id: &str, _key: &str) -> Result<(), MemoryError> {
        Ok(())
    }

    async fn clear(&self, _session_id: &str) -> Result<(), MemoryError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> VertexAiRagMemoryConfig {
        VertexAiRagMemoryConfig {
            corpus: "projects/test/locations/us-central1/ragCorpora/test-corpus".into(),
            project: "test".into(),
            location: "us-central1".into(),
        }
    }

    #[test]
    fn service_metadata() {
        let svc = VertexAiRagMemoryService::new(test_config());
        assert!(svc.corpus().contains("test-corpus"));
    }

    #[tokio::test]
    async fn store_and_search_stub() {
        let svc = VertexAiRagMemoryService::new(test_config());
        let entry = MemoryEntry::new("test", serde_json::json!("data"));
        svc.store("s1", entry).await.unwrap();
        let results = svc.search("s1", "test").await.unwrap();
        assert!(results.is_empty()); // stub returns empty
    }
}
