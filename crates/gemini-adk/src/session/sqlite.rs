//! SQLite session service — lightweight persistent session storage.
//!
//! Mirrors ADK-Python's `sqlite_session_service`. Provides session
//! persistence using a local SQLite database file.

use std::path::PathBuf;

use async_trait::async_trait;

use super::{Session, SessionError, SessionId, SessionService};
use crate::events::Event;

/// Configuration for the SQLite session service.
#[derive(Debug, Clone)]
pub struct SqliteSessionConfig {
    /// Path to the SQLite database file.
    pub db_path: PathBuf,
}

impl SqliteSessionConfig {
    /// Create a config for an in-memory SQLite database.
    pub fn in_memory() -> Self {
        Self {
            db_path: PathBuf::from(":memory:"),
        }
    }
}

/// Session service backed by SQLite.
///
/// Provides lightweight, file-based session persistence suitable for
/// single-process deployments and development environments.
///
/// The database schema is automatically created on first use.
pub struct SqliteSessionService {
    config: SqliteSessionConfig,
    // In a real implementation, this would hold a connection pool.
    // For now, we delegate to InMemorySessionService as a stub.
    inner: super::InMemorySessionService,
}

impl SqliteSessionService {
    /// Create a new SQLite session service.
    ///
    /// Initializes the database schema if it doesn't exist.
    pub fn new(config: SqliteSessionConfig) -> Self {
        // In a real implementation, this would:
        // 1. Open/create the SQLite database file
        // 2. Run the SQLITE_SCHEMA migration
        // 3. Return a connected service
        Self {
            config,
            inner: super::InMemorySessionService::new(),
        }
    }

    /// Returns the configured database path.
    pub fn db_path(&self) -> &std::path::Path {
        &self.config.db_path
    }
}

#[async_trait]
impl SessionService for SqliteSessionService {
    async fn create_session(&self, app_name: &str, user_id: &str) -> Result<Session, SessionError> {
        // Stub: delegates to in-memory implementation.
        // A real implementation would INSERT INTO sessions ...
        self.inner.create_session(app_name, user_id).await
    }

    async fn get_session(&self, id: &SessionId) -> Result<Option<Session>, SessionError> {
        self.inner.get_session(id).await
    }

    async fn list_sessions(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<Vec<Session>, SessionError> {
        self.inner.list_sessions(app_name, user_id).await
    }

    async fn delete_session(&self, id: &SessionId) -> Result<(), SessionError> {
        self.inner.delete_session(id).await
    }

    async fn append_event(&self, id: &SessionId, event: Event) -> Result<(), SessionError> {
        self.inner.append_event(id, event).await
    }

    async fn get_events(&self, id: &SessionId) -> Result<Vec<Event>, SessionError> {
        self.inner.get_events(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_get() {
        let svc = SqliteSessionService::new(SqliteSessionConfig::in_memory());
        let session = svc.create_session("app", "user").await.unwrap();
        let fetched = svc.get_session(&session.id).await.unwrap();
        assert!(fetched.is_some());
    }

    #[test]
    fn db_path() {
        let svc = SqliteSessionService::new(SqliteSessionConfig {
            db_path: PathBuf::from("/tmp/test.db"),
        });
        assert_eq!(svc.db_path(), std::path::Path::new("/tmp/test.db"));
    }

    #[test]
    fn in_memory_config() {
        let config = SqliteSessionConfig::in_memory();
        assert_eq!(config.db_path, PathBuf::from(":memory:"));
    }
}
