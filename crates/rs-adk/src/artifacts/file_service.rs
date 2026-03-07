//! Filesystem-backed artifact service with versioning.
//!
//! Directory layout: `{root}/{session_id}/{artifact_name}/v{version}/data`
//! Metadata stored as: `{root}/{session_id}/{artifact_name}/v{version}/metadata.json`

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::fs;

use super::{now_secs, Artifact, ArtifactError, ArtifactMetadata, ArtifactService};

/// Filesystem-backed artifact storage with versioning.
///
/// Each artifact version is stored in its own directory with a `data` file
/// and a `metadata.json` sidecar. Session IDs and artifact names are sanitized
/// to prevent path traversal attacks.
pub struct FileArtifactService {
    root_dir: PathBuf,
}

impl FileArtifactService {
    /// Create a new file artifact service rooted at the given directory.
    ///
    /// Creates the root directory if it doesn't exist.
    pub fn new(root_dir: impl Into<PathBuf>) -> Result<Self, ArtifactError> {
        let root = root_dir.into();
        // Create root dir if it doesn't exist (use std::fs since this is construction)
        std::fs::create_dir_all(&root)
            .map_err(|e| ArtifactError::Storage(format!("Failed to create root dir: {}", e)))?;
        Ok(Self { root_dir: root })
    }

    fn artifact_dir(&self, session_id: &str, name: &str) -> PathBuf {
        // Sanitize inputs to prevent path traversal
        let safe_session = sanitize_path_component(session_id);
        let safe_name = sanitize_path_component(name);
        self.root_dir.join(&safe_session).join(&safe_name)
    }

    fn version_dir(&self, session_id: &str, name: &str, version: u32) -> PathBuf {
        self.artifact_dir(session_id, name)
            .join(format!("v{}", version))
    }

    /// Get the next version number by counting existing version directories.
    async fn next_version(&self, session_id: &str, name: &str) -> u32 {
        let dir = self.artifact_dir(session_id, name);
        if !dir.exists() {
            return 1;
        }
        let mut max_version = 0u32;
        if let Ok(mut entries) = fs::read_dir(&dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(v) = name.strip_prefix('v') {
                        if let Ok(version) = v.parse::<u32>() {
                            max_version = max_version.max(version);
                        }
                    }
                }
            }
        }
        max_version + 1
    }
}

/// Replace path separators and ".." with underscores to prevent traversal.
fn sanitize_path_component(s: &str) -> String {
    s.replace(['/', '\\', '.'], "_")
}

#[async_trait]
impl ArtifactService for FileArtifactService {
    async fn save(
        &self,
        session_id: &str,
        artifact: Artifact,
    ) -> Result<ArtifactMetadata, ArtifactError> {
        let version = self.next_version(session_id, &artifact.metadata.name).await;
        let ver_dir = self.version_dir(session_id, &artifact.metadata.name, version);
        fs::create_dir_all(&ver_dir)
            .await
            .map_err(|e| ArtifactError::Storage(e.to_string()))?;

        // Write data
        fs::write(ver_dir.join("data"), &artifact.data)
            .await
            .map_err(|e| ArtifactError::Storage(e.to_string()))?;

        // Update metadata with correct version and write as JSON sidecar
        let mut metadata = artifact.metadata;
        metadata.version = version;
        metadata.updated_at = now_secs();
        if version == 1 {
            metadata.created_at = metadata.updated_at;
        }

        let metadata_json = serde_json::to_string_pretty(&metadata)
            .map_err(|e| ArtifactError::Storage(e.to_string()))?;
        fs::write(ver_dir.join("metadata.json"), metadata_json)
            .await
            .map_err(|e| ArtifactError::Storage(e.to_string()))?;

        Ok(metadata)
    }

    async fn load(&self, session_id: &str, name: &str) -> Result<Option<Artifact>, ArtifactError> {
        let latest = self.next_version(session_id, name).await;
        if latest == 1 {
            return Ok(None);
        }
        self.load_version(session_id, name, latest - 1).await
    }

    async fn load_version(
        &self,
        session_id: &str,
        name: &str,
        version: u32,
    ) -> Result<Option<Artifact>, ArtifactError> {
        let ver_dir = self.version_dir(session_id, name, version);
        if !ver_dir.exists() {
            return Ok(None);
        }

        let data = fs::read(ver_dir.join("data"))
            .await
            .map_err(|e| ArtifactError::Storage(e.to_string()))?;
        let metadata_str = fs::read_to_string(ver_dir.join("metadata.json"))
            .await
            .map_err(|e| ArtifactError::Storage(e.to_string()))?;
        let metadata: ArtifactMetadata = serde_json::from_str(&metadata_str)
            .map_err(|e| ArtifactError::Storage(e.to_string()))?;

        Ok(Some(Artifact { metadata, data }))
    }

