//! Agent orchestration — invoke an agent in a [`Mode`].
//!
//! An agent is a value ([`TextAgent`] — a local agent, a composed pipeline, or a
//! remote A2A agent). Orchestration is the single question of *how* you invoke
//! it; the result always lands in governed `State` under `{name}:result` (or
//! `{name}:error`), so coordination is reactive and uniform regardless of the
//! invoker (the model, a `Flow`, an `Extract`, or a watcher).
//!
//! | Mode | Sync? | Lowers to |
//! |------|-------|-----------|
//! | [`Mode::Call`] | sync — caller awaits | [`call`] (agent-as-tool, awaited inline) |
//! | [`Mode::Dispatch`] | async, fire-and-forget | [`BackgroundAgentDispatcher::dispatch`](crate::live::BackgroundAgentDispatcher) |
//! | [`Mode::Background`] | async, model-aware | an agent-tool marked [`ToolExecutionMode::Background`](crate::live::ToolExecutionMode) |
//!
//! All three write `{name}:result`, so a `Flow` step can complete on a resolved
//! result via [`Guard::resolved`](crate::flow::Guard::resolved), and any
//! consumer reads the value the same way.

use std::sync::Arc;

use crate::error::AgentError;
use crate::state::State;
use crate::text::TextAgent;

/// How an agent is invoked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Synchronous — the caller awaits the result. Use only for *fast*
    /// dependencies (a voice session should not block on slow work).
    Call,
    /// Asynchronous, fire-and-forget — the conversation does not wait.
    Dispatch,
    /// Asynchronous, model-aware — runs detached; the result is delivered back
    /// to the model via `FunctionResponseScheduling`.
    Background,
}

/// State key an agent's successful result is written to.
pub fn result_key(name: &str) -> String {
    format!("{name}:result")
}

/// State key an agent's error is written to.
pub fn error_key(name: &str) -> String {
    format!("{name}:error")
}

/// Invoke `agent` **synchronously**: run it to completion, write its result to
/// `{name}:result` (or its error to `{name}:error`), and return the result.
///
/// This is the [`Mode::Call`] lowering. It uses the same `{name}:result`
/// convention as [`BackgroundAgentDispatcher::dispatch`](crate::live::BackgroundAgentDispatcher),
/// so sync and async invocations are observed identically.
pub async fn call(
    name: &str,
    agent: Arc<dyn TextAgent>,
    state: &State,
) -> Result<String, AgentError> {
    let result = agent.run(state).await;
    match &result {
        Ok(r) => state.set(result_key(name), r),
        Err(e) => state.set(error_key(name), e.to_string()),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct Echo(&'static str);
    #[async_trait]
    impl TextAgent for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            Ok(self.0.to_string())
        }
    }

    struct Boom;
    #[async_trait]
    impl TextAgent for Boom {
        fn name(&self) -> &str {
            "boom"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            Err(AgentError::Other("kaboom".into()))
        }
    }

    #[tokio::test]
    async fn call_writes_result_to_state() {
        let state = State::new();
        let out = call("verify", Arc::new(Echo("ok-123")), &state)
            .await
            .unwrap();
        assert_eq!(out, "ok-123");
        assert_eq!(
            state.get::<String>("verify:result").as_deref(),
            Some("ok-123")
        );
    }

    #[tokio::test]
    async fn call_writes_error_to_state() {
        let state = State::new();
        let r = call("verify", Arc::new(Boom), &state).await;
        assert!(r.is_err());
        assert!(state.contains("verify:error"));
        assert!(!state.contains("verify:result"));
    }
}
