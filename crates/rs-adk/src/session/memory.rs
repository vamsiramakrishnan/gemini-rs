//! In-memory session service using DashMap.

use async_trait::async_trait;
use dashmap::DashMap;

use super::{Session, SessionError, SessionId, SessionService};
use crate::events::Event;

/// In-memory session service backed by [`DashMap`] for lock-free concurrent access.
///
/// Suitable for testing, prototyping, and single-process deployments.
/// Sessions are lost on process restart.
pub struct InMemorySessionService {
    sessions: DashMap<String, Session>,
    events: DashMap<String, Vec<Event>>,
}

impl InMemorySessionService {
    /// Create a new in-memory session service.
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            events: DashMap::new(),
        }
    }
}

impl Default for InMemorySessionService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionService for InMemorySessionService {
    async fn create_session(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<Session, SessionError> {
        let session = Session::new(app_name, user_id);
        let id = session.id.as_str().to_string();
        self.sessions.insert(id.clone(), session.clone());
        self.events.insert(id, Vec::new());
        Ok(session)
    }

    async fn get_session(&self, id: &SessionId) -> Result<Option<Session>, SessionError> {
        Ok(self.sessions.get(id.as_str()).map(|r| r.clone()))
    }

    async fn list_sessions(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<Vec<Session>, SessionError> {
        let result: Vec<Session> = self
            .sessions
            .iter()
            .filter(|r| r.app_name == app_name && r.user_id == user_id)
            .map(|r| r.clone())
            .collect();
        Ok(result)
    }

    async fn delete_session(&self, id: &SessionId) -> Result<(), SessionError> {
        self.sessions.remove(id.as_str());
        self.events.remove(id.as_str());
        Ok(())
    }

    async fn append_event(&self, id: &SessionId, event: Event) -> Result<(), SessionError> {
        let mut events = self
            .events
            .get_mut(id.as_str())
            .ok_or_else(|| SessionError::NotFound(id.clone()))?;
        events.push(event);
        Ok(())
    }

    async fn get_events(&self, id: &SessionId) -> Result<Vec<Event>, SessionError> {
        let events = self
            .events
            .get(id.as_str())
            .ok_or_else(|| SessionError::NotFound(id.clone()))?;
        Ok(events.clone())
    }
}
