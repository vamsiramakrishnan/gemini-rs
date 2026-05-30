//! Database-backed session service.
//!
//! Feature-gated behind `database-sessions`. Provides a real `sqlx`-backed
//! [`SessionService`] implementation that supports both SQLite and (when the
//! `postgres-sessions` feature is enabled) PostgreSQL, chosen by the
//! connection-URL scheme.
//!
//! Storage layout (portable across drivers):
//! - `sessions(id TEXT PRIMARY KEY, app_name TEXT, user_id TEXT, data TEXT)`
//!   where `data` is the full JSON of the [`Session`] *without* its events.
//! - `events(session_id TEXT, seq INTEGER, data TEXT)` where `data` is the
//!   full JSON of one [`Event`], ordered by `seq`.
//!
//! Whole structs are serialized with `serde_json`, avoiding column drift and
//! keeping the schema driver-portable.

#[cfg(feature = "database-sessions")]
use async_trait::async_trait;
#[cfg(feature = "database-sessions")]
use tokio::sync::Mutex;

#[cfg(feature = "database-sessions")]
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
#[cfg(feature = "database-sessions")]
use sqlx::SqlitePool;

#[cfg(feature = "postgres-sessions")]
use sqlx::postgres::PgPoolOptions;
#[cfg(feature = "postgres-sessions")]
use sqlx::PgPool;

#[cfg(feature = "database-sessions")]
use super::{Session, SessionError, SessionId, SessionService};

#[cfg(feature = "database-sessions")]
use crate::events::Event;

/// Internal connection pool, chosen by URL scheme.
#[cfg(feature = "database-sessions")]
enum Pool {
    /// SQLite pool (file or in-memory).
    Sqlite(SqlitePool),
    /// PostgreSQL pool.
    #[cfg(feature = "postgres-sessions")]
    Postgres(PgPool),
}

/// SQL database-backed session service.
///
/// Supports SQLite and (with `postgres-sessions`) PostgreSQL via connection
/// URL. The pool is opened lazily on first use and cached, so [`new`] stays
/// synchronous and cheap.
///
/// [`new`]: DatabaseSessionService::new
#[cfg(feature = "database-sessions")]
pub struct DatabaseSessionService {
    connection_url: String,
    /// Max pool connections (PostgreSQL). SQLite always uses 1 so an
    /// in-memory database persists across calls.
    max_connections: Option<u32>,
    pool: Mutex<Option<std::sync::Arc<Pool>>>,
}

#[cfg(feature = "database-sessions")]
impl DatabaseSessionService {
    /// Create a new database session service.
    ///
    /// The connection URL determines the backend by scheme:
    /// - `sqlite:` / `sqlite::memory:` → SQLite
    /// - `postgres:` / `postgresql:` → PostgreSQL (requires `postgres-sessions`)
    ///
    /// This does not open a connection; the pool is created lazily on first
    /// use (or eagerly via [`initialize`](Self::initialize)).
    pub fn new(connection_url: impl Into<String>) -> Self {
        Self {
            connection_url: connection_url.into(),
            max_connections: None,
            pool: Mutex::new(None),
        }
    }

    /// Set the maximum number of pool connections (PostgreSQL only).
    ///
    /// SQLite ignores this and always uses a single connection so that an
    /// in-memory database survives across calls.
    pub fn with_max_connections(mut self, max: u32) -> Self {
        self.max_connections = Some(max);
        self
    }

    /// Returns the connection URL this service was configured with.
    pub fn connection_url(&self) -> &str {
        &self.connection_url
    }

    /// Open the connection pool for the configured URL.
    async fn open_pool(&self) -> Result<Pool, SessionError> {
        let url = self.connection_url.as_str();
        if is_postgres_url(url) {
            #[cfg(feature = "postgres-sessions")]
            {
                let mut opts = PgPoolOptions::new();
                if let Some(max) = self.max_connections {
                    opts = opts.max_connections(max);
                }
                let pool = opts
                    .connect(url)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?;
                return Ok(Pool::Postgres(pool));
            }
            #[cfg(not(feature = "postgres-sessions"))]
            {
                return Err(SessionError::Storage(format!(
                    "PostgreSQL URL '{url}' requires the 'postgres-sessions' feature"
                )));
            }
        }

        // SQLite (default). Support `sqlite::memory:`, `sqlite:path`,
        // `sqlite://path`, and bare paths.
        let opts = sqlite_connect_options(url)?;
        // max_connections(1) is CRITICAL for `:memory:` so that the single
        // in-memory database persists across calls instead of every new
        // connection getting a fresh empty DB.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;
        Ok(Pool::Sqlite(pool))
    }

