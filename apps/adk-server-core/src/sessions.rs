//! Pluggable session store trait + in-memory default implementation.

use crate::types::{SessionData, now_iso8601};
use std::collections::HashMap;

/// Trait for session storage backends.
///
/// Default implementation: [`InMemorySessionStore`].
/// Swap in `SqliteSessionService`, `PostgresSessionService`, or
/// `VertexAiSessionService` from rs-adk for production.
pub trait SessionStore: Send + Sync {
    /// List sessions for an app+user pair.
    fn list(&self, app: &str, user: &str, limit: usize, offset: usize) -> Vec<SessionData>;

    /// Create a new session. Returns the created session.
    fn create(&self, app: &str, user: &str) -> SessionData;

    /// Get a session by ID.
    fn get(&self, id: &str) -> Option<SessionData>;

    /// Delete a session. Returns true if it existed.
    fn delete(&self, id: &str) -> bool;

    /// Get events for a session.
    fn events(&self, id: &str) -> Vec<serde_json::Value>;

    /// Get state for a session.
    fn state(&self, id: &str) -> HashMap<String, serde_json::Value>;

    /// Append an event to a session.
    fn append_event(&self, id: &str, event: serde_json::Value);

    /// Update state for a session.
    fn update_state(&self, id: &str, key: String, value: serde_json::Value);

    /// Rewind a session to a given invocation. Returns events removed.
    fn rewind(&self, id: &str, invocation_id: &str) -> usize;

    /// Count of active sessions.
    fn count(&self) -> usize;
}

/// Thread-safe in-memory session store.
pub struct InMemorySessionStore {
    sessions: parking_lot::RwLock<HashMap<String, SessionData>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self {
            sessions: parking_lot::RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemorySessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore for InMemorySessionStore {
    fn list(&self, app: &str, user: &str, limit: usize, offset: usize) -> Vec<SessionData> {
        self.sessions
            .read()
            .values()
            .filter(|s| s.app_name == app && s.user_id == user)
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }

    fn create(&self, app: &str, user: &str) -> SessionData {
        let now = now_iso8601();
        let session = SessionData {
            id: uuid::Uuid::new_v4().to_string(),
            app_name: app.to_string(),
            user_id: user.to_string(),
            state: HashMap::new(),
            events: vec![],
            created_at: now.clone(),
            updated_at: now,
        };
        self.sessions
            .write()
            .insert(session.id.clone(), session.clone());
        session
    }

    fn get(&self, id: &str) -> Option<SessionData> {
        self.sessions.read().get(id).cloned()
    }

    fn delete(&self, id: &str) -> bool {
        self.sessions.write().remove(id).is_some()
    }

    fn events(&self, id: &str) -> Vec<serde_json::Value> {
        self.sessions
            .read()
            .get(id)
            .map(|s| s.events.clone())
            .unwrap_or_default()
    }

    fn state(&self, id: &str) -> HashMap<String, serde_json::Value> {
        self.sessions
            .read()
            .get(id)
            .map(|s| s.state.clone())
            .unwrap_or_default()
    }

    fn append_event(&self, id: &str, event: serde_json::Value) {
        let mut sessions = self.sessions.write();
        if let Some(session) = sessions.get_mut(id) {
            session.events.push(event);
            session.updated_at = now_iso8601();
        }
    }

    fn update_state(&self, id: &str, key: String, value: serde_json::Value) {
        let mut sessions = self.sessions.write();
        if let Some(session) = sessions.get_mut(id) {
            session.state.insert(key, value);
            session.updated_at = now_iso8601();
        }
    }

    fn rewind(&self, id: &str, invocation_id: &str) -> usize {
        let mut sessions = self.sessions.write();
        let Some(session) = sessions.get_mut(id) else {
            return 0;
        };

        let cutoff = session.events.iter().rposition(|e| {
            e.get("invocation_id")
                .and_then(|v| v.as_str())
                == Some(invocation_id)
        });

        match cutoff {
            Some(idx) => {
                let removed = session.events.len() - (idx + 1);
                session.events.truncate(idx + 1);
                session.updated_at = now_iso8601();
                removed
            }
            None => 0,
        }
    }

    fn count(&self) -> usize {
        self.sessions.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get() {
        let store = InMemorySessionStore::new();
        let session = store.create("app", "user");
        assert_eq!(store.get(&session.id).unwrap().app_name, "app");
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn delete_session() {
        let store = InMemorySessionStore::new();
        let session = store.create("app", "user");
        assert!(store.delete(&session.id));
        assert!(!store.delete(&session.id));
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn list_filters_by_app_user() {
        let store = InMemorySessionStore::new();
        store.create("app1", "user1");
        store.create("app1", "user1");
        store.create("app2", "user1");

        assert_eq!(store.list("app1", "user1", 50, 0).len(), 2);
        assert_eq!(store.list("app2", "user1", 50, 0).len(), 1);
        assert_eq!(store.list("app1", "user2", 50, 0).len(), 0);
    }

    #[test]
    fn append_and_get_events() {
        let store = InMemorySessionStore::new();
        let session = store.create("app", "user");
        store.append_event(&session.id, serde_json::json!({"type": "text"}));
        store.append_event(&session.id, serde_json::json!({"type": "tool"}));
        assert_eq!(store.events(&session.id).len(), 2);
    }

    #[test]
    fn state_update() {
        let store = InMemorySessionStore::new();
        let session = store.create("app", "user");
        store.update_state(&session.id, "key".into(), serde_json::json!("val"));
        let state = store.state(&session.id);
        assert_eq!(state.get("key").unwrap(), &serde_json::json!("val"));
    }
}
