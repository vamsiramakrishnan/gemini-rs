//! In-memory artifact service using DashMap.

use async_trait::async_trait;
use dashmap::DashMap;

use super::{now_secs, Artifact, ArtifactError, ArtifactMetadata, ArtifactService};

/// In-memory artifact service backed by [`DashMap`] for lock-free concurrent access.
///
/// Each artifact name can have multiple versions. Suitable for testing,
/// prototyping, and single-process deployments. Data is lost on process restart.
pub struct InMemoryArtifactService {
    /// Maps session ID to artifact name to version history.
    store: DashMap<String, DashMap<String, Vec<Artifact>>>,
}

impl InMemoryArtifactService {
    /// Create a new in-memory artifact service.
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
        }
    }
}

impl Default for InMemoryArtifactService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ArtifactService for InMemoryArtifactService {
    async fn save(
        &self,
        session_id: &str,
        mut artifact: Artifact,
    ) -> Result<ArtifactMetadata, ArtifactError> {
        let session = self
            .store
            .entry(session_id.to_string())
            .or_default();

        let mut versions = session
            .entry(artifact.metadata.name.clone())
            .or_default();

        let version = versions.len() as u32 + 1;
        artifact.metadata.version = version;
        artifact.metadata.updated_at = now_secs();
        if version == 1 {
            artifact.metadata.created_at = artifact.metadata.updated_at;
        }

        let metadata = artifact.metadata.clone();
        versions.push(artifact);
        Ok(metadata)
    }

    async fn load(
        &self,
        session_id: &str,
        name: &str,
    ) -> Result<Option<Artifact>, ArtifactError> {
        let result = self
            .store
            .get(session_id)
            .and_then(|session| {
                session
                    .get(name)
                    .and_then(|versions| versions.last().cloned())
            });
        Ok(result)
    }

    async fn load_version(
        &self,
        session_id: &str,
        name: &str,
        version: u32,
    ) -> Result<Option<Artifact>, ArtifactError> {
        let result = self
            .store
            .get(session_id)
            .and_then(|session| {
                session.get(name).and_then(|versions| {
                    if version == 0 || version as usize > versions.len() {
                        None
                    } else {
                        Some(versions[(version - 1) as usize].clone())
                    }
                })
            });
        Ok(result)
    }

    async fn list(
        &self,
        session_id: &str,
    ) -> Result<Vec<ArtifactMetadata>, ArtifactError> {
        let result = self
            .store
            .get(session_id)
            .map(|session| {
                session
                    .iter()
                    .filter_map(|entry| entry.value().last().map(|a| a.metadata.clone()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(result)
    }

    async fn delete(
        &self,
        session_id: &str,
        name: &str,
    ) -> Result<(), ArtifactError> {
        if let Some(session) = self.store.get(session_id) {
            session.remove(name);
        }
        Ok(())
    }
}
