# Session Persistence

The SDK has two distinct persistence systems that solve different problems:

| System | Purpose | API entry point |
|--------|---------|-----------------|
| **Live `SessionPersistence`** | Survive process restarts mid-conversation — snapshot/resume of a running Live session | `.persistence(...)` + `.session_id(...)` on `Live::builder()` |
| **`SessionService`** | Multi-session, multi-turn CRUD storage — create/list/delete sessions and append events | Standalone service, independent of the Live builder |

They are independent. You can use either, both, or neither depending on your needs.

---

## Live Session Persistence

### Why persist a Live session

The Gemini Live API supports session resumption via opaque server-issued handles.
When a process restarts (or a WebSocket drops), the SDK can reconnect to the same
server-side session and restore the client-side control plane state: `State`
key-value pairs, the current phase, turn count, and a transcript summary.

Without persistence, a reconnecting client starts a fresh session with no memory
of what came before.

### SessionSnapshot

The snapshot that is saved and restored holds the fields that the control plane
needs to pick up exactly where it left off:

```rust,ignore
pub struct SessionSnapshot {
    pub state: HashMap<String, Value>,       // all State key-value pairs
    pub phase: String,                       // current phase name
    pub turn_count: u32,                     // turns completed
    pub transcript_summary: String,          // brief human-readable summary
    pub resume_handle: Option<String>,       // opaque token from the Gemini server
    pub saved_at: String,                    // ISO 8601 timestamp
}
```

### Built-in backends

#### FsPersistence

Writes each snapshot as a JSON file under a configurable directory. The directory
is created automatically on first save.

```rust,ignore
use gemini_adk_rs::live::persistence::FsPersistence;
use std::sync::Arc;

Live::builder()
    .instruction("You are a helpful assistant")
    .persistence(Arc::new(FsPersistence::new("/var/lib/myapp/sessions")))
    .session_id("user-42-session-7")
    .connect_from_env()
    .await?;
```

Files are stored at `<dir>/<session_id>.json`. `FsPersistence` is suited for
development and single-server deployments. It is not safe for concurrent access
from multiple processes.

#### MemoryPersistence

Holds snapshots in a `DashMap`. Data is lost when the process exits. Useful for
tests or in-process restart simulations:

```rust,ignore
use gemini_adk_rs::live::persistence::MemoryPersistence;
use std::sync::Arc;

let persistence = Arc::new(MemoryPersistence::new());
Live::builder()
    .persistence(persistence.clone())
    .session_id("test-session")
    .connect_from_env()
    .await?;
```

### Wiring persistence to the builder

Two builder methods work together:

```rust,ignore
Live::builder()
    .persistence(Arc::new(FsPersistence::new("/var/lib/sessions")))
    .session_id("user-{user_id}")
    // optional: react when a prior snapshot is loaded
    .on_resumed(|snapshot, state| async move {
        println!(
            "Resumed from phase '{}', turn {}",
            snapshot.phase, snapshot.turn_count
        );
        // State is already restored — you can inspect or override keys here
    })
    .connect_from_env()
    .await?;
```

When a snapshot exists for the given `session_id`, the SDK restores `State`,
seeks the phase machine to the saved phase, and fires `on_resumed`. The
`resume_handle` in the snapshot is forwarded to the Gemini server so the
conversation history is not lost.

A final snapshot is also written **synchronously** when the control lane shuts
down (disconnect or server close), so state accumulated since the last turn
boundary is captured even if the process exits immediately afterwards.

### Manual resume after GoAway (server-side handles)

Independent of snapshots, the Gemini server itself supports session
resumption: with resumption enabled it periodically issues opaque handles, and
presenting the latest handle on the next connect continues the **server-side**
conversation (model context included).

`LiveHandle::resume_handle()` exposes the latest handle at any time — most
usefully inside `on_go_away`, which fires when the server announces it will
close the connection soon:

```rust,ignore
// Session 1: enable resumption and capture the handle.
let handle = Live::builder()
    .session_resume(true)
    .on_go_away(|time_left| async move {
        println!("Server closing in {time_left:?} — capture the resume handle now");
    })
    .connect_from_env()
    .await?;

// ... conversation runs; the server keeps issuing fresh handles ...

let resume = handle.resume_handle();   // Option<String>
handle.disconnect().await?;

// Session 2: present the handle on the next connect.
let mut builder = Live::builder().instruction("…");
if let Some(h) = resume {
    builder = builder.session_resume_from(h);   // resumption stays enabled
}
let handle = builder.connect_from_env().await?;
```

The SDK performs **no automatic reconnect** — when and whether to resume is an
explicit application decision. The same handle is also captured in every
persistence snapshot (`SessionSnapshot::resume_handle`), so a process restart
can resume from storage instead of memory.

### Custom persistence backends

Implement the `SessionPersistence` trait to target Redis, Firestore, DynamoDB,
or any other store:

```rust,ignore
use async_trait::async_trait;
use gemini_adk_rs::live::persistence::{PersistenceError, SessionPersistence, SessionSnapshot};

struct RedisSessionPersistence { client: redis::Client }

#[async_trait]
impl SessionPersistence for RedisSessionPersistence {
    async fn save(
        &self,
        session_id: &str,
        snapshot: &SessionSnapshot,
    ) -> Result<(), PersistenceError> {
        let json = serde_json::to_string(snapshot)?; // serde_json::Error → PersistenceError::Serde
        let mut conn = self.client.get_async_connection().await.map_err(backend)?;
        redis::cmd("SET").arg(session_id).arg(json).query_async(&mut conn).await.map_err(backend)?;
        Ok(())
    }

    async fn load(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionSnapshot>, PersistenceError> {
        let mut conn = self.client.get_async_connection().await.map_err(backend)?;
        let json: Option<String> =
            redis::cmd("GET").arg(session_id).query_async(&mut conn).await.map_err(backend)?;
        Ok(json.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    async fn delete(
        &self,
        session_id: &str,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.client.get_async_connection().await.map_err(backend)?;
        redis::cmd("DEL").arg(session_id).query_async(&mut conn).await.map_err(backend)?;
        Ok(())
    }
}

/// Store-specific failures map to `PersistenceError::Backend`; I/O and serde
/// errors convert with `?` (`PersistenceError::Io` / `PersistenceError::Serde`).
fn backend(e: redis::RedisError) -> PersistenceError {
    PersistenceError::Backend(e.to_string())
}
```

---

## SessionService: Multi-Session CRUD Storage

`SessionService` is a separate, higher-level trait modelled after ADK-Python's
`BaseSessionService`. It manages a collection of named sessions, each with an
ordered event log. This is the right tool when you need to:

- Track many simultaneous user sessions
- Query all sessions for a given user
- Store and replay conversation history
- Integrate with a team's existing database

### The SessionService trait

```rust,ignore
#[async_trait]
pub trait SessionService: Send + Sync {
    async fn create_session(&self, app_name: &str, user_id: &str) -> Result<Session, SessionError>;
    async fn get_session(&self, id: &SessionId) -> Result<Option<Session>, SessionError>;
    async fn list_sessions(&self, app_name: &str, user_id: &str) -> Result<Vec<Session>, SessionError>;
    async fn delete_session(&self, id: &SessionId) -> Result<(), SessionError>;
    async fn append_event(&self, id: &SessionId, event: Event) -> Result<(), SessionError>;
    async fn get_events(&self, id: &SessionId) -> Result<Vec<Event>, SessionError>;
}
```

### InMemorySessionService (default)

Available with no feature flags. Data is lost on process exit; good for tests
and local development:

```rust,ignore
use gemini_adk_rs::session::InMemorySessionService;

let svc = InMemorySessionService::new();
let session = svc.create_session("my-app", "user-1").await?;
svc.append_event(&session.id, Event::new("user", Some("Hello!".to_string()))).await?;
let events = svc.get_events(&session.id).await?;
```

### SqliteSessionService

File-based persistence for single-server deployments. Uses `sqlx` under the
`database-sessions` feature. Without that feature it falls back to the
in-memory backend so the default build stays dependency-free.

```toml
# Cargo.toml
[dependencies]
gemini-adk-rs = { version = "0.6", features = ["database-sessions"] }
```