    async fn list(&self, session_id: &str) -> Result<Vec<ArtifactMetadata>, ArtifactError> {
        let session_dir = self.root_dir.join(sanitize_path_component(session_id));
        if !session_dir.exists() {
            return Ok(vec![]);
        }

        let mut result = vec![];
        let mut entries = fs::read_dir(&session_dir)
            .await
            .map_err(|e| ArtifactError::Storage(e.to_string()))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| ArtifactError::Storage(e.to_string()))?
        {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                // Load latest version metadata
                if let Ok(Some(artifact)) = self.load(session_id, &name).await {
                    result.push(artifact.metadata);
                }
            }
        }
        Ok(result)
    }

    async fn delete(&self, session_id: &str, name: &str) -> Result<(), ArtifactError> {
        let dir = self.artifact_dir(session_id, name);
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .await
                .map_err(|e| ArtifactError::Storage(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Create a unique temp directory for each test.
    fn test_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir()
            .join("rs_adk_file_artifact_tests")
            .join(format!("test_{}_{}", std::process::id(), id));
        // Clean up any leftovers from previous runs
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn save_and_load_round_trip() {
        let dir = test_dir();
        let svc = FileArtifactService::new(&dir).unwrap();

        let artifact = Artifact::text("notes", "Hello, world!");
        let meta = svc.save("session1", artifact).await.unwrap();
        assert_eq!(meta.name, "notes");
        assert_eq!(meta.version, 1);

        let loaded = svc.load("session1", "notes").await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&loaded.data).unwrap(), "Hello, world!");
        assert_eq!(loaded.metadata.version, 1);
        assert_eq!(loaded.metadata.mime_type, "text/plain");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn versioning_increments_and_load_gets_latest() {
        let dir = test_dir();
        let svc = FileArtifactService::new(&dir).unwrap();

        let m1 = svc
            .save("s1", Artifact::text("doc", "version 1"))
            .await
            .unwrap();
        assert_eq!(m1.version, 1);

        let m2 = svc
            .save("s1", Artifact::text("doc", "version 2"))
            .await
            .unwrap();
        assert_eq!(m2.version, 2);

        let m3 = svc
            .save("s1", Artifact::text("doc", "version 3"))
            .await
            .unwrap();
        assert_eq!(m3.version, 3);

        // load() should return latest (v3)
        let latest = svc.load("s1", "doc").await.unwrap().unwrap();
        assert_eq!(latest.metadata.version, 3);
        assert_eq!(std::str::from_utf8(&latest.data).unwrap(), "version 3");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn load_specific_version() {
        let dir = test_dir();
        let svc = FileArtifactService::new(&dir).unwrap();

        svc.save("s1", Artifact::text("doc", "v1 data"))
            .await
            .unwrap();
        svc.save("s1", Artifact::text("doc", "v2 data"))
            .await
            .unwrap();
        svc.save("s1", Artifact::text("doc", "v3 data"))
            .await
            .unwrap();

        let v1 = svc.load_version("s1", "doc", 1).await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&v1.data).unwrap(), "v1 data");
        assert_eq!(v1.metadata.version, 1);

        let v2 = svc.load_version("s1", "doc", 2).await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&v2.data).unwrap(), "v2 data");

        let v3 = svc.load_version("s1", "doc", 3).await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&v3.data).unwrap(), "v3 data");

        // Nonexistent version returns None
        let v99 = svc.load_version("s1", "doc", 99).await.unwrap();
        assert!(v99.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn list_artifacts() {
        let dir = test_dir();
        let svc = FileArtifactService::new(&dir).unwrap();

        svc.save("s1", Artifact::text("alpha", "data"))
            .await
            .unwrap();
        svc.save("s1", Artifact::text("beta", "data"))
            .await
            .unwrap();
        svc.save("s2", Artifact::text("gamma", "data"))
            .await
            .unwrap();

        let list = svc.list("s1").await.unwrap();
        assert_eq!(list.len(), 2);
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));

        // Different session
        let list2 = svc.list("s2").await.unwrap();
        assert_eq!(list2.len(), 1);
        assert_eq!(list2[0].name, "gamma");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_artifact() {
        let dir = test_dir();
        let svc = FileArtifactService::new(&dir).unwrap();

        svc.save("s1", Artifact::text("notes", "data"))
            .await
            .unwrap();
        svc.save("s1", Artifact::text("notes", "v2")).await.unwrap();

        svc.delete("s1", "notes").await.unwrap();

        let result = svc.load("s1", "notes").await.unwrap();
        assert!(result.is_none());

        // Deleting again should be a no-op
        svc.delete("s1", "notes").await.unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn load_nonexistent_returns_none() {
        let dir = test_dir();
        let svc = FileArtifactService::new(&dir).unwrap();

        let result = svc.load("no_session", "no_artifact").await.unwrap();
        assert!(result.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn path_traversal_prevention() {
        let dir = test_dir();
        let svc = FileArtifactService::new(&dir).unwrap();

        // Session ID and name with traversal attempts should be sanitized
        let artifact = Artifact::text("../../../etc/passwd", "malicious");
        let meta = svc.save("../../hack", artifact).await.unwrap();
        assert_eq!(meta.version, 1);

        // The sanitized name should not contain path separators or dots
        let sanitized_session = sanitize_path_component("../../hack");
        let sanitized_name = sanitize_path_component("../../../etc/passwd");
        assert!(!sanitized_session.contains('/'));
        assert!(!sanitized_session.contains('\\'));
        assert!(!sanitized_session.contains('.'));
        assert!(!sanitized_name.contains('/'));
        assert!(!sanitized_name.contains('\\'));
        assert!(!sanitized_name.contains('.'));

        // Should be able to load with the original (unsanitized) names
        let loaded = svc.load("../../hack", "../../../etc/passwd").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(
            std::str::from_utf8(&loaded.unwrap().data).unwrap(),
            "malicious"
        );

        // Verify files stayed within root
        assert!(dir.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sanitize_removes_dangerous_chars() {
        assert_eq!(sanitize_path_component("normal"), "normal");
        assert_eq!(sanitize_path_component(".."), "__");
        assert_eq!(sanitize_path_component("a/b"), "a_b");
        assert_eq!(sanitize_path_component("a\\b"), "a_b");
        assert_eq!(sanitize_path_component("../../etc"), "______etc");
        assert_eq!(sanitize_path_component("file.txt"), "file_txt");
    }
}
