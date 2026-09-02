use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::TextAgent;
use crate::context::AgentEvent;
use crate::error::AgentError;
use crate::middleware::MiddlewareChain;
use crate::state::State;

/// Background task handle: agent name → join handle yielding `Ok(json)`/`Err(msg)`.
type TaskMap = HashMap<String, tokio::task::JoinHandle<Result<String, String>>>;

/// Shared registry for dispatched background tasks.
#[derive(Clone, Default)]
pub struct TaskRegistry {
    pub(crate) inner: Arc<tokio::sync::Mutex<TaskMap>>,
}

impl TaskRegistry {
    /// Create a new empty task registry.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Fire-and-forget background task launcher with global task budget.
///
/// Launches each child agent as a background `tokio::spawn` task,
/// stores handles in a `TaskRegistry`, and returns immediately.
pub struct DispatchTextAgent {
    name: String,
    children: Vec<(String, Arc<dyn TextAgent>)>,
    registry: TaskRegistry,
    budget: Arc<tokio::sync::Semaphore>,
    middleware: MiddlewareChain,
}

impl DispatchTextAgent {
    /// Create a new dispatch agent with named children and a concurrency budget.
    pub fn new(
        name: impl Into<String>,
        children: Vec<(String, Arc<dyn TextAgent>)>,
        registry: TaskRegistry,
        budget: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            name: name.into(),
            children,
            registry,
            budget,
            middleware: MiddlewareChain::new(),
        }
    }

    /// Attach a middleware chain. `AgentEvent::AgentStarted` is emitted
    /// through it as each child task is launched, and `AgentEvent::AgentCompleted`
    /// from inside the detached task when the child finishes (the chain is
    /// cloned into the task, so this fires even after `run` has returned).
    pub fn with_middleware_chain(mut self, chain: MiddlewareChain) -> Self {
        self.middleware = chain;
        self
    }
}

#[async_trait]
impl TextAgent for DispatchTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut registry = self.registry.inner.lock().await;

        for (task_name, agent) in &self.children {
            let agent = agent.clone();
            let state = state.clone();
            let budget = self.budget.clone();
            let task_name_owned = task_name.clone();

            let handle = tokio::spawn(async move {
                let _permit = budget
                    .acquire()
                    .await
                    .map_err(|e| format!("Semaphore closed: {e}"))?;
                agent
                    .run(&state)
                    .await
                    .map_err(|e| format!("Task '{task_name_owned}' failed: {e}"))
            });

            registry.insert(task_name.clone(), handle);
        }

        let _ = state.set(
            "_dispatch_status",
            self.children
                .iter()
                .map(|(name, _)| (name.clone(), "running".to_string()))
                .collect::<HashMap<String, String>>(),
        );

        Ok(String::new())
    }
}

// ── JoinTextAgent ─────────────────────────────────────────────────────────

/// Waits for dispatched background tasks and collects their results.
pub struct JoinTextAgent {
    name: String,
    registry: TaskRegistry,
    target_names: Option<Vec<String>>,
    timeout: Option<Duration>,
    middleware: MiddlewareChain,
}

impl JoinTextAgent {
    /// Create a new join agent that waits for dispatched tasks.
    pub fn new(name: impl Into<String>, registry: TaskRegistry) -> Self {
        Self {
            name: name.into(),
            registry,
            target_names: None,
            timeout: None,
            middleware: MiddlewareChain::new(),
        }
    }

    /// Attach a middleware chain. `AgentEvent::AgentCompleted` is emitted
    /// through it for each task as it is joined successfully, and
    /// `AgentEvent::Timeout` when a task exceeds the join timeout.
    pub fn with_middleware_chain(mut self, chain: MiddlewareChain) -> Self {
        self.middleware = chain;
        self
    }

    /// Only wait for specific named tasks.
    pub fn targets(mut self, names: Vec<String>) -> Self {
        self.target_names = Some(names);
        self
    }

    /// Set a timeout for waiting.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[async_trait]
impl TextAgent for JoinTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut registry = self.registry.inner.lock().await;

        // Select tasks to wait for.
        let tasks: HashMap<String, _> = if let Some(targets) = &self.target_names {
            targets
                .iter()
                .filter_map(|name| registry.remove(name).map(|h| (name.clone(), h)))
                .collect()
        } else {
            std::mem::take(&mut *registry)
        };
        drop(registry);

        let mut results = Vec::new();

        for (task_name, handle) in tasks {
            let result = if let Some(timeout) = self.timeout {
                match tokio::time::timeout(timeout, handle).await {
                    Ok(Ok(Ok(text))) => {
                        let _ = state.set(format!("_result_{task_name}"), &text);
                        Ok(text)
                    }
                    Ok(Ok(Err(e))) => Err(AgentError::Other(e)),
                    Ok(Err(e)) => Err(AgentError::Other(format!("Join error: {e}"))),
                    Err(_) => {
                        let _ = self.middleware.run_on_event(&AgentEvent::Timeout).await;
                        Err(AgentError::Timeout)
                    }
                }
            } else {
                match handle.await {
                    Ok(Ok(text)) => {
                        let _ = state.set(format!("_result_{task_name}"), &text);
                        Ok(text)
                    }
                    Ok(Err(e)) => Err(AgentError::Other(e)),
                    Err(e) => Err(AgentError::Other(format!("Join error: {e}"))),
                }
            };

            results.push(result?);
        }

        let combined = results.join("\n");
        let _ = state.set("output", &combined);
        Ok(combined)
    }
}
