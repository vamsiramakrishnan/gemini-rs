//! SQL schema definitions for database-backed session persistence.
//!
//! These are always available (not feature-gated) so that tooling, migrations,
//! and documentation can reference them regardless of runtime backend.

/// SQL migration for PostgreSQL.
pub const POSTGRES_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    app_name TEXT NOT NULL,
    user_id TEXT NOT NULL,
    state JSONB NOT NULL DEFAULT '{}',
    create_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    update_time TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    invocation_id TEXT NOT NULL,
    author TEXT NOT NULL,
    content TEXT,
    actions JSONB NOT NULL DEFAULT '{}',
    timestamp BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_app_user ON sessions(app_name, user_id);
"#;

/// SQL migration for SQLite.
pub const SQLITE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    app_name TEXT NOT NULL,
    user_id TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT '{}',
    create_time TEXT NOT NULL DEFAULT (datetime('now')),
    update_time TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    invocation_id TEXT NOT NULL,
    author TEXT NOT NULL,
    content TEXT,
    actions TEXT NOT NULL DEFAULT '{}',
    timestamp INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_app_user ON sessions(app_name, user_id);
"#;
