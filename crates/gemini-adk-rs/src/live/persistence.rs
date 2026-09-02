//! Session persistence — survive process restarts.
//!
//! The Gemini Live API supports session resumption via opaque handles.
//! This module persists the SDK's client-side state (State, phase position,
//! transcript summary) so it can be restored on reconnection.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Serializable snapshot of the control plane state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// All state key-value pairs.
    pub state: HashMap<String, Value>,
    /// Current phase name.
    pub phase: String,
    /// Turn count at time of snapshot.
    pub turn_count: u32,
    /// Human-readable summary of recent transcript.
    pub transcript_summary: String,
    /// Resume handle from the Gemini server.
    pub resume_handle: Option<String>,
    /// ISO 8601 timestamp.
    pub saved_at: String,
}

/// Error returned by a [`SessionPersistence`] backend.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    /// Filesystem or network I/O failed.
    #[error("persistence I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A snapshot could not be encoded or decoded.
    #[error("persistence serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// No snapshot is stored under the given session id (for backends that
    /// distinguish this from an empty `load`).
    #[error("no persisted session '{0}'")]
    NotFound(String),
    /// A backend-specific failure (Redis, Firestore, DynamoDB, …).
    #[error("persistence backend error: {0}")]
    Backend(String),
}

/// Trait for persisting session state across process restarts.
///
/// Implementations might write to the filesystem, Redis, Firestore, etc.
#[async_trait]
pub trait SessionPersistence: Send + Sync {
    /// Save a session snapshot.
    async fn save(
        &self,
        session_id: &str,
        snapshot: &SessionSnapshot,
    ) -> Result<(), PersistenceError>;

    /// Load a previously saved session snapshot.
    async fn load(&self, session_id: &str) -> Result<Option<SessionSnapshot>, PersistenceError>;

    /// Delete a saved session.
    async fn delete(&self, session_id: &str) -> Result<(), PersistenceError>;
}

/// File-system persistence (good for development and single-server deployments).
pub struct FsPersistence {
    dir: PathBuf,
}

impl FsPersistence {
    /// Create a new file-system persistence backend.
    ///
    /// The directory will be created if it doesn't exist.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{session_id}.json"))
    }

    fn tmp_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{session_id}.json.tmp"))
    }
}

