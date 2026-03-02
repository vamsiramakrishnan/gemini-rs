//! ParallelAgent — runs sub-agents concurrently.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::Agent;
use crate::context::InvocationContext;
use crate::error::AgentError;

/// Runs sub-agents concurrently.
///
/// All sub-agents run in parallel via `tokio::spawn`. Each gets a fresh
/// `InvocationContext` wrapping the same underlying `AgentSession` (shared state
/// and session). All must complete before `ParallelAgent` returns.
///
/// If any sub-agent fails, its error is returned (first error wins when
/// iterating over join handles in order).
pub struct ParallelAgent {
    name: String,
    sub_agents: Vec<Arc<dyn Agent>>,
}

impl ParallelAgent {
    /// Create a new parallel agent with the given name and sub-agents.
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self {
            name: name.into(),
            sub_agents,
        }
    }
}

#[async_trait]
impl Agent for ParallelAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
        let mut handles = Vec::new();

        for sub in &self.sub_agents {
            let sub = sub.clone();
            let agent_session = ctx.agent_session.clone();
            let event_tx = ctx.event_tx.clone();
            let middleware = ctx.middleware.clone();

            handles.push(tokio::spawn(async move {
                let mut branch_ctx = InvocationContext {
                    agent_session,
                    event_tx,
                    middleware,
                };
                sub.run_live(&mut branch_ctx).await
            }));
        }

        for handle in handles {
            handle
                .await
                .map_err(|e| AgentError::Other(format!("Join error: {}", e)))??;
        }

        Ok(())
    }

    fn sub_agents(&self) -> Vec<Arc<dyn Agent>> {
        self.sub_agents.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_session::{AgentSession, NoOpSessionWriter};
    use crate::context::InvocationContext;
    use crate::error::AgentError;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    /// Helper: create a test InvocationContext with a no-op session.
    fn test_ctx() -> InvocationContext {
        let (event_tx, _) = broadcast::channel(16);
        let writer: Arc<dyn rs_genai::session::SessionWriter> =
            Arc::new(NoOpSessionWriter);
        let agent_session = AgentSession::from_writer(writer, event_tx);
        InvocationContext::new(agent_session)
    }

    /// A test agent that sets a key in shared state.
    struct StateSetAgent {
        agent_name: String,
        key: String,
        value: String,
    }

    #[async_trait]
    impl Agent for StateSetAgent {
        fn name(&self) -> &str {
            &self.agent_name
        }

        async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
            ctx.state().set(&self.key, &self.value);
            Ok(())
        }
    }

    /// A test agent that always fails.
    struct FailAgent {
        agent_name: String,
    }

    #[async_trait]
    impl Agent for FailAgent {
        fn name(&self) -> &str {
            &self.agent_name
        }

        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            Err(AgentError::Other("parallel fail".to_string()))
        }
    }

    #[tokio::test]
    async fn parallel_runs_all() {
        let agents: Vec<Arc<dyn Agent>> = vec![
            Arc::new(StateSetAgent {
                agent_name: "a".into(),
                key: "key_a".into(),
                value: "val_a".into(),
            }),
            Arc::new(StateSetAgent {
                agent_name: "b".into(),
                key: "key_b".into(),
                value: "val_b".into(),
            }),
            Arc::new(StateSetAgent {
                agent_name: "c".into(),
                key: "key_c".into(),
                value: "val_c".into(),
            }),
        ];

        let par = ParallelAgent::new("par", agents);
        let mut ctx = test_ctx();
        par.run_live(&mut ctx).await.unwrap();

        // All three keys should be set via the shared AgentSession state.
        assert_eq!(
            ctx.state().get::<String>("key_a"),
            Some("val_a".to_string())
        );
        assert_eq!(
            ctx.state().get::<String>("key_b"),
            Some("val_b".to_string())
        );
        assert_eq!(
            ctx.state().get::<String>("key_c"),
            Some("val_c".to_string())
        );
    }

    #[tokio::test]
    async fn parallel_fails_if_any_fails() {
        let agents: Vec<Arc<dyn Agent>> = vec![
            Arc::new(StateSetAgent {
                agent_name: "a".into(),
                key: "key_a".into(),
                value: "val_a".into(),
            }),
            Arc::new(FailAgent {
                agent_name: "b".into(),
            }),
            Arc::new(StateSetAgent {
                agent_name: "c".into(),
                key: "key_c".into(),
                value: "val_c".into(),
            }),
        ];

        let par = ParallelAgent::new("par", agents);
        let mut ctx = test_ctx();
        let result = par.run_live(&mut ctx).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parallel_empty_succeeds() {
        let par = ParallelAgent::new("empty", vec![]);
        let mut ctx = test_ctx();
        par.run_live(&mut ctx).await.unwrap();
    }

    #[test]
    fn parallel_sub_agents_returns_children() {
        let agents: Vec<Arc<dyn Agent>> = vec![Arc::new(StateSetAgent {
            agent_name: "child".into(),
            key: "k".into(),
            value: "v".into(),
        })];

        let par = ParallelAgent::new("par", agents);
        assert_eq!(par.sub_agents().len(), 1);
        assert_eq!(par.sub_agents()[0].name(), "child");
    }
}
