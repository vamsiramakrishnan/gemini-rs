//! PostgreSQL session service — scalable persistent session storage.
//!
//! Provides session persistence using a PostgreSQL database. Suitable for
//! multi-process and distributed deployments.
//!
//! Feature-gated behind `postgres-sessions`. Delegates to the real
//! `sqlx`-backed [`DatabaseSessionService`](super::DatabaseSessionService).

use async_trait::async_trait;

use super::{DatabaseSessionService, Session, SessionError, SessionId, SessionService};
use crate::events::Event;

/// Configuration for the PostgreSQL session service.
#[derive(Debug, Clone)]
pub struct PostgresSessionConfig {
    /// PostgreSQL connection string (e.g., `postgres://user:pass@host/db`).
    pub connection_string: String,
    /// Maximum number of connections in the pool.
    pub max_connections: u32,
}

impl PostgresSessionConfig {
    /// Create a new config with the given connection string and default pool size.
    pub fn new(connection_string: impl Into<String>) -> Self {
        Self {
            connection_string: connection_string.into(),
            max_connections: 10,
        }
    }

    /// Set the maximum number of connections in the pool.
    pub fn max_connections(mut self, max: u32) -> Self {
        self.max_connections = max;
        self
    }
}

/// Session service backed by PostgreSQL.
///
/// Provides scalable, multi-process session persistence using PostgreSQL.
/// Suitable for production deployments requiring horizontal scaling.
///
/// Delegates all storage operations to a [`DatabaseSessionService`] built from
/// the configured connection string. The connection pool is opened lazily on
/// first use; call [`initialize`](Self::initialize) to open it eagerly and run
/// the schema migration.
pub struct PostgresSessionService {
    config: PostgresSessionConfig,
    inner: DatabaseSessionService,
}

impl PostgresSessionService {
    /// Create a new PostgreSQL session service.
    ///
    /// This only creates the service struct. The pool connects lazily on first
    /// use, or eagerly via [`initialize`](Self::initialize).
    pub fn new(config: PostgresSessionConfig) -> Self {
        let inner = DatabaseSessionService::new(config.connection_string.clone());
        Self { config, inner }
    }

    /// Open the pool and run the schema migration.
    ///
    /// Creates the `sessions` and `events` tables if they don't exist.
    /// Safe to call multiple times.
    pub async fn initialize(&self) -> Result<(), SessionError> {
        self.inner.initialize().await
    }

    /// Returns the configured connection string.
    pub fn connection_string(&self) -> &str {
        &self.config.connection_string
    }

    /// Returns the configured maximum number of pool connections.
    pub fn max_connections(&self) -> u32 {
        self.config.max_connections
    }
}

#[async_trait]
impl SessionService for PostgresSessionService {
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

    #[test]
    fn config_new() {
        let config = PostgresSessionConfig::new("postgres://localhost/test");
        assert_eq!(config.connection_string, "postgres://localhost/test");
        assert_eq!(config.max_connections, 10);
    }

    #[test]
    fn config_max_connections() {
        let config = PostgresSessionConfig::new("postgres://localhost/test").max_connections(20);
        assert_eq!(config.max_connections, 20);
    }

    #[test]
    fn service_accessors() {
        let svc = PostgresSessionService::new(
            PostgresSessionConfig::new("postgres://user:pass@host/db").max_connections(5),
        );
        assert_eq!(svc.connection_string(), "postgres://user:pass@host/db");
        assert_eq!(svc.max_connections(), 5);
    }
}
