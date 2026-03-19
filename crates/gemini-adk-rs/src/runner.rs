//! Runner — orchestrates agent execution across Gemini Live sessions.
//!
//! Handles the full lifecycle: connect → run agent → handle transfer → reconnect → repeat.
//! Transfer is complex in Gemini Live because tools are fixed at setup — changing agents
//! means changing sessions (unlike traditional ADK where tools change per-call).

use std::sync::Arc;

use crate::agent::Agent;
use crate::agent_session::AgentSession;
use crate::context::InvocationContext;
use crate::error::AgentError;
use crate::middleware::MiddlewareChain;
use crate::plugin::{Plugin, PluginManager};
use crate::router::AgentRegistry;
use crate::state::State;

/// Orchestrates agent execution across Gemini Live sessions.
///
/// Handles the full lifecycle: connect → run → transfer → reconnect → repeat.
/// Transfer is complex in Gemini Live because tools are fixed at setup —
/// changing agents means changing sessions.
///
/// # Example
///
/// ```ignore
/// let runner = Runner::new(root_agent);
///
/// runner.run(|agent| async move {
///     let config = SessionConfig::new(&api_key)
///         .model(GeminiModel::GeminiLive2_5FlashNativeAudio);
///     // Add agent's tools to config
///     let session = connect(config, TransportConfig::default()).await?;
///     Ok(AgentSession::new(session))
/// }).await?;
/// ```
pub struct Runner {
    root_agent: Arc<dyn Agent>,
    registry: AgentRegistry,
    middleware: MiddlewareChain,
    plugins: PluginManager,
    state: State,
}

impl Runner {
    /// Create a new Runner with a root agent.
    ///
    /// Automatically registers the root agent and all sub-agents recursively.
    pub fn new(root_agent: impl Agent + 'static) -> Self {
        let agent = Arc::new(root_agent);
        let mut registry = AgentRegistry::new();
        Self::register_tree(&mut registry, agent.clone());
        Self {
            root_agent: agent,
            registry,
            middleware: MiddlewareChain::new(),
            plugins: PluginManager::new(),
            state: State::new(),
        }
    }

    /// Create a Runner from an already-Arc'd agent.
    pub fn from_arc(root_agent: Arc<dyn Agent>) -> Self {
        let mut registry = AgentRegistry::new();
        Self::register_tree(&mut registry, root_agent.clone());
        Self {
            root_agent,
            registry,
            middleware: MiddlewareChain::new(),
            plugins: PluginManager::new(),
            state: State::new(),
        }
    }

    /// Add middleware to the runner (applied to all agent invocations).
    pub fn with_middleware(mut self, mw: impl crate::middleware::Middleware + 'static) -> Self {
        self.middleware.add(Arc::new(mw));
        self
    }

    /// Add a plugin to the runner.
    pub fn with_plugin(mut self, plugin: impl Plugin + 'static) -> Self {
        self.plugins.add(Arc::new(plugin));
        self
    }

    /// Set initial state (available to all agents).
    pub fn with_state(mut self, state: State) -> Self {
        self.state = state;
        self
    }

    /// Manually register an additional agent (useful for cross-tree transfers).
    pub fn register(&mut self, agent: Arc<dyn Agent>) {
        self.registry.register(agent);
    }

    /// Access the agent registry.
    pub fn registry(&self) -> &AgentRegistry {
        &self.registry
    }

    /// Access the root agent.
    pub fn root_agent(&self) -> &dyn Agent {
        self.root_agent.as_ref()
    }

