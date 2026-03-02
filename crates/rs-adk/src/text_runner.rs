//! InMemoryRunner — runs TextAgents with session management and services.
//!
//! Provides a complete runtime for text-based agent execution with automatic
//! session management, memory, artifacts, and plugin hooks.

use std::sync::Arc;

use crate::artifacts::{ArtifactService, InMemoryArtifactService};
use crate::error::AgentError;
use crate::events::Event;
use crate::memory::{InMemoryMemoryService, MemoryService};
use crate::plugin::{Plugin, PluginManager};
use crate::session::{InMemorySessionService, SessionId, SessionService};
use crate::state::State;
use crate::text::TextAgent;

/// Runs TextAgents with full service wiring (session, memory, artifacts, plugins).
///
/// Auto-wires in-memory service implementations by default; override with
/// builder methods for custom persistence.
pub struct InMemoryRunner {
    root_agent: Arc<dyn TextAgent>,
    session_service: Arc<dyn SessionService>,
    memory_service: Arc<dyn MemoryService>,
    artifact_service: Arc<dyn ArtifactService>,
    plugins: PluginManager,
    app_name: String,
}

impl InMemoryRunner {
    /// Create a new runner with in-memory defaults for all services.
    pub fn new(agent: Arc<dyn TextAgent>, app_name: impl Into<String>) -> Self {
        Self {
            root_agent: agent,
            session_service: Arc::new(InMemorySessionService::new()),
            memory_service: Arc::new(InMemoryMemoryService::new()),
            artifact_service: Arc::new(InMemoryArtifactService::new()),
            plugins: PluginManager::new(),
            app_name: app_name.into(),
        }
    }

    /// Override the session service.
    pub fn session_service(mut self, svc: Arc<dyn SessionService>) -> Self {
        self.session_service = svc;
        self
    }

    /// Override the memory service.
    pub fn memory_service(mut self, svc: Arc<dyn MemoryService>) -> Self {
        self.memory_service = svc;
        self
    }

    /// Override the artifact service.
    pub fn artifact_service(mut self, svc: Arc<dyn ArtifactService>) -> Self {
        self.artifact_service = svc;
        self
    }

    /// Add a plugin.
    pub fn plugin(mut self, p: impl Plugin + 'static) -> Self {
        self.plugins.add(Arc::new(p));
        self
    }

    /// Run with session management. Creates or resumes a session.
    ///
    /// 1. Creates a new session or loads an existing one
    /// 2. Sets `"input"` in state from `prompt`
    /// 3. Runs the agent
    /// 4. Persists the result as an event in the session
    /// 5. Returns the agent's text output
    pub async fn run(
        &self,
        prompt: &str,
        user_id: &str,
        session_id: Option<&SessionId>,
    ) -> Result<String, AgentError> {
        // 1. Create or load session
        let session = match session_id {
            Some(id) => {
                self.session_service
                    .get_session(id)
                    .await
                    .map_err(|e| AgentError::Other(format!("Session error: {e}")))?
                    .ok_or_else(|| AgentError::Other(format!("Session not found: {id}")))?
            }
            None => {
                self.session_service
                    .create_session(&self.app_name, user_id)
                    .await
                    .map_err(|e| AgentError::Other(format!("Session create error: {e}")))?
            }
        };

        // 2. Build state and set input
        let state = State::new();

        // Load existing events to rebuild state (state deltas)
        let events = self
            .session_service
            .get_events(&session.id)
            .await
            .map_err(|e| AgentError::Other(format!("Events error: {e}")))?;
        for event in &events {
            for (key, value) in &event.actions.state_delta {
                state.set(key.clone(), value.clone());
            }
        }

        state.set("input", prompt);

        // Persist user input event
        let user_event = Event::new("user", Some(prompt.to_string()));
        self.session_service
            .append_event(&session.id, user_event)
            .await
            .map_err(|e| AgentError::Other(format!("Event append error: {e}")))?;

        // 3. Run agent
        let result = self.root_agent.run(&state).await?;

        // 4. Persist result event
        let result_event = Event::new(self.root_agent.name(), Some(result.clone()));
        self.session_service
            .append_event(&session.id, result_event)
            .await
            .map_err(|e| AgentError::Other(format!("Event append error: {e}")))?;

        // 5. Return result
        Ok(result)
    }

    /// Run without persistence (one-shot, ephemeral).
    pub async fn run_ephemeral(&self, prompt: &str) -> Result<String, AgentError> {
        let state = State::new();
        state.set("input", prompt);
        self.root_agent.run(&state).await
    }

    /// Access the session service.
    pub fn session_service_ref(&self) -> &dyn SessionService {
        self.session_service.as_ref()
    }

    /// Access the app name.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::FnTextAgent;

    fn echo_agent() -> Arc<dyn TextAgent> {
        Arc::new(FnTextAgent::new("echo", |state| {
            let input: String = state.get("input").unwrap_or_default();
            Ok(format!("Echo: {input}"))
        }))
    }

    #[tokio::test]
    async fn run_ephemeral() {
        let runner = InMemoryRunner::new(echo_agent(), "test-app");
        let result = runner.run_ephemeral("Hello").await.unwrap();
        assert_eq!(result, "Echo: Hello");
    }

    #[tokio::test]
    async fn run_with_session_creates_and_persists() {
        let runner = InMemoryRunner::new(echo_agent(), "test-app");

        // First run — creates session
        let result = runner.run("Hello", "user-1", None).await.unwrap();
        assert_eq!(result, "Echo: Hello");

        // Verify session was created
        let sessions = runner
            .session_service_ref()
            .list_sessions("test-app", "user-1")
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);

        // Verify events were persisted (user input + agent response)
        let events = runner
            .session_service_ref()
            .get_events(&sessions[0].id)
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].author, "user");
        assert_eq!(events[1].author, "echo");
    }

    #[tokio::test]
    async fn run_resumes_existing_session() {
        let runner = InMemoryRunner::new(echo_agent(), "test-app");

        // Create a session via first run
        let result1 = runner.run("First", "user-1", None).await.unwrap();
        assert_eq!(result1, "Echo: First");

        // Get the session ID
        let sessions = runner
            .session_service_ref()
            .list_sessions("test-app", "user-1")
            .await
            .unwrap();
        let session_id = &sessions[0].id;

        // Resume with the same session
        let result2 = runner.run("Second", "user-1", Some(session_id)).await.unwrap();
        assert_eq!(result2, "Echo: Second");

        // Should have 4 events total (2 per run)
        let events = runner
            .session_service_ref()
            .get_events(session_id)
            .await
            .unwrap();
        assert_eq!(events.len(), 4);
    }

    #[tokio::test]
    async fn run_with_nonexistent_session_errors() {
        let runner = InMemoryRunner::new(echo_agent(), "test-app");
        let fake_id = SessionId::new();
        let result = runner.run("Hello", "user-1", Some(&fake_id)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn custom_session_service() {
        let custom_svc = Arc::new(InMemorySessionService::new());
        let runner = InMemoryRunner::new(echo_agent(), "app")
            .session_service(custom_svc.clone());

        runner.run("Hi", "u1", None).await.unwrap();

        let sessions = custom_svc.list_sessions("app", "u1").await.unwrap();
        assert_eq!(sessions.len(), 1);
    }
}
