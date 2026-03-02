//! LoopAgent — runs sub-agents repeatedly until max iterations or escalation.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::Agent;
use crate::context::InvocationContext;
use crate::error::AgentError;

/// Runs sub-agents repeatedly until `max_iterations` is reached or escalation.
///
/// Each iteration runs all sub-agents sequentially. To break out of the loop
/// early, a sub-agent can return `TransferRequested("__escalate")`. Other
/// transfer requests are propagated as-is, stopping the loop with an error.
pub struct LoopAgent {
    name: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    max_iterations: u32,
}

impl LoopAgent {
    /// Create a new loop agent with the given name, sub-agents, and maximum
    /// number of iterations.
    pub fn new(
        name: impl Into<String>,
        sub_agents: Vec<Arc<dyn Agent>>,
        max_iterations: u32,
    ) -> Self {
        Self {
            name: name.into(),
            sub_agents,
            max_iterations,
        }
    }
}

#[async_trait]
impl Agent for LoopAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
        for _iter in 0..self.max_iterations {
            for sub in &self.sub_agents {
                match sub.run_live(ctx).await {
                    Ok(()) => {}
                    Err(AgentError::TransferRequested(ref target)) if target == "__escalate" => {
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                }
            }
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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tokio::sync::broadcast;

    /// Helper: create a test InvocationContext with a no-op session.
    fn test_ctx() -> InvocationContext {
        let (event_tx, _) = broadcast::channel(16);
        let writer: Arc<dyn gemini_live_wire::session::SessionWriter> =
            Arc::new(NoOpSessionWriter);
        let agent_session = AgentSession::from_writer(writer, event_tx);
        InvocationContext::new(agent_session)
    }

    /// A test agent that increments a counter each time it runs.
    struct CounterAgent {
        agent_name: String,
        counter: Arc<AtomicU32>,
    }

    #[async_trait]
    impl Agent for CounterAgent {
        fn name(&self) -> &str {
            &self.agent_name
        }

        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// A test agent that escalates after a certain number of invocations.
    struct EscalateAfterAgent {
        agent_name: String,
        counter: Arc<AtomicU32>,
        escalate_at: u32,
    }

    #[async_trait]
    impl Agent for EscalateAfterAgent {
        fn name(&self) -> &str {
            &self.agent_name
        }

        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            let count = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
            if count >= self.escalate_at {
                Err(AgentError::TransferRequested("__escalate".to_string()))
            } else {
                Ok(())
            }
        }
    }

    /// A test agent that returns a non-escalate transfer request after N invocations.
    struct TransferAfterAgent {
        agent_name: String,
        counter: Arc<AtomicU32>,
        transfer_at: u32,
        target: String,
    }

    #[async_trait]
    impl Agent for TransferAfterAgent {
        fn name(&self) -> &str {
            &self.agent_name
        }

        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            let count = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
            if count >= self.transfer_at {
                Err(AgentError::TransferRequested(self.target.clone()))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn loop_runs_max_iterations() {
        let counter = Arc::new(AtomicU32::new(0));
        let agents: Vec<Arc<dyn Agent>> = vec![Arc::new(CounterAgent {
            agent_name: "counter".into(),
            counter: counter.clone(),
        })];

        let loop_agent = LoopAgent::new("loop", agents, 5);
        let mut ctx = test_ctx();
        loop_agent.run_live(&mut ctx).await.unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn loop_escalate_breaks_early() {
        let counter = Arc::new(AtomicU32::new(0));
        let agents: Vec<Arc<dyn Agent>> = vec![Arc::new(EscalateAfterAgent {
            agent_name: "escalator".into(),
            counter: counter.clone(),
            escalate_at: 3,
        })];

        let loop_agent = LoopAgent::new("loop", agents, 10);
        let mut ctx = test_ctx();
        // Should return Ok because __escalate is treated as a clean break.
        loop_agent.run_live(&mut ctx).await.unwrap();

        // Agent ran 3 times: iterations 1, 2, 3 (escalated on 3rd).
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn loop_propagates_non_escalate_transfer() {
        let counter = Arc::new(AtomicU32::new(0));
        let agents: Vec<Arc<dyn Agent>> = vec![Arc::new(TransferAfterAgent {
            agent_name: "transferer".into(),
            counter: counter.clone(),
            transfer_at: 2,
            target: "other_agent".into(),
        })];

        let loop_agent = LoopAgent::new("loop", agents, 10);
        let mut ctx = test_ctx();
        let result = loop_agent.run_live(&mut ctx).await;

        match result {
            Err(AgentError::TransferRequested(target)) => {
                assert_eq!(target, "other_agent");
            }
            other => panic!("expected TransferRequested, got {:?}", other),
        }

        // Agent ran twice: first time Ok, second time TransferRequested.
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn loop_zero_iterations() {
        let counter = Arc::new(AtomicU32::new(0));
        let agents: Vec<Arc<dyn Agent>> = vec![Arc::new(CounterAgent {
            agent_name: "counter".into(),
            counter: counter.clone(),
        })];

        let loop_agent = LoopAgent::new("loop", agents, 0);
        let mut ctx = test_ctx();
        loop_agent.run_live(&mut ctx).await.unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn loop_sub_agents_returns_children() {
        let counter = Arc::new(AtomicU32::new(0));
        let agents: Vec<Arc<dyn Agent>> = vec![Arc::new(CounterAgent {
            agent_name: "child".into(),
            counter,
        })];

        let loop_agent = LoopAgent::new("loop", agents, 5);
        assert_eq!(loop_agent.sub_agents().len(), 1);
        assert_eq!(loop_agent.sub_agents()[0].name(), "child");
    }
}