#[async_trait]
impl SessionPersistence for FsPersistence {
    async fn save(
        &self,
        session_id: &str,
        snapshot: &SessionSnapshot,
    ) -> Result<(), PersistenceError> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let json = serde_json::to_string_pretty(snapshot)?;
        // Write to a sibling temp file, then atomically rename over the
        // destination. `rename(2)` is atomic on the same filesystem, so a
        // crash mid-write (or a concurrent `load`) only ever observes the
        // previous complete snapshot or the new complete snapshot — never a
        // torn half-write.
        let tmp = self.tmp_path(session_id);
        tokio::fs::write(&tmp, json).await?;
        tokio::fs::rename(&tmp, self.path(session_id)).await?;
        Ok(())
    }

    async fn load(&self, session_id: &str) -> Result<Option<SessionSnapshot>, PersistenceError> {
        let path = self.path(session_id);
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete(&self, session_id: &str) -> Result<(), PersistenceError> {
        let path = self.path(session_id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// In-memory persistence (good for tests).
pub struct MemoryPersistence {
    store: std::sync::Arc<dashmap::DashMap<String, SessionSnapshot>>,
}

impl MemoryPersistence {
    /// Create a new in-memory persistence backend.
    pub fn new() -> Self {
        Self {
            store: std::sync::Arc::new(dashmap::DashMap::new()),
        }
    }
}

impl Default for MemoryPersistence {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionPersistence for MemoryPersistence {
    async fn save(
        &self,
        session_id: &str,
        snapshot: &SessionSnapshot,
    ) -> Result<(), PersistenceError> {
        self.store.insert(session_id.to_string(), snapshot.clone());
        Ok(())
    }

    async fn load(&self, session_id: &str) -> Result<Option<SessionSnapshot>, PersistenceError> {
        Ok(self.store.get(session_id).map(|v| v.value().clone()))
    }

    async fn delete(&self, session_id: &str) -> Result<(), PersistenceError> {
        self.store.remove(session_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_persistence_round_trip() {
        let p = MemoryPersistence::new();
        let snapshot = SessionSnapshot {
            state: [("name".into(), Value::String("Alice".into()))]
                .into_iter()
                .collect(),
            phase: "greeting".into(),
            turn_count: 5,
            transcript_summary: "User: Hello\nAssistant: Hi!".into(),
            resume_handle: Some("handle-123".into()),
            saved_at: "2026-03-07T00:00:00Z".into(),
        };

        p.save("session-1", &snapshot).await.unwrap();

        let loaded = p.load("session-1").await.unwrap().unwrap();
        assert_eq!(loaded.phase, "greeting");
        assert_eq!(loaded.turn_count, 5);
        assert_eq!(loaded.resume_handle, Some("handle-123".into()));
    }

    #[tokio::test]
    async fn memory_persistence_load_missing() {
        let p = MemoryPersistence::new();
        assert!(p.load("nonexistent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn memory_persistence_delete() {
        let p = MemoryPersistence::new();
        let snapshot = SessionSnapshot {
            state: HashMap::new(),
            phase: "test".into(),
            turn_count: 0,
            transcript_summary: String::new(),
            resume_handle: None,
            saved_at: "2026-03-07T00:00:00Z".into(),
        };

        p.save("session-1", &snapshot).await.unwrap();
        p.delete("session-1").await.unwrap();
        assert!(p.load("session-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn fs_persistence_round_trip() {
        let dir = std::env::temp_dir().join("gemini_rs_test_persistence");
        let p = FsPersistence::new(&dir);
        let snapshot = SessionSnapshot {
            state: [("key".into(), Value::from(42))].into_iter().collect(),
            phase: "main".into(),
            turn_count: 3,
            transcript_summary: "test".into(),
            resume_handle: None,
            saved_at: "2026-03-07T00:00:00Z".into(),
        };

        p.save("test-session", &snapshot).await.unwrap();
        let loaded = p.load("test-session").await.unwrap().unwrap();
        assert_eq!(loaded.phase, "main");

        // Cleanup
        p.delete("test-session").await.unwrap();
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn fs_persistence_save_is_atomic_and_leaves_no_tmp_file() {
        let dir = std::env::temp_dir().join(format!(
            "gemini_rs_test_persistence_atomic_{}",
            uuid::Uuid::new_v4()
        ));
        let p = FsPersistence::new(&dir);
        let snapshot = SessionSnapshot {
            state: HashMap::new(),
            phase: "main".into(),
            turn_count: 1,
            transcript_summary: "x".into(),
            resume_handle: None,
            saved_at: "now".into(),
        };

        p.save("atomic-session", &snapshot).await.unwrap();

        assert!(
            !p.tmp_path("atomic-session").exists(),
            "tmp file must be renamed away after save"
        );
        assert!(p.path("atomic-session").exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs_persistence_concurrent_loads_never_observe_torn_snapshot() {
        // Hammer save() while load()ing concurrently: with the tmp+rename
        // scheme every load parses; with the old direct `fs::write` a reader
        // could observe a truncated/partial file mid-write.
        let dir = std::env::temp_dir().join(format!(
            "gemini_rs_test_persistence_torn_{}",
            uuid::Uuid::new_v4()
        ));
        let p = std::sync::Arc::new(FsPersistence::new(&dir));

        // A snapshot large enough that a write is not a single tiny syscall.
        let big = "x".repeat(256 * 1024);
        let snapshot = SessionSnapshot {
            state: [("blob".to_string(), Value::String(big))]
                .into_iter()
                .collect(),
            phase: "main".into(),
            turn_count: 0,
            transcript_summary: String::new(),
            resume_handle: None,
            saved_at: "now".into(),
        };
        p.save("torn", &snapshot).await.unwrap();

        let writer = {
            let p = p.clone();
            let snapshot = snapshot.clone();
            tokio::spawn(async move {
                for _ in 0..50 {
                    p.save("torn", &snapshot).await.unwrap();
                }
            })
        };

        for _ in 0..200 {
            let loaded = p
                .load("torn")
                .await
                .expect("load must never observe a torn snapshot");
            assert!(loaded.is_some(), "snapshot must always be present");
        }

        writer.await.unwrap();
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
