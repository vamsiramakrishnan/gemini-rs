//! The byte-level store the OKF repository is projected onto.
//!
//! Separating "which files exist and what is in them" from "what a memory
//! means" lets the repository be exercised without touching a disk, and lets
//! production swap the filesystem for object storage without the repository
//! noticing.

use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::core::MemoryError;

/// A flat, path-addressed store of UTF-8 documents.
#[async_trait]
pub trait OkfStore: Send + Sync {
    /// Read a document, or `None` when it does not exist.
    async fn read(&self, path: &str) -> Result<Option<String>, MemoryError>;

    /// Write a document, creating parents as needed.
    async fn write(&self, path: &str, contents: &str) -> Result<(), MemoryError>;

    /// Remove a document. Removing a missing document is not an error.
    async fn remove(&self, path: &str) -> Result<(), MemoryError>;

    /// List every document path under a prefix.
    async fn list(&self, prefix: &str) -> Result<Vec<String>, MemoryError>;
}

/// An in-process store — the default for tests and ephemeral sessions.
#[derive(Debug, Default)]
pub struct MemoryStore {
    files: parking_lot::RwLock<BTreeMap<String, String>>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every path currently held.
    pub fn paths(&self) -> Vec<String> {
        self.files.read().keys().cloned().collect()
    }
}

#[async_trait]
impl OkfStore for MemoryStore {
    async fn read(&self, path: &str) -> Result<Option<String>, MemoryError> {
        Ok(self.files.read().get(path).cloned())
    }

    async fn write(&self, path: &str, contents: &str) -> Result<(), MemoryError> {
        reject_traversal(path)?;
        self.files
            .write()
            .insert(path.to_string(), contents.to_string());
        Ok(())
    }

    async fn remove(&self, path: &str) -> Result<(), MemoryError> {
        self.files.write().remove(path);
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, MemoryError> {
        Ok(self
            .files
            .read()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

/// A filesystem-backed store rooted at a directory.
///
/// Every path is validated against traversal before it touches the filesystem —
/// memory paths are derived from user and record identifiers, and identifiers
/// must never be able to escape their own namespace.
#[derive(Debug, Clone)]
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    /// Root the store at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory this store writes under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, MemoryError> {
        reject_traversal(path)?;
        Ok(self.root.join(path))
    }
}

#[async_trait]
impl OkfStore for FsStore {
    async fn read(&self, path: &str) -> Result<Option<String>, MemoryError> {
        let full = self.resolve(path)?;
        match tokio::fs::read_to_string(&full).await {
            Ok(contents) => Ok(Some(contents)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(MemoryError::Storage(format!("{}: {e}", full.display()))),
        }
    }

    async fn write(&self, path: &str, contents: &str) -> Result<(), MemoryError> {
        let full = self.resolve(path)?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| MemoryError::Storage(format!("{}: {e}", parent.display())))?;
        }
        // Write-then-rename so a crash mid-write cannot leave a half-written
        // canonical record behind.
        let temp = full.with_extension("tmp");
        tokio::fs::write(&temp, contents)
            .await
            .map_err(|e| MemoryError::Storage(format!("{}: {e}", temp.display())))?;
        tokio::fs::rename(&temp, &full)
            .await
            .map_err(|e| MemoryError::Storage(format!("{}: {e}", full.display())))?;
        Ok(())
    }

    async fn remove(&self, path: &str) -> Result<(), MemoryError> {
        let full = self.resolve(path)?;
        match tokio::fs::remove_file(&full).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(MemoryError::Storage(format!("{}: {e}", full.display()))),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, MemoryError> {
        reject_traversal(prefix)?;
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(MemoryError::Storage(format!("{}: {e}", dir.display()))),
            };
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| MemoryError::Storage(e.to_string()))?
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if let Ok(relative) = path.strip_prefix(&self.root) {
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    if relative.starts_with(prefix) {
                        out.push(relative);
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }
}

/// Refuse absolute paths and any `..` component.
fn reject_traversal(path: &str) -> Result<(), MemoryError> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(MemoryError::PolicyRefused(format!(
            "absolute memory path `{path}` refused"
        )));
    }
    for component in candidate.components() {
        if matches!(component, Component::ParentDir | Component::RootDir) {
            return Err(MemoryError::PolicyRefused(format!(
                "memory path `{path}` escapes its namespace"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_store_round_trips_and_lists_by_prefix() {
        let store = MemoryStore::new();
        store.write("users/a/profile.md", "one").await.unwrap();
        store.write("users/b/profile.md", "two").await.unwrap();

        assert_eq!(
            store.read("users/a/profile.md").await.unwrap().as_deref(),
            Some("one")
        );
        assert_eq!(store.list("users/a/").await.unwrap().len(), 1);

        store.remove("users/a/profile.md").await.unwrap();
        assert!(store.read("users/a/profile.md").await.unwrap().is_none());
        // Removing twice is not an error.
        store.remove("users/a/profile.md").await.unwrap();
    }

    #[tokio::test]
    async fn traversal_is_refused_before_it_reaches_the_filesystem() {
        let store = MemoryStore::new();
        let err = store.write("../../etc/passwd", "x").await.unwrap_err();
        assert!(matches!(err, MemoryError::PolicyRefused(_)));
        assert!(store.write("/etc/passwd", "x").await.is_err());
    }

    #[tokio::test]
    async fn fs_store_round_trips_through_a_real_directory() {
        let root = std::env::temp_dir().join(format!("okf-test-{}", uuid::Uuid::new_v4()));
        let store = FsStore::new(&root);

        store
            .write("users/usr_1/preferences.md", "hello")
            .await
            .unwrap();
        assert_eq!(
            store
                .read("users/usr_1/preferences.md")
                .await
                .unwrap()
                .as_deref(),
            Some("hello")
        );
        assert_eq!(
            store.list("users/").await.unwrap(),
            vec!["users/usr_1/preferences.md"]
        );
        assert!(
            store
                .read("users/usr_1/missing.md")
                .await
                .unwrap()
                .is_none()
        );

        store.remove("users/usr_1/preferences.md").await.unwrap();
        assert!(store.list("users/").await.unwrap().is_empty());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }
}
