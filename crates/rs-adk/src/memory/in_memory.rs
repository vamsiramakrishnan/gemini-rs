//! In-memory memory service using DashMap.

use async_trait::async_trait;
use dashmap::DashMap;

use super::{MemoryEntry, MemoryError, MemoryService};

/// In-memory memory service backed by [`DashMap`] for lock-free concurrent access.
///
/// Memory entries are scoped by session ID. Suitable for testing, prototyping,
/// and single-process deployments. Data is lost on process restart.
pub struct InMemoryMemoryService {
    /// session_id → (key → MemoryEntry)
    store: DashMap<String, DashMap<String, MemoryEntry>>,
}

impl InMemoryMemoryService {
    /// Create a new in-memory memory service.
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
        }
    }
}

impl Default for InMemoryMemoryService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryService for InMemoryMemoryService {
    async fn store(
        &self,
        session_id: &str,
        entry: MemoryEntry,
    ) -> Result<(), MemoryError> {
        let session = self.store
            .entry(session_id.to_string())
            .or_default();
        session.insert(entry.key.clone(), entry);
        Ok(())
    }

    async fn get(
        &self,
        session_id: &str,
        key: &str,
    ) -> Result<Option<MemoryEntry>, MemoryError> {
        let result = self
            .store
            .get(session_id)
            .and_then(|session| session.get(key).map(|e| e.clone()));
        Ok(result)
    }

    async fn list(
        &self,
        session_id: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let entries = self
            .store
            .get(session_id)
            .map(|session| {
                session.iter().map(|e| e.value().clone()).collect()
            })
            .unwrap_or_default();
        Ok(entries)
    }

    async fn search(
        &self,
        session_id: &str,
        query: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let query_lower = query.to_lowercase();
        let entries = self
            .store
            .get(session_id)
            .map(|session| {
                session
                    .iter()
                    .filter(|e| {
                        e.key().to_lowercase().contains(&query_lower)
                            || e.value()
                                .value
                                .to_string()
                                .to_lowercase()
                                .contains(&query_lower)
                    })
                    .map(|e| e.value().clone())
                    .collect()
            })
            .unwrap_or_default();
        Ok(entries)
    }

    async fn delete(
        &self,
        session_id: &str,
        key: &str,
    ) -> Result<(), MemoryError> {
        if let Some(session) = self.store.get(session_id) {
            session.remove(key);
        }
        Ok(())
    }

    async fn clear(&self, session_id: &str) -> Result<(), MemoryError> {
        if let Some(session) = self.store.get(session_id) {
            session.clear();
        }
        Ok(())
    }
}