    /// Lazily ensure the pool is connected and the schema exists, returning a
    /// shared handle to it.
    async fn pool(&self) -> Result<std::sync::Arc<Pool>, SessionError> {
        let mut guard = self.pool.lock().await;
        if let Some(p) = guard.as_ref() {
            return Ok(p.clone());
        }
        let pool = self.open_pool().await?;
        create_schema(&pool).await?;
        let arc = std::sync::Arc::new(pool);
        *guard = Some(arc.clone());
        Ok(arc)
    }

    /// Initialize the database: open the pool and create the schema.
    ///
    /// Safe to call multiple times. Subsequent CRUD calls reuse the cached
    /// pool.
    pub async fn initialize(&self) -> Result<(), SessionError> {
        self.pool().await?;
        Ok(())
    }
}

/// Returns true if a sqlx error is a unique-constraint violation.
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.is_unique_violation())
}

/// Returns true if `url` denotes a PostgreSQL connection.
#[cfg(feature = "database-sessions")]
fn is_postgres_url(url: &str) -> bool {
    url.starts_with("postgres:") || url.starts_with("postgresql:")
}

/// Build SQLite connect options from a connection URL, creating the file if
/// missing.
#[cfg(feature = "database-sessions")]
fn sqlite_connect_options(url: &str) -> Result<SqliteConnectOptions, SessionError> {
    use std::str::FromStr;
    // Normalize bare paths to a `sqlite:` URL form sqlx understands.
    let normalized = if url.starts_with("sqlite:") {
        url.to_string()
    } else {
        format!("sqlite://{url}")
    };
    SqliteConnectOptions::from_str(&normalized)
        .map(|o| o.create_if_missing(true))
        .map_err(|e| SessionError::Storage(e.to_string()))
}

/// Create the portable schema if it does not already exist.
#[cfg(feature = "database-sessions")]
async fn create_schema(pool: &Pool) -> Result<(), SessionError> {
    match pool {
        Pool::Sqlite(p) => {
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS sessions (\
                    id TEXT PRIMARY KEY, \
                    app_name TEXT NOT NULL, \
                    user_id TEXT NOT NULL, \
                    data TEXT NOT NULL)",
            )
            .execute(p)
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS events (\
                    session_id TEXT NOT NULL, \
                    seq INTEGER NOT NULL, \
                    data TEXT NOT NULL, \
                    PRIMARY KEY (session_id, seq))",
            )
            .execute(p)
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_sessions_app_user \
                    ON sessions (app_name, user_id)",
            )
            .execute(p)
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;
            sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_session ON events (session_id)")
                .execute(p)
                .await
                .map_err(|e| SessionError::Storage(e.to_string()))?;
            Ok(())
        }
        #[cfg(feature = "postgres-sessions")]
        Pool::Postgres(p) => {
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS sessions (\
                    id TEXT PRIMARY KEY, \
                    app_name TEXT NOT NULL, \
                    user_id TEXT NOT NULL, \
                    data TEXT NOT NULL)",
            )
            .execute(p)
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS events (\
                    session_id TEXT NOT NULL, \
                    seq BIGINT NOT NULL, \
                    data TEXT NOT NULL, \
                    PRIMARY KEY (session_id, seq))",
            )
            .execute(p)
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_sessions_app_user \
                    ON sessions (app_name, user_id)",
            )
            .execute(p)
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;
            sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_session ON events (session_id)")
                .execute(p)
                .await
                .map_err(|e| SessionError::Storage(e.to_string()))?;
            Ok(())
        }
    }
}

