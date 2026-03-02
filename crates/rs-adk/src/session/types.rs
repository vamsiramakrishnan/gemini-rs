//! Session and SessionId types.

use serde::{Deserialize, Serialize};

/// Unique identifier for a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    /// Create a new random session ID.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Create a session ID from an existing string.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A persistent session with metadata and state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier.
    pub id: SessionId,
    /// Application name this session belongs to.
    pub app_name: String,
    /// User identifier.
    pub user_id: String,
    /// Session state as key-value pairs.
    pub state: std::collections::HashMap<String, serde_json::Value>,
    /// When the session was created (ISO 8601).
    pub created_at: String,
    /// When the session was last updated (ISO 8601).
    pub updated_at: String,
}

impl Session {
    /// Create a new session.
    pub fn new(app_name: impl Into<String>, user_id: impl Into<String>) -> Self {
        let now = now_iso8601();
        Self {
            id: SessionId::new(),
            app_name: app_name.into(),
            user_id: user_id.into(),
            state: std::collections::HashMap::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

fn now_iso8601() -> String {
    // Simple UTC timestamp without chrono dependency
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}Z", dur.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_display() {
        let id = SessionId::from_string("test-123");
        assert_eq!(id.to_string(), "test-123");
        assert_eq!(id.as_str(), "test-123");
    }

    #[test]
    fn session_id_equality() {
        let a = SessionId::from_string("abc");
        let b = SessionId::from_string("abc");
        assert_eq!(a, b);
    }

    #[test]
    fn session_new() {
        let s = Session::new("my-app", "user-1");
        assert_eq!(s.app_name, "my-app");
        assert_eq!(s.user_id, "user-1");
        assert!(s.state.is_empty());
    }
}
