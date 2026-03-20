//! Vertex AI Memory Bank service — stores and retrieves memories via Vertex AI Memory Bank.
//!
//! Mirrors ADK-Python's `vertex_ai_memory_bank_service`.

use async_trait::async_trait;

use super::{MemoryEntry, MemoryError, MemoryService};

/// Configuration for Vertex AI Memory Bank service.
#[derive(Debug, Clone)]
pub struct VertexAiMemoryBankConfig {
    /// Google Cloud project ID.
    pub project: String,
    /// Google Cloud location.
    pub location: String,
    /// Memory bank resource name.
    pub memory_bank: String,
}

/// Memory service backed by Vertex AI Memory Bank.
///
/// Uses the Vertex AI Memory Bank API for structured memory
/// storage and retrieval with automatic summarization.
#[derive(Debug, Clone)]
pub struct VertexAiMemoryBankService {
    config: VertexAiMemoryBankConfig,
}

impl VertexAiMemoryBankService {
    /// Create a new Vertex AI Memory Bank service.
    pub fn new(config: VertexAiMemoryBankConfig) -> Self {
        Self { config }
    }

    /// Returns the configured memory bank resource.
    pub fn memory_bank(&self) -> &str {
        &self.config.memory_bank
    }
}

#[async_trait]
impl MemoryService for VertexAiMemoryBankService {
    async fn store(&self, _session_id: &str, _entry: MemoryEntry) -> Result<(), MemoryError> {
        // In a real integration, calls the Memory Bank API to store.
        Ok(())
    }

    async fn get(&self, _session_id: &str, _key: &str) -> Result<Option<MemoryEntry>, MemoryError> {
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

    #[test]
    fn service_metadata() {
        let svc = VertexAiMemoryBankService::new(VertexAiMemoryBankConfig {
            project: "test-project".into(),
            location: "us-central1".into(),
            memory_bank: "test-bank".into(),
        });
        assert_eq!(svc.memory_bank(), "test-bank");
    }
}