/// Serialize a session WITHOUT its events to a JSON string.
#[cfg(feature = "database-sessions")]
fn session_to_data(session: &Session) -> Result<String, SessionError> {
    let mut bare = session.clone();
    bare.events = Vec::new();
    serde_json::to_string(&bare).map_err(|e| SessionError::Storage(e.to_string()))
}

#[cfg(feature = "database-sessions")]
#[async_trait]
impl SessionService for DatabaseSessionService {
    async fn create_session(&self, app_name: &str, user_id: &str) -> Result<Session, SessionError> {
        let pool = self.pool().await?;
        let session = Session::new(app_name, user_id);
        let id = session.id.as_str().to_string();
        let data = session_to_data(&session)?;

        match pool.as_ref() {
            Pool::Sqlite(p) => {
                sqlx::query(
                    "INSERT INTO sessions (id, app_name, user_id, data) VALUES (?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(app_name)
                .bind(user_id)
                .bind(&data)
                .execute(p)
                .await
                .map_err(|e| SessionError::Storage(e.to_string()))?;
            }
            #[cfg(feature = "postgres-sessions")]
            Pool::Postgres(p) => {
                sqlx::query(
                    "INSERT INTO sessions (id, app_name, user_id, data) VALUES ($1, $2, $3, $4)",
                )
                .bind(&id)
                .bind(app_name)
                .bind(user_id)
                .bind(&data)
                .execute(p)
                .await
                .map_err(|e| SessionError::Storage(e.to_string()))?;
            }
        }
        Ok(session)
    }

    async fn get_session(&self, id: &SessionId) -> Result<Option<Session>, SessionError> {
        let pool = self.pool().await?;
        let id_str = id.as_str();

        let data: Option<String> = match pool.as_ref() {
            Pool::Sqlite(p) => sqlx::query_scalar("SELECT data FROM sessions WHERE id = ?")
                .bind(id_str)
                .fetch_optional(p)
                .await
                .map_err(|e| SessionError::Storage(e.to_string()))?,
            #[cfg(feature = "postgres-sessions")]
            Pool::Postgres(p) => sqlx::query_scalar("SELECT data FROM sessions WHERE id = $1")
                .bind(id_str)
                .fetch_optional(p)
                .await
                .map_err(|e| SessionError::Storage(e.to_string()))?,
        };

        match data {
            None => Ok(None),
            Some(json) => {
                let mut session: Session = serde_json::from_str(&json)
                    .map_err(|e| SessionError::Storage(e.to_string()))?;
                // Match in-memory behavior: load events alongside the session.
                session.events = self.get_events(id).await?;
                Ok(Some(session))
            }
        }
    }

    async fn list_sessions(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<Vec<Session>, SessionError> {
        let pool = self.pool().await?;

        let rows: Vec<String> = match pool.as_ref() {
            Pool::Sqlite(p) => {
                sqlx::query_scalar("SELECT data FROM sessions WHERE app_name = ? AND user_id = ?")
                    .bind(app_name)
                    .bind(user_id)
                    .fetch_all(p)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?
            }
            #[cfg(feature = "postgres-sessions")]
            Pool::Postgres(p) => {
                sqlx::query_scalar("SELECT data FROM sessions WHERE app_name = $1 AND user_id = $2")
                    .bind(app_name)
                    .bind(user_id)
                    .fetch_all(p)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?
            }
        };

        let mut sessions = Vec::with_capacity(rows.len());
        for json in rows {
            let session: Session =
                serde_json::from_str(&json).map_err(|e| SessionError::Storage(e.to_string()))?;
            sessions.push(session);
        }
        Ok(sessions)
    }

    async fn delete_session(&self, id: &SessionId) -> Result<(), SessionError> {
        let pool = self.pool().await?;
        let id_str = id.as_str();

        match pool.as_ref() {
            Pool::Sqlite(p) => {
                sqlx::query("DELETE FROM events WHERE session_id = ?")
                    .bind(id_str)
                    .execute(p)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?;
                sqlx::query("DELETE FROM sessions WHERE id = ?")
                    .bind(id_str)
                    .execute(p)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?;
            }
            #[cfg(feature = "postgres-sessions")]
            Pool::Postgres(p) => {
                sqlx::query("DELETE FROM events WHERE session_id = $1")
                    .bind(id_str)
                    .execute(p)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?;
                sqlx::query("DELETE FROM sessions WHERE id = $1")
                    .bind(id_str)
                    .execute(p)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?;
            }
        }
        Ok(())
    }

    async fn append_event(&self, id: &SessionId, event: Event) -> Result<(), SessionError> {
        let pool = self.pool().await?;
        let id_str = id.as_str();

        // Ensure the session exists (matches in-memory NotFound semantics).
        let exists: Option<String> = match pool.as_ref() {
            Pool::Sqlite(p) => sqlx::query_scalar("SELECT id FROM sessions WHERE id = ?")
                .bind(id_str)
                .fetch_optional(p)
                .await
                .map_err(|e| SessionError::Storage(e.to_string()))?,
            #[cfg(feature = "postgres-sessions")]
            Pool::Postgres(p) => sqlx::query_scalar("SELECT id FROM sessions WHERE id = $1")
                .bind(id_str)
                .fetch_optional(p)
                .await
                .map_err(|e| SessionError::Storage(e.to_string()))?,
        };
        if exists.is_none() {
            return Err(SessionError::NotFound(id.clone()));
        }

        let data =
            serde_json::to_string(&event).map_err(|e| SessionError::Storage(e.to_string()))?;

        // Allocate the sequence number and insert in a single atomic statement
        // (`INSERT ... SELECT COALESCE(MAX(seq), -1) + 1`) so concurrent
        // appenders can't read the same `MAX(seq)` and collide. On Postgres,
        // two transactions under READ COMMITTED can still both observe the
        // pre-insert snapshot and one will hit the `(session_id, seq)` unique
        // constraint — retry a bounded number of times in that case. SQLite's
        // single-connection pool serializes writes, so it never retries.
        const MAX_ATTEMPTS: u32 = 8;
        for attempt in 0..MAX_ATTEMPTS {
            let result = match pool.as_ref() {
                Pool::Sqlite(p) => sqlx::query(
                    "INSERT INTO events (session_id, seq, data) \
                     SELECT ?, COALESCE(MAX(seq), -1) + 1, ? FROM events WHERE session_id = ?",
                )
                .bind(id_str)
                .bind(&data)
                .bind(id_str)
                .execute(p)
                .await
                .map(|_| ()),
                #[cfg(feature = "postgres-sessions")]
                Pool::Postgres(p) => sqlx::query(
                    "INSERT INTO events (session_id, seq, data) \
                     SELECT $1, COALESCE(MAX(seq), -1) + 1, $2 FROM events WHERE session_id = $1",
                )
                .bind(id_str)
                .bind(&data)
                .execute(p)
                .await
                .map(|_| ()),
            };
            match result {
                Ok(_) => return Ok(()),
                Err(e) if is_unique_violation(&e) && attempt + 1 < MAX_ATTEMPTS => continue,
                Err(e) => return Err(SessionError::Storage(e.to_string())),
            }
        }
        Err(SessionError::Storage(
            "append_event: exhausted retries allocating event sequence number".into(),
        ))
    }

    async fn get_events(&self, id: &SessionId) -> Result<Vec<Event>, SessionError> {
        let pool = self.pool().await?;
        let id_str = id.as_str();

        let rows: Vec<String> = match pool.as_ref() {
            Pool::Sqlite(p) => {
                sqlx::query_scalar("SELECT data FROM events WHERE session_id = ? ORDER BY seq ASC")
                    .bind(id_str)
                    .fetch_all(p)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?
            }
            #[cfg(feature = "postgres-sessions")]
            Pool::Postgres(p) => {
                sqlx::query_scalar("SELECT data FROM events WHERE session_id = $1 ORDER BY seq ASC")
                    .bind(id_str)
                    .fetch_all(p)
                    .await
                    .map_err(|e| SessionError::Storage(e.to_string()))?
            }
        };

        let mut events = Vec::with_capacity(rows.len());
        for json in rows {
            let event: Event =
                serde_json::from_str(&json).map_err(|e| SessionError::Storage(e.to_string()))?;
            events.push(event);
        }
        Ok(events)
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
    async fn initialize_succeeds_for_sqlite_memory() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        svc.initialize().await.unwrap();
    }

    #[tokio::test]
    async fn create_session_persists() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        svc.initialize().await.unwrap();
        let session = svc.create_session("app", "user").await.unwrap();
        assert_eq!(session.app_name, "app");
        assert_eq!(session.user_id, "user");
    }

    /// Regression for the event-sequence race: concurrent `append_event`
    /// calls must each get a distinct seq and none may be dropped on the
    /// `(session_id, seq)` primary key.
    #[tokio::test]
    async fn concurrent_appends_allocate_distinct_sequences() {
        let svc = std::sync::Arc::new(DatabaseSessionService::new("sqlite::memory:"));
        svc.initialize().await.unwrap();
        let session = svc.create_session("app", "user").await.unwrap();

        const N: usize = 25;
        let mut handles = Vec::new();
        for i in 0..N {
            let svc = svc.clone();
            let id = session.id.clone();
            handles.push(tokio::spawn(async move {
                svc.append_event(&id, Event::new("user", Some(format!("msg-{i}"))))
                    .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        // All N events landed (no drops), and ordering is stable.
        let events = svc.get_events(&session.id).await.unwrap();
        assert_eq!(events.len(), N, "every concurrent append must persist");
    }

    #[tokio::test]
    async fn trait_impl_is_object_safe() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        let _dyn_ref: &dyn SessionService = &svc;
    }

    /// Full round-trip against an in-memory SQLite database, mirroring the
    /// in-memory service's test expectations as the oracle.
    #[tokio::test]
    async fn full_round_trip() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        svc.initialize().await.unwrap();

        // create
        let session = svc.create_session("my-app", "user-1").await.unwrap();
        assert_eq!(session.app_name, "my-app");
        assert_eq!(session.user_id, "user-1");

        // get -> Some
        let fetched = svc.get_session(&session.id).await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, session.id);
        assert!(fetched.events.is_empty());

        // append_event x2
        svc.append_event(&session.id, Event::new("user", Some("Hello!".to_string())))
            .await
            .unwrap();
        svc.append_event(
            &session.id,
            Event::new("assistant", Some("Hi there".to_string())),
        )
        .await
        .unwrap();

        // get_events -> ordered
        let events = svc.get_events(&session.id).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].author, "user");
        assert_eq!(events[0].content.as_deref(), Some("Hello!"));
        assert_eq!(events[1].author, "assistant");

        // get_session now loads events alongside
        let with_events = svc.get_session(&session.id).await.unwrap().unwrap();
        assert_eq!(with_events.events.len(), 2);

        // list_sessions filters by app + user
        svc.create_session("my-app", "user-1").await.unwrap();
        svc.create_session("my-app", "user-2").await.unwrap();
        svc.create_session("other-app", "user-1").await.unwrap();
        let list = svc.list_sessions("my-app", "user-1").await.unwrap();
        assert_eq!(list.len(), 2);

        // delete -> get None
        svc.delete_session(&session.id).await.unwrap();
        let gone = svc.get_session(&session.id).await.unwrap();
        assert!(gone.is_none());
        // events cleared too
        let no_events = svc.get_events(&session.id).await.unwrap();
        assert!(no_events.is_empty());
    }

    #[tokio::test]
    async fn append_to_missing_session_is_not_found() {
        let svc = DatabaseSessionService::new("sqlite::memory:");
        svc.initialize().await.unwrap();
        let id = SessionId::new();
        let result = svc
            .append_event(&id, Event::new("user", Some("Hi".to_string())))
            .await;
        assert!(matches!(result, Err(SessionError::NotFound(_))));
    }

    #[tokio::test]
    async fn lazy_connect_without_explicit_initialize() {
        // CRUD should work without an explicit initialize() call.
        let svc = DatabaseSessionService::new("sqlite::memory:");
        let session = svc.create_session("app", "user").await.unwrap();
        let fetched = svc.get_session(&session.id).await.unwrap();
        assert!(fetched.is_some());
    }
}
