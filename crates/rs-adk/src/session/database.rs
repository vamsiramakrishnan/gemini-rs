//! Database-backed session service (stub).
//!
//! Feature-gated behind `database-sessions`. Currently provides the type and
//! trait implementation as stubs that return errors — full `sqlx` integration
//! will be added in a future task.

#[cfg(feature = "database-sessions")]
use async_trait::async_trait;

#[cfg(feature = "database-sessions")]
use super::{Session, SessionError, SessionId, SessionService};

#[cfg(feature = "database-sessions")]
use crate::events::Event;

/// SQL database-backed session service.
/// Supports PostgreSQL and SQLite via connection URL.
#[cfg(feature = "database-sessions")]
pub struct DatabaseSessionService {
    connection_url: String,
}

#[cfg(feature = "database-sessions")]
impl DatabaseSessionService {
    /// Create a new database session service.
    /// The connection URL should be a valid database URL (e.g., "sqlite::memory:", "postgres://...").
    pub fn new(connection_url: impl Into<String>) -> Self {
        Self {
            connection_url: connection_url.into(),
        }
    }

    /// Returns the connection URL this service was configured with.
    pub fn connection_url(&self) -> &str {
        &self.connection_url
    }

    /// Initialize the database schema (run migrations).
    pub async fn initialize(&self) -> Result<(), SessionError> {
        Err(SessionError::Storage(
            "Database session service not yet fully implemented — awaiting sqlx integration".into(),
        ))
    }
}

#[cfg(feature = "database-sessions")]
#[async_trait]
impl SessionService for DatabaseSessionService {
    async fn create_session(
        &self,
        _app_name: &str,
        _user_id: &str,
    ) -> Result<Session, SessionError> {
        Err(SessionError::Storage(
            "Database session service not yet fully implemented".into(),
        ))
    }

    async fn get_session(&self, _id: &SessionId) -> Result<Option<Session>, SessionError> {
        Err(SessionError::Storage(
            "Database session service not yet fully implemented".into(),
        ))
    }

    async fn list_sessions(
        &self,
        _app_name: &str,
        _user_id: &str,
    ) -> Result<Vec<Session>, SessionError> {
        Err(SessionError::Storage(
            "Database session service not yet fully implemented".into(),
        ))
    }

    async fn delete_session(&self, _id: &SessionId) -> Result<(), SessionError> {
        Err(SessionError::Storage(
            "Database session service not yet fully implemented".into(),
        ))
    }

    async fn append_event(&self, _id: &SessionId, _event: Event) -> Result<(), SessionError> {
        Err(SessionError::Storage(
            "Database session service not yet fully implemented".into(),
        ))
    }

    async fn get_events(&self, _id: &SessionId) -> Result<Vec<Event>, SessionError> {
        Err(SessionError::Storage(
            "Database session service not yet fully implemented".into(),
        ))
    }
}

#[cfg(all(test, feature = "database-sessions"))]
mod tests {
    use super::*;

    #[test]
    fn construction() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        assert_eq!(svc.connection_url(), "sqlite::memory:");
    }

    #[test]
    fn construction_with_postgres_url() {
        let svc = DatabaseSessionService::new("postgres://localhost/mydb");
        assert_eq!(svc.connection_url(), "postgres://localhost/mydb");
    }

    #[tokio::test]
    async fn initialize_returns_stub_error() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        let result = svc.initialize().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_session_returns_stub_error() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        let result = svc.create_session("app", "user").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn trait_impl_is_object_safe() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        let _dyn_ref: &dyn SessionService = &svc;
    }
}
