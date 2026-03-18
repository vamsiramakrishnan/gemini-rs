//! Forwarding artifact service that proxies all calls to a parent service.

use std::sync::Arc;

use async_trait::async_trait;

use super::{Artifact, ArtifactError, ArtifactMetadata, ArtifactService};

/// Artifact service that proxies all calls to a parent service.
/// Used by ToolContext to give tools scoped artifact access.
pub struct ForwardingArtifactService {
    inner: Arc<dyn ArtifactService>,
}

impl ForwardingArtifactService {
    /// Create a new forwarding artifact service wrapping the given inner service.
    pub fn new(inner: Arc<dyn ArtifactService>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl ArtifactService for ForwardingArtifactService {
    async fn save(
        &self,
        session_id: &str,
        artifact: Artifact,
    ) -> Result<ArtifactMetadata, ArtifactError> {
        self.inner.save(session_id, artifact).await
    }

    async fn load(&self, session_id: &str, name: &str) -> Result<Option<Artifact>, ArtifactError> {
        self.inner.load(session_id, name).await
    }

    async fn load_version(
        &self,
        session_id: &str,
        name: &str,
        version: u32,
    ) -> Result<Option<Artifact>, ArtifactError> {
        self.inner.load_version(session_id, name, version).await
    }

    async fn list(&self, session_id: &str) -> Result<Vec<ArtifactMetadata>, ArtifactError> {
        self.inner.list(session_id).await
    }

    async fn delete(&self, session_id: &str, name: &str) -> Result<(), ArtifactError> {
        self.inner.delete(session_id, name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::InMemoryArtifactService;

    #[tokio::test]
    async fn forwarding_save_delegates() {
        let inner = Arc::new(InMemoryArtifactService::new());
        let fwd = ForwardingArtifactService::new(inner.clone());

        let artifact = Artifact::text("notes", "hello");
        let meta = fwd.save("s1", artifact).await.unwrap();
        assert_eq!(meta.name, "notes");
        assert_eq!(meta.version, 1);

        // Verify the inner service actually received the artifact.
        let loaded = inner.load("s1", "notes").await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&loaded.data).unwrap(), "hello");
    }

    #[tokio::test]
    async fn forwarding_load_delegates() {
        let inner = Arc::new(InMemoryArtifactService::new());
        inner
            .save("s1", Artifact::text("doc", "content"))
            .await
            .unwrap();

        let fwd = ForwardingArtifactService::new(inner);
        let loaded = fwd.load("s1", "doc").await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&loaded.data).unwrap(), "content");
    }

    #[tokio::test]
    async fn forwarding_load_version_delegates() {
        let inner = Arc::new(InMemoryArtifactService::new());
        inner.save("s1", Artifact::text("doc", "v1")).await.unwrap();
        inner.save("s1", Artifact::text("doc", "v2")).await.unwrap();

        let fwd = ForwardingArtifactService::new(inner);
        let v1 = fwd.load_version("s1", "doc", 1).await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&v1.data).unwrap(), "v1");
        let v2 = fwd.load_version("s1", "doc", 2).await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&v2.data).unwrap(), "v2");
    }

    #[tokio::test]
    async fn forwarding_list_delegates() {
        let inner = Arc::new(InMemoryArtifactService::new());
        inner.save("s1", Artifact::text("a", "data")).await.unwrap();
        inner.save("s1", Artifact::text("b", "data")).await.unwrap();

        let fwd = ForwardingArtifactService::new(inner);
        let list = fwd.list("s1").await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn forwarding_delete_delegates() {
        let inner = Arc::new(InMemoryArtifactService::new());
        inner
            .save("s1", Artifact::text("notes", "data"))
            .await
            .unwrap();

        let fwd = ForwardingArtifactService::new(inner.clone());
        fwd.delete("s1", "notes").await.unwrap();

        // Verify deletion happened in the inner service.
        let result = inner.load("s1", "notes").await.unwrap();
        assert!(result.is_none());
    }
}
