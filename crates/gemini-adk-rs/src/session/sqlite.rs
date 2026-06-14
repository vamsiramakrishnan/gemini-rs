//! SQLite session service — lightweight persistent session storage.
//!
//! Mirrors ADK-Python's `sqlite_session_service`. Provides session
//! persistence using a local SQLite database file.
//!
//! When the `database-sessions` feature is enabled this delegates to the real
//! `sqlx`-backed [`DatabaseSessionService`](super::DatabaseSessionService).
//! Without that feature it falls back to an in-memory stub so the default
//! build stays dependency-free.

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

/// Backend used by [`SqliteSessionService`].
///
/// With `database-sessions` this is the real `sqlx`-backed service; otherwise
/// it is an in-memory stub.
#[cfg(feature = "database-sessions")]
type Backend = super::DatabaseSessionService;
#[cfg(not(feature = "database-sessions"))]
type Backend = super::InMemorySessionService;

/// Session service backed by SQLite.
///
/// Provides lightweight, file-based session persistence suitable for
/// single-process deployments and development environments.
///
/// The database schema is automatically created on first use.
pub struct SqliteSessionService {
    config: SqliteSessionConfig,
    inner: Backend,
}

impl SqliteSessionService {
    /// Create a new SQLite session service.
    ///
    /// With the `database-sessions` feature this builds a real `sqlx` pool
    /// (opened lazily on first use). Without it, an in-memory stub is used.
    pub fn new(config: SqliteSessionConfig) -> Self {
        let inner = Self::build_backend(&config);
        Self { config, inner }
    }

    #[cfg(feature = "database-sessions")]
    fn build_backend(config: &SqliteSessionConfig) -> Backend {
        let path = config.db_path.to_string_lossy();
        // `:memory:` maps to sqlx's in-memory URL; file paths map to
        // `sqlite://<path>`.
        let url = if path == ":memory:" {
            "sqlite::memory:".to_string()
        } else {
            format!("sqlite://{path}")
        };
        super::DatabaseSessionService::new(url)
    }

    #[cfg(not(feature = "database-sessions"))]
    fn build_backend(config: &SqliteSessionConfig) -> Backend {
        // Without the feature there is no SQLite backend: fall back to in-memory,
        // but LOUDLY — silently dropping a configured DB path would lose every
        // session on restart with no signal.
        static WARN_ONCE: std::sync::Once = std::sync::Once::new();
        WARN_ONCE.call_once(|| {
            let msg = format!(
                "SqliteSessionService: the `database-sessions` feature is not                  enabled — ignoring db_path '{}' and using in-memory storage                  (sessions are lost on restart)",
                config.db_path.to_string_lossy()
            );
            #[cfg(feature = "tracing-support")]
            tracing::warn!(target: "gemini_adk_rs::session", "{msg}");
            #[cfg(not(feature = "tracing-support"))]
            eprintln!("warning: {msg}");
        });
        super::InMemorySessionService::new()
    }

    /// Returns the configured database path.
    pub fn db_path(&self) -> &std::path::Path {
        &self.config.db_path
    }
}

#[async_trait]
impl SessionService for SqliteSessionService {
    async fn create_session(&self, app_name: &str, user_id: &str) -> Result<Session, SessionError> {
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