    /// Run the agent lifecycle. Handles transfers automatically.
    ///
    /// `connect_fn` is a factory that creates a new AgentSession for a given agent.
    /// This allows the Runner to reconnect with different configs on agent transfer
    /// (different tools/instructions → different Gemini Live session).
    ///
    /// The Runner will:
    /// 1. Call `connect_fn` with the current agent
    /// 2. Run `agent.run_live()` on the resulting session
    /// 3. If `TransferRequested` is returned, resolve the target agent,
    ///    disconnect, preserve state, and loop back to step 1
    /// 4. If the agent completes normally, return Ok(())
    pub async fn run<F, Fut>(&self, connect_fn: F) -> Result<(), AgentError>
    where
        F: Fn(Arc<dyn Agent>) -> Fut + Send + Sync,
        Fut: std::future::Future<Output = Result<AgentSession, AgentError>> + Send,
    {
        let mut current_agent = self.root_agent.clone();
        let runner_state = self.state.clone();

        // Telemetry
        crate::telemetry::logging::log_agent_started(
            current_agent.name(),
            0, // runner doesn't have tools
        );

        loop {
            // Connect with current agent's config
            let agent_session = connect_fn(current_agent.clone()).await?;

            // Merge runner state into session state
            agent_session.state().merge(&runner_state);

            // Create invocation context with runner's middleware
            let mut ctx =
                InvocationContext::with_middleware(agent_session.clone(), self.middleware.clone());

            // Run before_run plugins
            self.plugins.run_before_run(&ctx).await;

            // Run the agent
            match current_agent.run_live(&mut ctx).await {
                Ok(()) => {
                    // Run after_run plugins
                    self.plugins.run_after_run(&ctx).await;
                    // Agent completed normally — preserve state and return
                    runner_state.merge(agent_session.state());
                    break;
                }
                Err(AgentError::TransferRequested(target_name)) => {
                    // Resolve target agent
                    let target = self
                        .registry
                        .resolve(&target_name)
                        .ok_or_else(|| AgentError::UnknownAgent(target_name.clone()))?;

                    crate::telemetry::logging::log_agent_transfer(
                        current_agent.name(),
                        &target_name,
                    );
                    crate::telemetry::metrics::record_agent_transfer(
                        current_agent.name(),
                        &target_name,
                    );

                    // Preserve state across transfer
                    runner_state.merge(agent_session.state());

                    // Disconnect current session
                    let _ = agent_session.disconnect().await;

                    // Switch to target agent
                    current_agent = target;
                    continue;
                }
                Err(e) => {
                    // Other error — preserve state and propagate
                    runner_state.merge(agent_session.state());
                    let _ = agent_session.disconnect().await;
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Recursively register an agent and all its sub-agents.
    fn register_tree(registry: &mut AgentRegistry, agent: Arc<dyn Agent>) {
        registry.register(agent.clone());
        for sub in agent.sub_agents() {
            Self::register_tree(registry, sub);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;
    use async_trait::async_trait;
    use gemini_genai_rs::session::{SessionHandle, SessionPhase, SessionState};
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::{broadcast, mpsc, watch};

    // Mock agent that completes immediately
    struct NoopAgent {
        name: String,
    }

    #[async_trait]
    impl Agent for NoopAgent {
        fn name(&self) -> &str {
            &self.name
        }
        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            Ok(())
        }
    }

    // Mock agent that requests transfer
    struct TransferAgent {
        name: String,
        target: String,
    }

    #[async_trait]
    impl Agent for TransferAgent {
        fn name(&self) -> &str {
            &self.name
        }
        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            Err(AgentError::TransferRequested(self.target.clone()))
        }
        fn sub_agents(&self) -> Vec<Arc<dyn Agent>> {
            vec![]
        }
    }

    // Mock agent that reads state
    struct StateReaderAgent {
        name: String,
        key: String,
        expected: String,
    }

    #[async_trait]
    impl Agent for StateReaderAgent {
        fn name(&self) -> &str {
            &self.name
        }
        async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
            let val = ctx.state().get::<String>(&self.key);
            assert_eq!(val.as_deref(), Some(self.expected.as_str()));
            Ok(())
        }
    }

    // Mock agent that fails
    struct FailingAgent;

    #[async_trait]
    impl Agent for FailingAgent {
        fn name(&self) -> &str {
            "failing"
        }
        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            Err(AgentError::Other("boom".to_string()))
        }
    }

    fn mock_session_handle() -> SessionHandle {
        let (cmd_tx, _cmd_rx) = mpsc::channel(16);
        let (evt_tx, _) = broadcast::channel(16);
        let (phase_tx, phase_rx) = watch::channel(SessionPhase::Active);
        let state = Arc::new(SessionState::new(phase_tx));
        SessionHandle::new(cmd_tx, evt_tx, state, phase_rx)
    }

    fn mock_agent_session() -> AgentSession {
        AgentSession::new(mock_session_handle())
    }

    #[tokio::test]
    async fn runner_runs_single_agent() {
        let agent = NoopAgent {
            name: "root".to_string(),
        };
        let runner = Runner::new(agent);

        let result = runner
            .run(|_agent| async { Ok(mock_agent_session()) })
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn runner_handles_transfer() {
        // Root agent transfers to "target"
        let target = Arc::new(NoopAgent {
            name: "target".to_string(),
        });
        let root = TransferAgent {
            name: "root".to_string(),
            target: "target".to_string(),
        };

        let mut runner = Runner::new(root);
        // Register the target agent manually since TransferAgent doesn't declare sub_agents
        runner.register(target);

        let connect_count = Arc::new(AtomicU32::new(0));
        let count = connect_count.clone();

        let result = runner
            .run(move |_agent| {
                let c = count.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(mock_agent_session())
                }
            })
            .await;

        assert!(result.is_ok());
        // Should have connected twice: once for root, once for target
        assert_eq!(connect_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn runner_preserves_state_across_transfer() {
        // Agent A sets state, transfers to B, B reads state
        let agent_b = Arc::new(StateReaderAgent {
            name: "agent_b".to_string(),
            key: "greeting".to_string(),
            expected: "hello from A".to_string(),
        });

        // Agent A: sets state, then transfers
        struct SetAndTransferAgent;
        #[async_trait]
        impl Agent for SetAndTransferAgent {
            fn name(&self) -> &str {
                "agent_a"
            }
            async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
                ctx.state().set("greeting", "hello from A");
                Err(AgentError::TransferRequested("agent_b".to_string()))
            }
        }

        let mut runner = Runner::new(SetAndTransferAgent);
        runner.register(agent_b);

        let result = runner
            .run(|_agent| async { Ok(mock_agent_session()) })
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn runner_fails_on_unknown_transfer_target() {
        let root = TransferAgent {
            name: "root".to_string(),
            target: "nonexistent".to_string(),
        };

        let runner = Runner::new(root);

        let result = runner
            .run(|_agent| async { Ok(mock_agent_session()) })
            .await;

        match result {
            Err(AgentError::UnknownAgent(name)) => assert_eq!(name, "nonexistent"),
            other => panic!("expected UnknownAgent, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn runner_propagates_errors() {
        let runner = Runner::new(FailingAgent);

        let result = runner
            .run(|_agent| async { Ok(mock_agent_session()) })
            .await;

        match result {
            Err(AgentError::Other(msg)) => assert_eq!(msg, "boom"),
            other => panic!("expected Other error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn runner_with_initial_state() {
        struct StateCheckAgent;
        #[async_trait]
        impl Agent for StateCheckAgent {
            fn name(&self) -> &str {
                "checker"
            }
            async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
                let val = ctx.state().get::<String>("initial_key");
                assert_eq!(val.as_deref(), Some("initial_value"));
                Ok(())
            }
        }

        let initial_state = State::new();
        initial_state.set("initial_key", "initial_value");

        let runner = Runner::new(StateCheckAgent).with_state(initial_state);

        let result = runner
            .run(|_agent| async { Ok(mock_agent_session()) })
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn runner_auto_registers_sub_agents() {
        struct ParentAgent;
        #[async_trait]
        impl Agent for ParentAgent {
            fn name(&self) -> &str {
                "parent"
            }
            async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
                Ok(())
            }
            fn sub_agents(&self) -> Vec<Arc<dyn Agent>> {
                vec![
                    Arc::new(NoopAgent {
                        name: "child_a".to_string(),
                    }),
                    Arc::new(NoopAgent {
                        name: "child_b".to_string(),
                    }),
                ]
            }
        }

        let runner = Runner::new(ParentAgent);
        assert!(runner.registry().resolve("parent").is_some());
        assert!(runner.registry().resolve("child_a").is_some());
        assert!(runner.registry().resolve("child_b").is_some());
    }
}