```rust,ignore
use gemini_adk_rs::session::{SqliteSessionConfig, SqliteSessionService};
use std::path::PathBuf;

// File-based
let svc = SqliteSessionService::new(SqliteSessionConfig {
    db_path: PathBuf::from("/var/lib/myapp/sessions.db"),
});

// In-memory (for tests)
let svc = SqliteSessionService::new(SqliteSessionConfig::in_memory());

let session = svc.create_session("my-app", "user-1").await?;
```

The database schema (`sessions` and `events` tables, with indexes on
`(app_name, user_id)` and `session_id`) is created automatically on first use.

### PostgresSessionService

Horizontally scalable persistence for production. Requires the
`postgres-sessions` feature (which implies `database-sessions`):

```toml
[dependencies]
gemini-adk-rs = { version = "0.6", features = ["postgres-sessions"] }
```

```rust,ignore
use gemini_adk_rs::session::{PostgresSessionConfig, PostgresSessionService};

let svc = PostgresSessionService::new(
    PostgresSessionConfig::new("postgres://user:pass@db-host/myapp")
        .max_connections(20),
);

// Optionally run schema migration eagerly (otherwise it runs lazily):
svc.initialize().await?;

let session = svc.create_session("my-app", "user-1").await?;
svc.append_event(&session.id, Event::new("user", Some("Hello".to_string()))).await?;
```

The Postgres schema uses `BIGINT` for the event sequence column (vs `INTEGER`
for SQLite) and handles concurrent `append_event` calls safely with up to 8
retry attempts on sequence-number collisions.

### VertexAiSessionService

Delegates storage to Google Cloud's managed Vertex AI session endpoint. Sessions
are stored server-side and can optionally expire via a configurable TTL. Requires
the `vertex-ai-sessions` feature:

```toml
[dependencies]
gemini-adk-rs = { version = "0.6", features = ["vertex-ai-sessions"] }
```

```rust,ignore
use gemini_adk_rs::session::{VertexAiSessionConfig, VertexAiSessionService};

let svc = VertexAiSessionService::new(
    VertexAiSessionConfig::new("my-gcp-project", "us-central1")
        .ttl_seconds(3600),   // sessions expire after 1 hour of inactivity
)
.with_token("ya29.my-access-token")
.reasoning_engine("my-engine-id");   // defaults to "default"

let session = svc.create_session("my-app", "user-1").await?;
```

For long-running services, supply a dynamic token refresher instead of a static
token:

```rust,ignore
let svc = VertexAiSessionService::new(config)
    .with_token_refresher(|| fetch_access_token()); // called before every request
```

Vertex AI stores sessions under a `reasoningEngines/<engine-id>/sessions`
resource path. The service maps Vertex's `sessionState`, `userId`, and event
shapes back to the SDK's `Session` and `Event` types.

### Feature flag summary

| Feature | Backend | Notes |
|---------|---------|-------|
| _(none)_ | `InMemorySessionService` | Default; no dependencies |
| `database-sessions` | `SqliteSessionService` (real) | Requires `sqlx` |
| `postgres-sessions` | `PostgresSessionService` | Implies `database-sessions` |
| `vertex-ai-sessions` | `VertexAiSessionService` | Requires `reqwest` |

### Production notes

- **SQLite**: single-process only. The connection pool uses one connection for
  in-memory databases so state is not lost between calls.
- **Postgres**: safe for multi-process and containerised deployments. Call
  `initialize()` once at startup to run the schema migration before traffic
  arrives.
- **Vertex AI**: auth tokens must be provided externally. Use
  `with_token_refresher` in production to avoid token expiry mid-request.
- `SessionService` and `SessionPersistence` (Live snapshots) are orthogonal.
  A voice bot can use `VertexAiSessionService` for session tracking while also
  using `FsPersistence` for Live snapshot/resume — they do not share storage.

## See also

- [Live Sessions](./live-sessions.md) — connecting, disconnecting, and the `LiveHandle` runtime API
- [Live Callbacks](./live-callbacks.md) — `on_resumed` callback fired when a prior snapshot is restored
- [cookbook 35 — session persistence](../../examples/cookbook/src/35_session_persistence.rs)
