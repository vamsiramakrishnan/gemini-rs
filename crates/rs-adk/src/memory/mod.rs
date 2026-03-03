//! Memory service — session-scoped memory for agents.
//!
//! Mirrors ADK-JS's `BaseMemoryService`. Provides a trait for storing and
//! searching memory entries (key-value) with an in-memory default.

mod in_memory;

pub use in_memory::InMemoryMemoryService;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A memory entry — a named piece of information stored by an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique key for this memory.
    pub key: String,
    /// The stored value.
    pub value: serde_json::Value,
    /// When this entry was created (Unix timestamp seconds).
    pub created_at: u64,
    /// When this entry was last updated (Unix timestamp seconds).
    pub updated_at: u64,
}

impl MemoryEntry {
    /// Create a new memory entry.
    pub fn new(key: impl Into<String>, value: serde_json::Value) -> Self {
        let now = now_secs();
        Self {
            key: key.into(),
            value,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Errors from memory service operations.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// The requested memory key was not found.
    #[error("Memory key not found: {0}")]
    NotFound(String),
    /// A storage backend error.
    #[error("Storage error: {0}")]
    Storage(String),
}

/// Trait for session-scoped memory persistence.
///
/// Memory is scoped to a session ID. Implementations must be `Send + Sync`.
#[async_trait]
pub trait MemoryService: Send + Sync {
    /// Store a memory entry for a session.
    async fn store(
        &self,
        session_id: &str,
        entry: MemoryEntry,
    ) -> Result<(), MemoryError>;

    /// Retrieve a memory entry by key.
    async fn get(
        &self,
        session_id: &str,
        key: &str,
    ) -> Result<Option<MemoryEntry>, MemoryError>;

    /// List all memory entries for a session.
    async fn list(
        &self,
        session_id: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// Search memory entries by a query string (simple substring match in default impl).
    async fn search(
        &self,
        session_id: &str,
        query: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// Delete a memory entry.
    async fn delete(
        &self,
        session_id: &str,
        key: &str,
    ) -> Result<(), MemoryError>;

    /// Clear all memory for a session.
    async fn clear(&self, session_id: &str) -> Result<(), MemoryError>;
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_entry_new() {
        let entry = MemoryEntry::new("topic", serde_json::json!("Rust"));
        assert_eq!(entry.key, "topic");
        assert_eq!(entry.value, serde_json::json!("Rust"));
        assert!(entry.created_at > 0);
    }

    #[test]
    fn memory_service_is_object_safe() {
        fn _assert(_: &dyn MemoryService) {}
    }

    #[tokio::test]
    async fn store_and_get() {
        let svc = InMemoryMemoryService::new();
        let entry = MemoryEntry::new("topic", serde_json::json!("AI"));
        svc.store("s1", entry).await.unwrap();

        let fetched = svc.get("s1", "topic").await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().value, serde_json::json!("AI"));
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let svc = InMemoryMemoryService::new();
        let fetched = svc.get("s1", "missing").await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn list_entries() {
        let svc = InMemoryMemoryService::new();
        svc.store("s1", MemoryEntry::new("a", serde_json::json!(1)))
            .await
            .unwrap();
        svc.store("s1", MemoryEntry::new("b", serde_json::json!(2)))
            .await
            .unwrap();
        svc.store("s2", MemoryEntry::new("c", serde_json::json!(3)))
            .await
            .unwrap();

        let entries = svc.list("s1").await.unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn search_entries() {
        let svc = InMemoryMemoryService::new();
        svc.store("s1", MemoryEntry::new("rust_topic", serde_json::json!("Rust programming")))
            .await
            .unwrap();
        svc.store("s1", MemoryEntry::new("python_topic", serde_json::json!("Python scripting")))
            .await
            .unwrap();

        let results = svc.search("s1", "rust").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "rust_topic");
    }

    #[tokio::test]
    async fn delete_entry() {
        let svc = InMemoryMemoryService::new();
        svc.store("s1", MemoryEntry::new("k", serde_json::json!(1)))
            .await
            .unwrap();
        svc.delete("s1", "k").await.unwrap();
        let fetched = svc.get("s1", "k").await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn clear_session() {
        let svc = InMemoryMemoryService::new();
        svc.store("s1", MemoryEntry::new("a", serde_json::json!(1)))
            .await
            .unwrap();
        svc.store("s1", MemoryEntry::new("b", serde_json::json!(2)))
            .await
            .unwrap();
        svc.clear("s1").await.unwrap();
        let entries = svc.list("s1").await.unwrap();
        assert!(entries.is_empty());
    }
}
