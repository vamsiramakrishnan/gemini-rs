//! `TextRunner` — runs `TextAgent`s with session management and services.
//!
//! Provides a complete runtime for text-based agent execution with automatic
//! session management, memory, artifacts, and plugin hooks.

use std::sync::Arc;

use futures_util::stream::{self, BoxStream, StreamExt};

use crate::artifacts::{ArtifactService, InMemoryArtifactService};
use crate::error::AgentError;
use crate::events::{Event, EventActions};
use crate::memory::{InMemoryMemoryService, MemoryService};
use crate::plugin::{Plugin, PluginManager};
use crate::session::{InMemorySessionService, SessionId, SessionService};
use crate::state::State;
use crate::text::TextAgent;

/// An item yielded by [`TextRunner::run_stream`].
///
/// Mirrors ADK-Python's `Runner.run_async()` event stream. Most items are
/// [`RunEvent::Event`] carrying a persisted [`Event`]; a terminal
/// [`RunEvent::Error`] is yielded if setup or the agent run fails, after which
/// the stream ends.
#[derive(Debug)]
pub enum RunEvent {
    /// A structured event produced during the run (user input, agent response,
    /// state deltas). Boxed to keep the enum small (the error variant is tiny).
    Event(Box<Event>),
    /// A terminal error. No further items follow.
    Error(AgentError),
}

impl RunEvent {
    /// Construct an event item.
    fn event_item(event: Event) -> Self {
        RunEvent::Event(Box::new(event))
    }

    /// Borrow the inner [`Event`], if this is an event item.
    pub fn event(&self) -> Option<&Event> {
        match self {
            RunEvent::Event(e) => Some(e),
            RunEvent::Error(_) => None,
        }
    }
}

/// Internal driver state for the [`TextRunner::run_stream`] state machine.
enum RunStep {
    /// Create/load the session, replay deltas, emit the user event.
    Start,
    /// Run the agent and emit its response event.
    Run {
        session_id: SessionId,
        state: State,
        baseline: std::collections::HashMap<String, serde_json::Value>,
    },
    /// Stream complete.
    Done,
}

/// Runs TextAgents with full service wiring (session, memory, artifacts, plugins).
///
/// Auto-wires in-memory service implementations by default; override with
/// builder methods for custom persistence.
pub struct TextRunner {
    root_agent: Arc<dyn TextAgent>,
    session_service: Arc<dyn SessionService>,
    memory_service: Arc<dyn MemoryService>,
    artifact_service: Arc<dyn ArtifactService>,
    plugins: PluginManager,
    app_name: String,
}

impl TextRunner {
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
    ///
    /// This is a thin convenience wrapper over [`run_stream`](Self::run_stream):
    /// it drains the event stream and returns the final response text (or the
    /// surfaced error). Both methods share the exact same execution path.
    pub async fn run(
        &self,
        prompt: &str,
        user_id: &str,
        session_id: Option<&SessionId>,
    ) -> Result<String, AgentError> {
        let mut stream = self.run_stream(prompt, user_id, session_id);
        let mut last_response: Option<String> = None;
        while let Some(item) = stream.next().await {
            match item {
                RunEvent::Error(e) => return Err(e),
                RunEvent::Event(ev) => {
                    // The final response event is authored by the root agent.
                    if ev.author == self.root_agent.name() {
                        last_response = ev.content.clone();
                    }
                }
            }
        }
        Ok(last_response.unwrap_or_default())
    }

