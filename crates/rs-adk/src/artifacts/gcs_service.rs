//! GCS-backed artifact service (stub).
//!
//! Feature-gated behind `gcs-artifacts`. Provides the correct API surface
//! for a future Google Cloud Storage implementation. All methods currently
//! return `ArtifactError::Storage` until a stable GCS client is integrated.

use async_trait::async_trait;

use super::{Artifact, ArtifactError, ArtifactMetadata, ArtifactService};

/// GCS-backed artifact service.
///
/// Path format: `{app_name}/{session_id}/{artifact_name}/v{version}`
pub struct GcsArtifactService {
    bucket: String,
    app_name: String,
}

impl GcsArtifactService {
    /// Create a new GCS artifact service targeting the given bucket.
    pub fn new(bucket: impl Into<String>, app_name: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            app_name: app_name.into(),
        }
    }

    /// The bucket this service targets.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// The application name prefix used in object paths.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    #[allow(dead_code)] // Used by tests now; will be used by future GCS implementation
    fn object_path(&self, session_id: &str, name: &str, version: u32) -> String {
        format!("{}/{}/{}/v{}", self.app_name, session_id, name, version)
    }

    #[allow(dead_code)] // Used by tests now; will be used by future GCS implementation
    fn metadata_path(&self, session_id: &str, name: &str, version: u32) -> String {
        format!(
            "{}/{}/{}/v{}/metadata.json",
            self.app_name, session_id, name, version
        )
    }
}

#[async_trait]
impl ArtifactService for GcsArtifactService {
    async fn save(
        &self,
        _session_id: &str,
        _artifact: Artifact,
    ) -> Result<ArtifactMetadata, ArtifactError> {
        Err(ArtifactError::Storage(
            "GCS artifact service not yet fully implemented — awaiting cloud-storage integration"
                .into(),
        ))
    }

    async fn load(
        &self,
        _session_id: &str,
        _name: &str,
    ) -> Result<Option<Artifact>, ArtifactError> {
        Err(ArtifactError::Storage(
            "GCS artifact service not yet fully implemented".into(),
        ))
    }

    async fn load_version(
        &self,
        _session_id: &str,
        _name: &str,
        _version: u32,
    ) -> Result<Option<Artifact>, ArtifactError> {
        Err(ArtifactError::Storage(
            "GCS artifact service not yet fully implemented".into(),
        ))
    }

    async fn list(
        &self,
        _session_id: &str,
    ) -> Result<Vec<ArtifactMetadata>, ArtifactError> {
        Err(ArtifactError::Storage(
            "GCS artifact service not yet fully implemented".into(),
        ))
    }

    async fn delete(
        &self,
        _session_id: &str,
        _name: &str,
    ) -> Result<(), ArtifactError> {
        Err(ArtifactError::Storage(
            "GCS artifact service not yet fully implemented".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_construct() {
        let svc = GcsArtifactService::new("my-bucket", "my-app");
        assert_eq!(svc.bucket(), "my-bucket");
        assert_eq!(svc.app_name(), "my-app");
    }

    #[test]
    fn object_path_format() {
        let svc = GcsArtifactService::new("bucket", "app");
        assert_eq!(svc.object_path("sess1", "file.bin", 3), "app/sess1/file.bin/v3");
    }

    #[test]
    fn metadata_path_format() {
        let svc = GcsArtifactService::new("bucket", "app");
        assert_eq!(
            svc.metadata_path("sess1", "file.bin", 2),
            "app/sess1/file.bin/v2/metadata.json"
        );
    }

    #[test]
    fn implements_artifact_service_trait() {
        // Verify GcsArtifactService satisfies the ArtifactService trait bound
        fn _assert_trait(_: &dyn ArtifactService) {}
        let svc = GcsArtifactService::new("b", "a");
        _assert_trait(&svc);
    }

    #[tokio::test]
    async fn stub_returns_storage_error() {
        let svc = GcsArtifactService::new("bucket", "app");
        let result = svc.load("s1", "x").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ArtifactError::Storage(_)));
    }
}
