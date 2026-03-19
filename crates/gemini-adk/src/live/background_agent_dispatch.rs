//! BackgroundAgentDispatcher — fire-and-forget text agent dispatch from live callbacks.
//!
//! Spawns [`TextAgent`] pipelines on background tokio tasks. Results are written
//! to [`State`] under `{task_name}:result` / `{task_name}:error` keys, where
//! watchers can react to them.
//!
//! A semaphore budget prevents unbounded task explosion.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, Semaphore};

use crate::state::State;
use crate::text::TextAgent;

/// Dispatcher for running [`TextAgent`] pipelines as background tasks.
///
/// # Example
///
/// ```ignore
/// let dispatcher = BackgroundAgentDispatcher::new(5); // max 5 concurrent
///
/// // From an on_turn_complete callback:
/// dispatcher.dispatch("compliance_check", compliance_agent.clone(), state.clone());
///
/// // Results appear in state:
/// //   "compliance_check:result" = "No violations detected"
/// // OR
/// //   "compliance_check:error" = "Agent failed: ..."
/// ```
pub struct BackgroundAgentDispatcher {
    budget: Arc<Semaphore>,
    tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    max_concurrent: usize,
}

impl BackgroundAgentDispatcher {
    /// Create a new dispatcher with the given concurrency budget.
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            budget: Arc::new(Semaphore::new(max_concurrent)),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
        }
    }

    /// Maximum concurrent background agents.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Number of currently available permits.
    pub fn available_permits(&self) -> usize {
        self.budget.available_permits()
    }

    /// Dispatch a text agent to run in the background.
    ///
    /// Results are written to state under `{task_name}:result`.
    /// Errors are written to `{task_name}:error`.
    ///
    /// If the budget is exhausted, the task will wait for a permit.
    pub fn dispatch(&self, task_name: impl Into<String>, agent: Arc<dyn TextAgent>, state: State) {
        let name = task_name.into();
        let budget = self.budget.clone();
        let tasks = self.tasks.clone();
        let result_key = format!("{name}:result");
        let error_key = format!("{name}:error");
        let name_for_cleanup = name.clone();

        let handle = tokio::spawn(async move {
            // Acquire permit (waits if budget exhausted)
            let _permit = match budget.acquire().await {
                Ok(p) => p,
                Err(_) => return, // Semaphore closed
            };

            match agent.run(&state).await {
                Ok(result) => {
                    state.set(&result_key, &result);
                }
                Err(e) => {
                    state.set(&error_key, format!("{e}"));
                }
            }

            // Clean up task handle
            tasks.lock().await.remove(&name_for_cleanup);
        });

        // Store handle for cancellation. Use blocking try_lock to avoid
        // making dispatch async — callers are typically in sync contexts.
        // Fall back to fire-and-forget if lock is contended.
        if let Ok(mut guard) = self.tasks.try_lock() {
            guard.insert(name, handle);
        }
    }

    /// Check if a named task is still running.
    pub async fn is_running(&self, name: &str) -> bool {
        let guard = self.tasks.lock().await;
        guard.get(name).map(|h| !h.is_finished()).unwrap_or(false)
    }

    /// Cancel all running background agents.
    pub async fn cancel_all(&self) {
        let mut guard = self.tasks.lock().await;
        for (_, handle) in guard.drain() {
            handle.abort();
        }
    }

    /// Cancel a specific named task.
    pub async fn cancel(&self, name: &str) {
        let mut guard = self.tasks.lock().await;
        if let Some(handle) = guard.remove(name) {
            handle.abort();
        }
    }

    /// Number of tasks currently tracked (running or recently completed).
    pub async fn active_count(&self) -> usize {
        let guard = self.tasks.lock().await;
        guard.values().filter(|h| !h.is_finished()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;
    use async_trait::async_trait;

    struct QuickAgent {
        output: String,
    }

    #[async_trait]
    impl TextAgent for QuickAgent {
        fn name(&self) -> &str {
            "quick"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            Ok(self.output.clone())
        }
    }

    struct SlowAgent;

    #[async_trait]
    impl TextAgent for SlowAgent {
        fn name(&self) -> &str {
            "slow"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            Ok("done".into())
        }
    }

    struct FailAgent;

    #[async_trait]
    impl TextAgent for FailAgent {
        fn name(&self) -> &str {
            "fail"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            Err(AgentError::Other("background failure".into()))
        }
    }

    struct StateWriterAgent;

    #[async_trait]
    impl TextAgent for StateWriterAgent {
        fn name(&self) -> &str {
            "writer"
        }
        async fn run(&self, state: &State) -> Result<String, AgentError> {
            state.set("bg_wrote", true);
            Ok("wrote state".into())
        }
    }

    #[tokio::test]
    async fn dispatch_writes_result_to_state() {
        let dispatcher = BackgroundAgentDispatcher::new(5);
        let state = State::new();
        let agent: Arc<dyn TextAgent> = Arc::new(QuickAgent {
            output: "analysis complete".into(),
        });

        dispatcher.dispatch("analysis", agent, state.clone());

        // Wait for completion
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(
            state.get::<String>("analysis:result"),
            Some("analysis complete".into())
        );
    }

    #[tokio::test]
    async fn dispatch_writes_error_to_state() {
        let dispatcher = BackgroundAgentDispatcher::new(5);
        let state = State::new();
        let agent: Arc<dyn TextAgent> = Arc::new(FailAgent);

        dispatcher.dispatch("check", agent, state.clone());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let error = state.get::<String>("check:error");
        assert!(error.is_some());
        assert!(error.unwrap().contains("background failure"));
    }

    #[tokio::test]
    async fn budget_limits_concurrency() {
        let dispatcher = BackgroundAgentDispatcher::new(2);
        let state = State::new();
        let agent: Arc<dyn TextAgent> = Arc::new(SlowAgent);

        assert_eq!(dispatcher.available_permits(), 2);

        dispatcher.dispatch("task1", agent.clone(), state.clone());
        dispatcher.dispatch("task2", agent.clone(), state.clone());

        // Let tasks start
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Both permits should be taken
        assert_eq!(dispatcher.available_permits(), 0);

        // Wait for completion
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        assert_eq!(dispatcher.available_permits(), 2);
    }

    #[tokio::test]
    async fn cancel_all_aborts_tasks() {
        let dispatcher = BackgroundAgentDispatcher::new(5);
        let state = State::new();
        let agent: Arc<dyn TextAgent> = Arc::new(SlowAgent);

        dispatcher.dispatch("long", agent, state.clone());

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(dispatcher.is_running("long").await);

        dispatcher.cancel_all().await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Task was aborted, no result written
        assert!(state.get::<String>("long:result").is_none());
    }

    #[tokio::test]
    async fn state_mutations_visible_to_parent() {
        let dispatcher = BackgroundAgentDispatcher::new(5);
        let state = State::new();
        let agent: Arc<dyn TextAgent> = Arc::new(StateWriterAgent);

        dispatcher.dispatch("writer", agent, state.clone());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(state.get::<bool>("bg_wrote"), Some(true));
        assert_eq!(
            state.get::<String>("writer:result"),
            Some("wrote state".into())
        );
    }

    #[tokio::test]
    async fn cancel_specific_task() {
        let dispatcher = BackgroundAgentDispatcher::new(5);
        let state = State::new();
        let agent: Arc<dyn TextAgent> = Arc::new(SlowAgent);

        dispatcher.dispatch("keep", agent.clone(), state.clone());
        dispatcher.dispatch("abort", agent, state.clone());

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        dispatcher.cancel("abort").await;

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // "keep" should complete
        assert_eq!(state.get::<String>("keep:result"), Some("done".into()));
        // "abort" should not have result
        assert!(state.get::<String>("abort:result").is_none());
    }
}
