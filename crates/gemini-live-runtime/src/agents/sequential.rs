//! SequentialAgent — runs sub-agents one after another.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::Agent;
use crate::context::InvocationContext;
use crate::error::AgentError;

/// Runs sub-agents in sequential order.
///
/// Each sub-agent runs to completion before the next starts.
/// If any sub-agent returns an error (including `TransferRequested`), execution stops
/// and the error is propagated to the caller.
pub struct SequentialAgent {
    name: String,
    sub_agents: Vec<Arc<dyn Agent>>,
}

impl SequentialAgent {
    /// Create a new sequential agent with the given name and ordered sub-agents.
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self {
            name: name.into(),
            sub_agents,
        }
    }
}

#[async_trait]
impl Agent for SequentialAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
        for sub in &self.sub_agents {
            sub.run_live(ctx).await?;
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
    use crate::agent_session::AgentSession;
    use crate::context::InvocationContext;
    use crate::error::AgentError;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    use crate::agent_session::NoOpSessionWriter;

    /// Helper: create a test InvocationContext with a no-op session.
    fn test_ctx() -> InvocationContext {
        let (event_tx, _) = broadcast::channel(16);
        let writer: Arc<dyn gemini_live_wire::session::SessionWriter> =
            Arc::new(NoOpSessionWriter);
        let agent_session = AgentSession::from_writer(writer, event_tx);
        InvocationContext::new(agent_session)
    }

    /// A test agent that appends its name to a shared Vec when run.
    struct AppendAgent {
        agent_name: String,
        log: Arc<parking_lot::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Agent for AppendAgent {
        fn name(&self) -> &str {
            &self.agent_name
        }

        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            self.log.lock().push(self.agent_name.clone());
            Ok(())
        }
    }

    /// A test agent that fails with an error.
    struct FailAgent {
        agent_name: String,
        log: Arc<parking_lot::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Agent for FailAgent {
        fn name(&self) -> &str {
            &self.agent_name
        }

        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            self.log.lock().push(self.agent_name.clone());
            Err(AgentError::Other("fail".to_string()))
        }
    }

    /// A test agent that returns TransferRequested.
    struct TransferAgent {
        agent_name: String,
        target: String,
        log: Arc<parking_lot::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Agent for TransferAgent {
        fn name(&self) -> &str {
            &self.agent_name
        }

        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            self.log.lock().push(self.agent_name.clone());
            Err(AgentError::TransferRequested(self.target.clone()))
        }
    }

    #[tokio::test]
    async fn sequential_runs_all_in_order() {
        let log = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let agents: Vec<Arc<dyn Agent>> = vec![
            Arc::new(AppendAgent {
                agent_name: "a".into(),
                log: log.clone(),
            }),
            Arc::new(AppendAgent {
                agent_name: "b".into(),
                log: log.clone(),
            }),
            Arc::new(AppendAgent {
                agent_name: "c".into(),
                log: log.clone(),
            }),
        ];

        let seq = SequentialAgent::new("seq", agents);
        let mut ctx = test_ctx();
        seq.run_live(&mut ctx).await.unwrap();

        let entries = log.lock().clone();
        assert_eq!(entries, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn sequential_stops_on_error() {
        let log = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let agents: Vec<Arc<dyn Agent>> = vec![
            Arc::new(AppendAgent {
                agent_name: "a".into(),
                log: log.clone(),
            }),
            Arc::new(FailAgent {
                agent_name: "b".into(),
                log: log.clone(),
            }),
            Arc::new(AppendAgent {
                agent_name: "c".into(),
                log: log.clone(),
            }),
        ];

        let seq = SequentialAgent::new("seq", agents);
        let mut ctx = test_ctx();
        let result = seq.run_live(&mut ctx).await;

        assert!(result.is_err());
        let entries = log.lock().clone();
        assert_eq!(entries, vec!["a", "b"]); // c never ran
    }

    #[tokio::test]
    async fn sequential_propagates_transfer() {
        let log = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let agents: Vec<Arc<dyn Agent>> = vec![
            Arc::new(AppendAgent {
                agent_name: "a".into(),
                log: log.clone(),
            }),
            Arc::new(TransferAgent {
                agent_name: "b".into(),
                target: "target_agent".into(),
                log: log.clone(),
            }),
            Arc::new(AppendAgent {
                agent_name: "c".into(),
                log: log.clone(),
            }),
        ];

        let seq = SequentialAgent::new("seq", agents);
        let mut ctx = test_ctx();
        let result = seq.run_live(&mut ctx).await;

        match result {
            Err(AgentError::TransferRequested(target)) => {
                assert_eq!(target, "target_agent");
            }
            other => panic!("expected TransferRequested, got {:?}", other),
        }
        let entries = log.lock().clone();
        assert_eq!(entries, vec!["a", "b"]); // c never ran
    }

    #[tokio::test]
    async fn sequential_empty_succeeds() {
        let seq = SequentialAgent::new("empty", vec![]);
        let mut ctx = test_ctx();
        seq.run_live(&mut ctx).await.unwrap();
    }

    #[test]
    fn sequential_sub_agents_returns_children() {
        let log = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let agents: Vec<Arc<dyn Agent>> = vec![Arc::new(AppendAgent {
            agent_name: "child".into(),
            log,
        })];

        let seq = SequentialAgent::new("seq", agents);
        assert_eq!(seq.sub_agents().len(), 1);
        assert_eq!(seq.sub_agents()[0].name(), "child");
    }
}
