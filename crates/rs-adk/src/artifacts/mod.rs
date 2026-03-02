//! Artifact service — versioned binary/JSON artifact storage.
//!
//! Mirrors ADK-JS's `BaseArtifactService`. Provides a trait for storing
//! and retrieving versioned artifacts with an in-memory default.

mod file_service;
mod forwarding;
#[cfg(feature = "gcs-artifacts")]
mod gcs_service;
mod in_memory;

pub use file_service::FileArtifactService;
pub use forwarding::ForwardingArtifactService;
#[cfg(feature = "gcs-artifacts")]
pub use gcs_service::GcsArtifactService;
pub use in_memory::InMemoryArtifactService;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Metadata for a stored artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactMetadata {
    /// Artifact name/key.
    pub name: String,
    /// MIME type (e.g., "application/json", "image/png").
    pub mime_type: String,
    /// Current version number (1-based).
    pub version: u32,
    /// Size in bytes.
    pub size: usize,
    /// When created (Unix timestamp seconds).
    pub created_at: u64,
    /// When last updated (Unix timestamp seconds).
    pub updated_at: u64,
}

/// A versioned artifact with data and metadata.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// Artifact metadata.
    pub metadata: ArtifactMetadata,
    /// The artifact data.
    pub data: Vec<u8>,
}

impl Artifact {
    /// Create a new artifact.
    pub fn new(
        name: impl Into<String>,
        mime_type: impl Into<String>,
        data: Vec<u8>,
    ) -> Self {
        let now = now_secs();
        let size = data.len();
        Self {
            metadata: ArtifactMetadata {
                name: name.into(),
                mime_type: mime_type.into(),
                version: 1,
                size,
                created_at: now,
                updated_at: now,
            },
            data,
        }
    }

    /// Create a JSON artifact.
    pub fn json(name: impl Into<String>, value: &serde_json::Value) -> Self {
        let data = serde_json::to_vec(value).unwrap_or_default();
        Self::new(name, "application/json", data)
    }

    /// Create a text artifact.
    pub fn text(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(name, "text/plain", text.into().into_bytes())
    }
}

/// Errors from artifact service operations.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("Artifact not found: {0}")]
    NotFound(String),
    #[error("Version not found: {name} v{version}")]
    VersionNotFound { name: String, version: u32 },
    #[error("Storage error: {0}")]
    Storage(String),
}

/// Trait for artifact persistence — CRUD with versioning.
///
/// Artifacts are scoped by session ID and identified by name.
/// Each update creates a new version.
#[async_trait]
pub trait ArtifactService: Send + Sync {
    /// Save an artifact, creating a new version if it already exists.
    async fn save(
        &self,
        session_id: &str,
        artifact: Artifact,
    ) -> Result<ArtifactMetadata, ArtifactError>;

    /// Load the latest version of an artifact.
    async fn load(
        &self,
        session_id: &str,
        name: &str,
    ) -> Result<Option<Artifact>, ArtifactError>;

    /// Load a specific version of an artifact.
    async fn load_version(
        &self,
        session_id: &str,
        name: &str,
        version: u32,
    ) -> Result<Option<Artifact>, ArtifactError>;

    /// List all artifact metadata for a session.
    async fn list(
        &self,
        session_id: &str,
    ) -> Result<Vec<ArtifactMetadata>, ArtifactError>;

    /// Delete all versions of an artifact.
    async fn delete(
        &self,
        session_id: &str,
        name: &str,
    ) -> Result<(), ArtifactError>;
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_new() {
        let a = Artifact::new("file.bin", "application/octet-stream", vec![1, 2, 3]);
        assert_eq!(a.metadata.name, "file.bin");
        assert_eq!(a.metadata.mime_type, "application/octet-stream");
        assert_eq!(a.metadata.version, 1);
        assert_eq!(a.metadata.size, 3);
        assert_eq!(a.data, vec![1, 2, 3]);
    }

    #[test]
    fn artifact_json() {
        let val = serde_json::json!({"key": "value"});
        let a = Artifact::json("config", &val);
        assert_eq!(a.metadata.mime_type, "application/json");
        let parsed: serde_json::Value = serde_json::from_slice(&a.data).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn artifact_text() {
        let a = Artifact::text("readme", "Hello, world!");
        assert_eq!(a.metadata.mime_type, "text/plain");
        assert_eq!(std::str::from_utf8(&a.data).unwrap(), "Hello, world!");
    }

    #[test]
    fn artifact_service_is_object_safe() {
        fn _assert(_: &dyn ArtifactService) {}
    }

    #[tokio::test]
    async fn save_and_load() {
        let svc = InMemoryArtifactService::new();
        let artifact = Artifact::text("notes", "First version");
        svc.save("s1", artifact).await.unwrap();

        let loaded = svc.load("s1", "notes").await.unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(std::str::from_utf8(&loaded.data).unwrap(), "First version");
        assert_eq!(loaded.metadata.version, 1);
    }

    #[tokio::test]
    async fn versioning() {
        let svc = InMemoryArtifactService::new();
        svc.save("s1", Artifact::text("notes", "v1")).await.unwrap();
        svc.save("s1", Artifact::text("notes", "v2")).await.unwrap();
        svc.save("s1", Artifact::text("notes", "v3")).await.unwrap();

        // Latest should be v3
        let latest = svc.load("s1", "notes").await.unwrap().unwrap();
        assert_eq!(latest.metadata.version, 3);
        assert_eq!(std::str::from_utf8(&latest.data).unwrap(), "v3");

        // Load specific version
        let v1 = svc.load_version("s1", "notes", 1).await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&v1.data).unwrap(), "v1");

        let v2 = svc.load_version("s1", "notes", 2).await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&v2.data).unwrap(), "v2");
    }

    #[tokio::test]
    async fn load_nonexistent_returns_none() {
        let svc = InMemoryArtifactService::new();
        let result = svc.load("s1", "missing").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn list_artifacts() {
        let svc = InMemoryArtifactService::new();
        svc.save("s1", Artifact::text("a", "data")).await.unwrap();
        svc.save("s1", Artifact::text("b", "data")).await.unwrap();
        svc.save("s2", Artifact::text("c", "data")).await.unwrap();

        let list = svc.list("s1").await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn delete_artifact() {
        let svc = InMemoryArtifactService::new();
        svc.save("s1", Artifact::text("notes", "data")).await.unwrap();
        svc.delete("s1", "notes").await.unwrap();
        let result = svc.load("s1", "notes").await.unwrap();
        assert!(result.is_none());
    }
}
