//! Session persistence — multi-session, multi-turn CRUD.
//!
//! Mirrors ADK-JS's `BaseSessionService`. Provides a trait for session
//! persistence with an in-memory default implementation.

mod memory;
mod sqlite;
mod types;

#[cfg(feature = "database-sessions")]
mod database;
#[cfg(feature = "database-sessions")]
pub use database::DatabaseSessionService;

pub mod db_schema;

pub use memory::InMemorySessionService;
pub use sqlite::{SqliteSessionConfig, SqliteSessionService};
pub use types::{Session, SessionId};

use async_trait::async_trait;

use crate::events::Event;

/// Errors from session service operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The session with the given ID was not found.
    #[error("Session not found: {0}")]
    NotFound(SessionId),
    /// A storage backend error.
    #[error("Storage error: {0}")]
    Storage(String),
}

/// Trait for session persistence — CRUD operations + event append.
///
/// Implementations must be `Send + Sync` for use across async tasks.
#[async_trait]
pub trait SessionService: Send + Sync {
    /// Create a new session.
    async fn create_session(&self, app_name: &str, user_id: &str) -> Result<Session, SessionError>;

    /// Get a session by ID.
    async fn get_session(&self, id: &SessionId) -> Result<Option<Session>, SessionError>;

    /// List sessions for an app + user.
    async fn list_sessions(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<Vec<Session>, SessionError>;

    /// Delete a session.
    async fn delete_session(&self, id: &SessionId) -> Result<(), SessionError>;

    /// Append an event to a session's history.
    async fn append_event(&self, id: &SessionId, event: Event) -> Result<(), SessionError>;

    /// Get all events for a session.
    async fn get_events(&self, id: &SessionId) -> Result<Vec<Event>, SessionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_get_session() {
        let svc = InMemorySessionService::new();
        let session = svc.create_session("my-app", "user-1").await.unwrap();
        assert_eq!(session.app_name, "my-app");
        assert_eq!(session.user_id, "user-1");

        let fetched = svc.get_session(&session.id).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().id, session.id);
    }

    #[tokio::test]
    async fn list_sessions_filters_by_app_and_user() {
        let svc = InMemorySessionService::new();
        svc.create_session("app-a", "user-1").await.unwrap();
        svc.create_session("app-a", "user-1").await.unwrap();
        svc.create_session("app-a", "user-2").await.unwrap();
        svc.create_session("app-b", "user-1").await.unwrap();

        let list = svc.list_sessions("app-a", "user-1").await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn delete_session_removes_it() {
        let svc = InMemorySessionService::new();
        let session = svc.create_session("app", "user").await.unwrap();
        svc.delete_session(&session.id).await.unwrap();
        let fetched = svc.get_session(&session.id).await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn append_and_get_events() {
        let svc = InMemorySessionService::new();
        let session = svc.create_session("app", "user").await.unwrap();

        let event = Event::new("user", Some("Hello!".to_string()));
        svc.append_event(&session.id, event).await.unwrap();

        let events = svc.get_events(&session.id).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].author, "user");
    }

    #[tokio::test]
    async fn append_event_to_nonexistent_session() {
        let svc = InMemorySessionService::new();
        let id = SessionId::new();
        let event = Event::new("user", Some("Hello".to_string()));
        let result = svc.append_event(&id, event).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn session_service_is_object_safe() {
        fn _assert(_: &dyn SessionService) {}
    }
}

#[cfg(test)]
mod schema_tests {
    use super::db_schema;

    #[test]
    fn postgres_schema_has_tables() {
        assert!(db_schema::POSTGRES_SCHEMA.contains("CREATE TABLE IF NOT EXISTS sessions"));
        assert!(db_schema::POSTGRES_SCHEMA.contains("CREATE TABLE IF NOT EXISTS events"));
    }

    #[test]
    fn sqlite_schema_has_tables() {
        assert!(db_schema::SQLITE_SCHEMA.contains("CREATE TABLE IF NOT EXISTS sessions"));
        assert!(db_schema::SQLITE_SCHEMA.contains("CREATE TABLE IF NOT EXISTS events"));
    }

    #[test]
    fn postgres_schema_has_indexes() {
        assert!(db_schema::POSTGRES_SCHEMA.contains("idx_events_session"));
        assert!(db_schema::POSTGRES_SCHEMA.contains("idx_sessions_app_user"));
    }

    #[test]
    fn sqlite_schema_has_indexes() {
        assert!(db_schema::SQLITE_SCHEMA.contains("idx_events_session"));
        assert!(db_schema::SQLITE_SCHEMA.contains("idx_sessions_app_user"));
    }

    #[test]
    fn postgres_schema_uses_jsonb() {
        assert!(db_schema::POSTGRES_SCHEMA.contains("JSONB"));
    }

    #[test]
    fn sqlite_schema_uses_text_for_json() {
        // SQLite doesn't have JSONB, so JSON columns use TEXT
        assert!(!db_schema::SQLITE_SCHEMA.contains("JSONB"));
    }
}
