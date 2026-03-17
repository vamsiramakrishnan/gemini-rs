//! PostgreSQL session service — scalable persistent session storage.
//!
//! Provides session persistence using a PostgreSQL database with JSONB
//! storage for structured event data. Suitable for multi-process and
//! distributed deployments.
//!
//! Feature-gated behind `postgres-sessions`.

use async_trait::async_trait;

use super::{db_schema, Session, SessionError, SessionId, SessionService};
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
/// Provides scalable, multi-process session persistence using PostgreSQL
/// with JSONB columns for structured event data. Suitable for production
/// deployments requiring horizontal scaling.
///
/// The database schema is defined in [`db_schema::POSTGRES_SCHEMA`] and
/// must be applied via [`initialize`](Self::initialize) before first use.
pub struct PostgresSessionService {
    config: PostgresSessionConfig,
    // In a real implementation, this would hold a connection pool
    // (e.g., `sqlx::PgPool` or `deadpool_postgres::Pool`).
}

impl PostgresSessionService {
    /// Create a new PostgreSQL session service.
    ///
    /// This only creates the service struct. Call [`initialize`](Self::initialize)
    /// to run the schema migration before using the service.
    pub fn new(config: PostgresSessionConfig) -> Self {
        Self { config }
    }

    /// Run the PostgreSQL schema migration.
    ///
    /// Creates the `sessions` and `events` tables if they don't exist.
    /// Safe to call multiple times (uses `CREATE TABLE IF NOT EXISTS`).
    pub async fn initialize(&self) -> Result<(), SessionError> {
        let _schema = db_schema::POSTGRES_SCHEMA;
        let _conn_str = &self.config.connection_string;
        let _max_conns = self.config.max_connections;
        // Real implementation would:
        // 1. Create a connection pool with max_connections
        // 2. Execute POSTGRES_SCHEMA as a migration
        todo!("Connect to PostgreSQL at {_conn_str} and run POSTGRES_SCHEMA migration")
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
        let session = Session::new(app_name, user_id);
        let _id = session.id.as_str();
        let _state = serde_json::to_value(&session.state)
            .map_err(|e| SessionError::Storage(e.to_string()))?;

        // Real implementation:
        // INSERT INTO sessions (id, app_name, user_id, state)
        // VALUES ($1, $2, $3, $4)
        // RETURNING id, app_name, user_id, state, create_time, update_time
        todo!("INSERT session {_id} into PostgreSQL")
    }

    async fn get_session(&self, id: &SessionId) -> Result<Option<Session>, SessionError> {
        let _id = id.as_str();

        // Real implementation:
        // SELECT id, app_name, user_id, state, create_time, update_time
        // FROM sessions WHERE id = $1
        todo!("SELECT session {_id} from PostgreSQL")
    }

    async fn list_sessions(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<Vec<Session>, SessionError> {
        let _app = app_name;
        let _user = user_id;

        // Real implementation:
        // SELECT id, app_name, user_id, state, create_time, update_time
        // FROM sessions WHERE app_name = $1 AND user_id = $2
        // ORDER BY create_time DESC
        todo!("SELECT sessions for app={_app} user={_user} from PostgreSQL")
    }

    async fn delete_session(&self, id: &SessionId) -> Result<(), SessionError> {
        let _id = id.as_str();

        // Real implementation:
        // DELETE FROM sessions WHERE id = $1
        // (events cascade-deleted via ON DELETE CASCADE)
        todo!("DELETE session {_id} from PostgreSQL")
    }

    async fn append_event(&self, id: &SessionId, event: Event) -> Result<(), SessionError> {
        let _session_id = id.as_str();
        let _event_id = &event.id;
        let _invocation_id = &event.invocation_id;
        let _author = &event.author;
        let _content = &event.content;
        let _actions = serde_json::to_value(&event.actions)
            .map_err(|e| SessionError::Storage(e.to_string()))?;
        let _timestamp = event.timestamp;

        // Real implementation:
        // INSERT INTO events (id, session_id, invocation_id, author, content, actions, timestamp)
        // VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7)
        //
        // Also update session's update_time:
        // UPDATE sessions SET update_time = NOW() WHERE id = $1
        todo!("INSERT event {_event_id} for session {_session_id} into PostgreSQL")
    }

    async fn get_events(&self, id: &SessionId) -> Result<Vec<Event>, SessionError> {
        let _id = id.as_str();

        // Real implementation:
        // SELECT id, session_id, invocation_id, author, content, actions, timestamp
        // FROM events WHERE session_id = $1
        // ORDER BY timestamp ASC, created_at ASC
        todo!("SELECT events for session {_id} from PostgreSQL")
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