    /// Run the agent as an event stream, mirroring ADK-Python's
    /// `Runner.run_async()` which yields a `Stream<Event>`.
    ///
    /// Yields [`RunEvent`] items as they happen, backed by the **same**
    /// execution path as [`run`](Self::run):
    ///
    /// 1. a user event carrying the prompt (also persisted to the session),
    /// 2. zero or more agent events surfacing state deltas produced during the
    ///    run (one per changed key, so eval harnesses and UIs can observe
    ///    mutations), and
    /// 3. a final response event authored by the root agent (also persisted).
    ///
    /// Setup failures (session create/load, event persistence) and agent
    /// failures are surfaced as a terminal [`RunEvent::Error`] rather than a
    /// `Result`, so the item type stays a plain event. After an error the
    /// stream ends.
    ///
    /// `run(prompt, user, session)` is equivalent to draining this stream and
    /// returning the last response event's content.
    pub fn run_stream<'a>(
        &'a self,
        prompt: &'a str,
        user_id: &'a str,
        session_id: Option<&'a SessionId>,
    ) -> BoxStream<'a, RunEvent> {
        // Snapshot a pre-run state baseline so we can diff state deltas the
        // agent produced. The agent only exposes a final `String`, so the
        // honestly-observable mid-run signal is the set of state keys it wrote.
        let prompt = prompt.to_string();
        let user_id = user_id.to_string();
        let session_id = session_id.cloned();

        stream::unfold(RunStep::Start, move |step| {
            let prompt = prompt.clone();
            let user_id = user_id.clone();
            let session_id = session_id.clone();
            async move {
                match step {
                    RunStep::Start => {
                        // 1. Create or load session.
                        let session = match &session_id {
                            Some(id) => match self.session_service.get_session(id).await {
                                Ok(Some(s)) => s,
                                Ok(None) => {
                                    return Some((
                                        RunEvent::Error(AgentError::Other(format!(
                                            "Session not found: {id}"
                                        ))),
                                        RunStep::Done,
                                    ));
                                }
                                Err(e) => {
                                    return Some((
                                        RunEvent::Error(AgentError::Other(format!(
                                            "Session error: {e}"
                                        ))),
                                        RunStep::Done,
                                    ));
                                }
                            },
                            None => match self
                                .session_service
                                .create_session(&self.app_name, &user_id)
                                .await
                            {
                                Ok(s) => s,
                                Err(e) => {
                                    return Some((
                                        RunEvent::Error(AgentError::Other(format!(
                                            "Session create error: {e}"
                                        ))),
                                        RunStep::Done,
                                    ));
                                }
                            },
                        };

                        // 2. Build state and replay prior deltas.
                        let state = State::new();
                        let prior = match self.session_service.get_events(&session.id).await {
                            Ok(evs) => evs,
                            Err(e) => {
                                return Some((
                                    RunEvent::Error(AgentError::Other(format!(
                                        "Events error: {e}"
                                    ))),
                                    RunStep::Done,
                                ));
                            }
                        };
                        for event in &prior {
                            // An event written before 1.0.1 carries no format
                            // marker: nothing in it was escaped, it has no
                            // removal channel, and every entry is a literal
                            // state key. Decoding one would rename the keys the
                            // application chose.
                            let encoded = event.actions.is_format_marked();
                            for (key, value) in &event.actions.state_delta {
                                if encoded
                                    && (key == EventActions::REMOVED_KEYS
                                        || key == EventActions::FORMAT)
                                {
                                    continue;
                                }
                                let key = if encoded {
                                    EventActions::decode_key(key).into_owned()
                                } else {
                                    key.clone()
                                };
                                let _ = state.set(key, value.clone());
                            }
                            // Removals ride in their own reserved entry and are
                            // applied after the sets: a stored `null` is a value
                            // like any other and must survive replay, so deletion
                            // cannot be spelled as one.
                            for key in event.actions.removed_keys() {
                                let _ = state.remove(key);
                            }
                        }
                        let _ = state.set("input", &prompt);

                        // Snapshot keys present before the agent runs.
                        let baseline = state.to_hashmap();

                        // Persist the user event.
                        let user_event = Event::new("user", Some(prompt.clone()));
                        if let Err(e) = self
                            .session_service
                            .append_event(&session.id, user_event.clone())
                            .await
                        {
                            return Some((
                                RunEvent::Error(AgentError::Other(format!(
                                    "Event append error: {e}"
                                ))),
                                RunStep::Done,
                            ));
                        }

                        // Emit the user event, carry state into the next step.
                        Some((
                            RunEvent::event_item(user_event),
                            RunStep::Run {
                                session_id: session.id,
                                state,
                                baseline,
                            },
                        ))
                    }
                    RunStep::Run {
                        session_id,
                        state,
                        baseline,
                    } => {
                        // 3. Run the agent (same path as `run`).
                        let result = match self.root_agent.run(&state).await {
                            Ok(r) => r,
                            Err(e) => return Some((RunEvent::Error(e), RunStep::Done)),
                        };

                        // Diff state to surface deltas the agent produced.
                        let after = state.to_hashmap();
                        let mut delta = std::collections::HashMap::new();
                        for (key, value) in &after {
                            if key == "input" {
                                continue;
                            }
                            if baseline.get(key) != Some(value) {
                                // Escaped, so a state key that collides with the
                                // reserved removal entry is carried rather than
                                // dropped — and cannot forge a deletion either.
                                delta.insert(
                                    EventActions::encode_key(key).into_owned(),
                                    value.clone(),
                                );
                            }
                        }
                        // Keys the agent removed: absent from `after` but present
                        // in the baseline. Recorded under the reserved entry, not
                        // as `null` values, so replay deletes them without making
                        // a deliberately-stored `null` indistinguishable from one.
                        // These are values in a JSON array, not map keys, so they
                        // need no escaping.
                        let removed: Vec<serde_json::Value> = baseline
                            .keys()
                            .filter(|key| *key != "input" && !after.contains_key(*key))
                            .map(|key| serde_json::Value::String(key.clone()))
                            .collect();
                        if !removed.is_empty() {
                            delta.insert(
                                EventActions::REMOVED_KEYS.to_string(),
                                serde_json::Value::Array(removed),
                            );
                        }

                        let mut actions = EventActions {
                            state_delta: delta,
                            ..Default::default()
                        };
                        // Marks the delta as escaped and its removal entry as a
                        // deletion list, so replay never has to guess which era
                        // an event came from.
                        actions.mark_format();
                        let result_event = Event::new(self.root_agent.name(), Some(result.clone()))
                            .with_actions(actions);

                        // 4. Persist the result event.
                        if let Err(e) = self
                            .session_service
                            .append_event(&session_id, result_event.clone())
                            .await
                        {
                            return Some((
                                RunEvent::Error(AgentError::Other(format!(
                                    "Event append error: {e}"
                                ))),
                                RunStep::Done,
                            ));
                        }

                        Some((RunEvent::event_item(result_event), RunStep::Done))
                    }
                    RunStep::Done => None,
                }
            }
        })
        .boxed()
    }

    /// Run without persistence (one-shot, ephemeral).
    pub async fn run_ephemeral(&self, prompt: &str) -> Result<String, AgentError> {
        let state = State::new();
        let _ = state.set("input", prompt);
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
        let runner = TextRunner::new(echo_agent(), "test-app");
        let result = runner.run_ephemeral("Hello").await.unwrap();
        assert_eq!(result, "Echo: Hello");
    }

    #[tokio::test]
    async fn run_with_session_creates_and_persists() {
        let runner = TextRunner::new(echo_agent(), "test-app");

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
        let runner = TextRunner::new(echo_agent(), "test-app");

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
        let result2 = runner
            .run("Second", "user-1", Some(session_id))
            .await
            .unwrap();
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
        let runner = TextRunner::new(echo_agent(), "test-app");
        let fake_id = SessionId::new();
        let result = runner.run("Hello", "user-1", Some(&fake_id)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn custom_session_service() {
        let custom_svc = Arc::new(InMemorySessionService::new());
        let runner = TextRunner::new(echo_agent(), "app").session_service(custom_svc.clone());

        runner.run("Hi", "u1", None).await.unwrap();

        let sessions = custom_svc.list_sessions("app", "u1").await.unwrap();
        assert_eq!(sessions.len(), 1);
    }

    /// A mock agent that writes a state delta and echoes — exercises the
    /// state-delta-surfacing path of `run_stream`.
    fn delta_agent() -> Arc<dyn TextAgent> {
        Arc::new(FnTextAgent::new("worker", |state| {
            let input: String = state.get("input").unwrap_or_default();
            let _ = state.set("turn_count", 1u32);
            Ok(format!("Handled: {input}"))
        }))
    }

    #[tokio::test]
    async fn run_stream_yields_user_then_final_event() {
        let runner = TextRunner::new(echo_agent(), "test-app");
        let mut stream = runner.run_stream("Hello", "user-1", None);

        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                RunEvent::Event(e) => events.push(e),
                RunEvent::Error(e) => panic!("unexpected error: {e}"),
            }
        }

        // First a user event, then the agent's final response event.
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].author, "user");
        assert_eq!(events[0].content.as_deref(), Some("Hello"));
        assert_eq!(events[1].author, "echo");
        assert_eq!(events[1].content.as_deref(), Some("Echo: Hello"));
    }

    #[tokio::test]
    async fn run_stream_surfaces_state_delta_on_final_event() {
        let runner = TextRunner::new(delta_agent(), "test-app");
        let mut stream = runner.run_stream("go", "user-1", None);

        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            if let RunEvent::Event(e) = item {
                events.push(e);
            }
        }

        let final_event = events.last().expect("final event");
        assert_eq!(final_event.author, "worker");
        assert_eq!(
            final_event.actions.state_delta.get("turn_count"),
            Some(&serde_json::json!(1))
        );
    }

    #[tokio::test]
    async fn removed_keys_are_recorded_and_stay_removed_across_replay() {
        // set → clear → peek on ONE session: the clear run must record the key
        // as removed, and the peek run's replay must honor that instead of
        // resurrecting the value from the earlier delta.
        let agent = Arc::new(FnTextAgent::new("worker", |state| {
            let input: String = state.get("input").unwrap_or_default();
            match input.as_str() {
                "set" => {
                    let _ = state.set("flag", true);
                }
                "clear" => {
                    let _ = state.remove("flag");
                }
                _ => {}
            }
            Ok(format!("saw: {:?}", state.get::<bool>("flag")))
        }));
        let runner = TextRunner::new(agent, "test-app");

        runner.run("set", "user-1", None).await.unwrap();
        let sessions = runner
            .session_service_ref()
            .list_sessions("test-app", "user-1")
            .await
            .unwrap();
        let sid = sessions[0].id.clone();

        runner.run("clear", "user-1", Some(&sid)).await.unwrap();
        let events = runner.session_service_ref().get_events(&sid).await.unwrap();
        let clear_event = events
            .iter()
            .rev()
            .find(|e| e.author == "worker")
            .expect("clear run's agent event");
        assert!(
            clear_event.actions.removed_keys().any(|k| k == "flag"),
            "removal persisted under the reserved entry, got {:?}",
            clear_event.actions.state_delta
        );
        assert!(
            !clear_event.actions.state_delta.contains_key("flag"),
            "a removal must not also be written as a delta value"
        );

        let peeked = runner.run("peek", "user-1", Some(&sid)).await.unwrap();
        assert_eq!(
            peeked, "saw: None",
            "removed key did not resurrect on replay"
        );
    }

    #[tokio::test]
    async fn a_state_value_stored_at_the_reserved_key_survives_replay() {
        // `State` accepts arbitrary keys, so an application can legitimately
        // write to the name the removal channel uses. Escaping carries it
        // instead of dropping it — and it must not read back as a deletion.
        let agent = Arc::new(FnTextAgent::new("worker", |state| {
            let input: String = state.get("input").unwrap_or_default();
            if input == "store" {
                let _ = state.set("keep", "kept");
                let _ = state.set(EventActions::REMOVED_KEYS, vec!["keep"]);
            }
            Ok(format!(
                "{:?}/{:?}",
                state.get::<Vec<String>>(EventActions::REMOVED_KEYS),
                state.get::<String>("keep")
            ))
        }));
        let runner = TextRunner::new(agent, "test-app");

        runner.run("store", "user-1", None).await.unwrap();
        let sessions = runner
            .session_service_ref()
            .list_sessions("test-app", "user-1")
            .await
            .unwrap();
        let sid = sessions[0].id.clone();

        let events = runner.session_service_ref().get_events(&sid).await.unwrap();
        let stored = events
            .iter()
            .rev()
            .find(|e| e.author == "worker")
            .expect("store run\'s agent event");
        assert_eq!(
            stored.actions.removed_keys().count(),
            0,
            "a state value must not be mistaken for a removal list"
        );

        let peeked = runner.run("peek", "user-1", Some(&sid)).await.unwrap();
        assert_eq!(
            peeked, "Some([\"keep\"])/Some(\"kept\")",
            "the value at the reserved key was dropped, or read as a deletion"
        );
    }

    /// Events persisted by 1.0.0 outlive the upgrade — this repo ships SQLite,
    /// Postgres and Vertex AI session services. Such an event carries no format
    /// marker, so every key in it must replay literally: no `:literal` suffix
    /// stripped, and no string array mistaken for a deletion list.
    #[tokio::test]
    async fn a_pre_upgrade_event_replays_literally() {
        let agent = Arc::new(FnTextAgent::new("worker", |state| {
            Ok(format!(
                "{:?}/{:?}/{:?}",
                state.get::<String>("adk:removed:literal"),
                state.get::<Vec<String>>(EventActions::REMOVED_KEYS),
                state.get::<String>("survivor"),
            ))
        }));
        let runner = TextRunner::new(agent, "test-app");
        let sessions = runner.session_service_ref();
        let session = sessions.create_session("test-app", "user-1").await.unwrap();

        // Hand-built in the pre-1.0.1 shape: no marker, nothing escaped, and a
        // string array sitting at the name the removal channel later took.
        let mut delta = std::collections::HashMap::new();
        delta.insert(
            "adk:removed:literal".to_string(),
            serde_json::json!("laddered"),
        );
        delta.insert(
            EventActions::REMOVED_KEYS.to_string(),
            serde_json::json!(["survivor"]),
        );
        delta.insert("survivor".to_string(), serde_json::json!("still here"));
        let legacy = Event::new("worker", Some("legacy".into()))
            .with_actions(EventActions::state_delta(delta));
        assert!(
            !legacy.actions.is_format_marked(),
            "the fixture must look like a pre-upgrade event"
        );
        sessions.append_event(&session.id, legacy).await.unwrap();

        let peeked = runner
            .run("peek", "user-1", Some(&session.id))
            .await
            .unwrap();
        assert_eq!(
            peeked, "Some(\"laddered\")/Some([\"survivor\"])/Some(\"still here\")",
            "a pre-upgrade event was decoded under the new rules"
        );
    }

    #[tokio::test]
    async fn a_deliberately_stored_json_null_survives_replay() {
        // `null` is an ordinary value an agent may store on purpose. When
        // deletion was spelled as a null tombstone, replay deleted this key
        // instead of restoring it, so persistence was not lossless.
        let agent = Arc::new(FnTextAgent::new("worker", |state| {
            let input: String = state.get("input").unwrap_or_default();
            if input == "store" {
                let _ = state.set("maybe", serde_json::Value::Null);
            }
            Ok(format!("present: {}", state.contains("maybe")))
        }));
        let runner = TextRunner::new(agent, "test-app");

        runner.run("store", "user-1", None).await.unwrap();
        let sessions = runner
            .session_service_ref()
            .list_sessions("test-app", "user-1")
            .await
            .unwrap();
        let sid = sessions[0].id.clone();

        let peeked = runner.run("peek", "user-1", Some(&sid)).await.unwrap();
        assert_eq!(
            peeked, "present: true",
            "a stored JSON null must survive replay, not be read as a deletion"
        );
    }

    #[tokio::test]
    async fn run_stream_drains_to_same_result_as_run() {
        let runner = TextRunner::new(echo_agent(), "test-app");

        // Draining the stream's final response equals what `run` returns.
        let mut stream = runner.run_stream("Hi", "user-1", None);
        let mut last = None;
        while let Some(item) = stream.next().await {
            if let RunEvent::Event(e) = item
                && e.author == "echo"
            {
                last = e.content.clone();
            }
        }
        assert_eq!(last.as_deref(), Some("Echo: Hi"));
    }

    #[tokio::test]
    async fn run_stream_persists_events_like_run() {
        let runner = TextRunner::new(echo_agent(), "test-app");
        let mut stream = runner.run_stream("Hello", "user-1", None);
        while stream.next().await.is_some() {}
        drop(stream);

        let sessions = runner
            .session_service_ref()
            .list_sessions("test-app", "user-1")
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
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
    async fn run_stream_emits_error_for_missing_session() {
        let runner = TextRunner::new(echo_agent(), "test-app");
        let fake_id = SessionId::new();
        let mut stream = runner.run_stream("Hello", "user-1", Some(&fake_id));

        let mut saw_error = false;
        while let Some(item) = stream.next().await {
            if let RunEvent::Error(_) = item {
                saw_error = true;
            }
        }
        assert!(saw_error);
    }
}
